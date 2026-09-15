// Refreshes the file/function inventory in the canonical architecture model.
// The hand-authored architecture, flows and findings remain authoritative;
// this script only derives source facts that would otherwise become stale.
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const directory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(directory, '../..');
const modelPath = join(directory, 'architecture.json');
const model = JSON.parse(await readFile(modelPath, 'utf8'));

const runGit = args => execFileSync('git', args, { cwd: repositoryRoot, encoding: 'utf8' }).trim();
const normalize = value => value.replaceAll('\\', '/');
const slug = value => value.replace(/[^a-zA-Z0-9]+/g, '-').replace(/^-|-$/g, '').toLowerCase();
const today = new Date().toISOString().slice(0, 10);

const sourceFiles = runGit(['ls-files', '--cached', '--others', '--exclude-standard'])
  .split(/\r?\n/)
  .map(normalize)
  .filter(Boolean)
  .filter(path => !path.startsWith('docs/') && !path.startsWith('dist/') && !path.includes('/target/'))
  .filter(path => !/\.(exe|png|icns|ico|lock)$/.test(path))
  .filter(path => /^(frontend\/.*\.(js|html|css)|src-tauri\/src\/.*\.rs|src-tauri\/(Cargo\.toml|tauri\.conf\.json|build\.rs)|tests\/.*\.js|\.github\/workflows\/.*\.yml|README\.md|build-windows\.ps1|run-.*\.(command|bat))$/.test(path));

const descriptions = {
  'frontend/app.js': ['Application controller', 'Coordinates screens, state, user actions and calls across the native boundary.'],
  'frontend/index.html': ['WebView shell', 'Declares the persistent application layout, panels, controls and dialogs.'],
  'frontend/styles.css': ['Application visual system', 'Defines layout, visual tokens and component states for the WebView.'],
  'frontend/graph-model.js': ['Commit graph model', 'Computes graph lanes and reference presentation without DOM or Git side effects.'],
  'frontend/console-context.js': ['Terminal context model', 'Builds explicit repository and folder scopes for terminal commands.'],
  'src-tauri/src/lib.rs': ['Tauri composition root', 'Registers the commands that JavaScript is allowed to invoke.'],
  'src-tauri/src/main.rs': ['Native entry point', 'Starts the desktop process and hands control to the Tauri composition root.'],
  'src-tauri/src/repository.rs': ['Repository backend', 'Implements repository reads, mutations, caching, submodules and external Git integrations.'],
  'src-tauri/build.rs': ['Build identity generator', 'Captures build metadata used to identify the packaged application.'],
  'src-tauri/Cargo.toml': ['Native dependency manifest', 'Defines the Rust package, Tauri features and native dependencies.'],
  'src-tauri/tauri.conf.json': ['Desktop packaging configuration', 'Defines the desktop bundle, window and frontend asset settings.'],
  'tests/graph-model.test.js': ['Graph model tests', 'Protects commit-DAG, lane and reference-selection behavior.'],
  'tests/console-context.test.js': ['Terminal context tests', 'Protects repository, folder and submodule command scopes.'],
  'README.md': ['Developer entry document', 'Explains setup, build, test and packaging workflows.'],
  'build-windows.ps1': ['Windows build workflow', 'Builds and validates the Windows executable.'],
  'run-macos.command': ['macOS launcher', 'Starts the packaged macOS application.'],
  'run-windows.bat': ['Windows launcher', 'Starts the packaged Windows application.']
};

function owningEntity(path, symbol = '', line = 0) {
  const pathCandidates = model.entities.filter(item => (item.source || []).some(source => {
    const sourcePath = normalize(source.path || '');
    return sourcePath === path || (sourcePath && !sourcePath.includes('.') && path.startsWith(`${sourcePath}/`));
  }));
  if (symbol) {
    const exact = pathCandidates.filter(item => (item.source || []).some(source => source.symbol === symbol || (source.symbols || []).includes(symbol)));
    if (exact.length) return exact.sort((a, b) => (b.level || 0) - (a.level || 0))[0].id;
    if (path === 'src-tauri/src/repository.rs') {
      const rules = [
        ['submodules', /submodule/],
        ['github-pr', /(?:^|_)(?:pr|github|gh)(?:_|$)|pull_request/],
        ['compare', /compare|resolve_commit/],
        ['stash', /stash/],
        ['branch-merge', /branch|merge|conflict/],
        ['working-tree', /stage|unstage|commit|working_tree|restore_file|remove_git_path/],
        ['history-graph', /graph|history|older_commits|tag_details|ref_seed/],
        ['remote-sync', /remote|fetch|pull|push|publish|sync_repository/],
        ['cache-lock-runtime', /cache|lock|invalidate|generation/],
        ['process-runner', /terminal|command|process|timeout|^git$/],
        ['diagnostics', /perf|anonym|build_info/],
        ['repository-loading', /load|open|status|directory|repository/]
      ];
      const inferred = rules.find(([, pattern]) => pattern.test(symbol));
      if (inferred) return inferred[0];
    }
    const containingRange = pathCandidates.filter(item => (item.source || []).some(source => source.path === path && source.startLine && source.endLine && line >= source.startLine && line <= source.endLine));
    if (containingRange.length) return containingRange.sort((a, b) => (b.level || 0) - (a.level || 0))[0].id;
  }
  return pathCandidates.sort((a, b) => (b.level || 0) - (a.level || 0))[0]?.id || (path.startsWith('tests/') ? 'tests' : 'system');
}

function layerForEntity(entityId) {
  let current = model.entities.find(item => item.id === entityId);
  while (current?.parentId && current.type !== 'layer') current = model.entities.find(item => item.id === current.parentId);
  if (current?.id === 'presentation') return 'presentation';
  if (current?.id === 'tauri-boundary') return 'tauri-boundary';
  if (current?.id === 'backend') return 'backend';
  return current?.id || 'system';
}

function findFunctions(path, text) {
  const lines = text.split(/\r?\n/);
  const language = extname(path) === '.rs' ? 'Rust' : 'JavaScript';
  const found = [];
  lines.forEach((line, index) => {
    let match;
    if (language === 'Rust') {
      match = line.match(/^\s*(pub(?:\([^)]*\))?\s+)?(async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/);
      if (!match) return;
      const prelude = lines.slice(Math.max(0, index - 4), index).join('\n');
      found.push({
        name: match[3], line: index + 1, language,
        async: Boolean(match[2]), public: Boolean(match[1]),
        tauriCommand: /#\[tauri::command\]/.test(prelude), isTest: /#\[(?:test|tokio::test)\]/.test(prelude),
        signature: line.trim()
      });
    } else {
      match = line.match(/^\s*(async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(([^)]*)\)/)
        || line.match(/^\s*const\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(async\s+)?\(([^)]*)\)\s*=>/);
      if (!match) return;
      const declaration = line.includes('function');
      found.push({
        name: declaration ? match[2] : match[1], line: index + 1, language,
        async: declaration ? Boolean(match[1]) : Boolean(match[2]), public: false,
        tauriCommand: false, isTest: path.startsWith('tests/'), signature: line.trim()
      });
    }
  });
  found.forEach((item, index) => {
    item.endLine = (found[index + 1]?.line || lines.length + 1) - 1;
    item.body = lines.slice(item.line - 1, item.endLine).join('\n');
  });
  return found;
}

const sourceText = new Map();
for (const path of sourceFiles) sourceText.set(path, await readFile(join(repositoryRoot, path), 'utf8'));

const rawFunctions = [];
for (const [path, text] of sourceText) {
  if (!/\.(js|rs)$/.test(path)) continue;
  for (const fn of findFunctions(path, text)) {
    const componentId = owningEntity(path, fn.name, fn.line);
    rawFunctions.push({ ...fn, path, componentId, layerId: layerForEntity(componentId) });
  }
}

const productionFunctions = rawFunctions.filter(fn => !fn.isTest);
const byName = new Map();
for (const fn of productionFunctions) {
  if (!byName.has(fn.name)) byName.set(fn.name, []);
  byName.get(fn.name).push(fn);
}

for (const fn of rawFunctions) {
  const callees = [];
  for (const [name, candidates] of byName) {
    if (name === fn.name || !new RegExp(`\\b${name.replaceAll('$', '\\$')}\\s*\\(`).test(fn.body)) continue;
    const sameFile = candidates.filter(candidate => candidate.path === fn.path);
    const target = sameFile.length === 1 ? sameFile[0] : candidates.length === 1 ? candidates[0] : null;
    if (target) callees.push(target);
  }
  fn.callees = [...new Set(callees.map(item => `${item.path}:${item.line}:${item.name}`))];
}

const callerCounts = new Map();
for (const fn of productionFunctions) for (const callee of fn.callees) callerCounts.set(callee, (callerCounts.get(callee) || 0) + 1);

function participatingFlows(fn) {
  return model.executionFlows.filter(flow => flow.steps.some(step => {
    const symbol = String(step.symbol || '').toLowerCase();
    const escaped = fn.name.toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    return new RegExp(`(^|[^a-z0-9_$])${escaped}([^a-z0-9_$]|$)`).test(symbol);
  })).map(flow => flow.id);
}

function sideEffects(fn) {
  const effects = [];
  if (/\binvoke\s*\(/.test(fn.body)) effects.push('Invokes the native Tauri backend');
  if (/\b(?:git|run_git_command|run_terminal_command_inner)\s*\(/.test(fn.body)) effects.push('May execute an external Git or shell process');
  if (/\bfs::|writeFile|remove_dir|create_dir/.test(fn.body)) effects.push('May read or modify the filesystem');
  if (/\.commit\s*\(|\.push\s*\(|\.write\s*\(|\.set_head/.test(fn.body)) effects.push('May mutate Git repository state');
  if (/innerHTML|textContent|classList|state\./.test(fn.body)) effects.push('Reads or updates interface state');
  if (fn.tauriCommand) effects.push('Crosses the JavaScript-to-Rust command boundary');
  return effects.length ? [...new Set(effects)] : ['No major side effect inferred by the static analyzer'];
}

for (const fn of rawFunctions) {
  const key = `${fn.path}:${fn.line}:${fn.name}`;
  fn.flowIds = participatingFlows(fn);
  fn.fanIn = callerCounts.get(key) || 0;
  fn.fanOut = fn.callees.length;
  fn.crossesBoundary = fn.tauriCommand || /\binvoke\s*\(/.test(fn.body);
  fn.score = 10
    + Math.min(fn.fanIn * 3, 30)
    + Math.min(fn.fanOut * 2, 24)
    + fn.flowIds.length * 10
    + (fn.crossesBoundary ? 14 : 0)
    + (fn.public ? 4 : 0)
    + (fn.async ? 3 : 0)
    + Math.min(Math.max(fn.endLine - fn.line, 0) / 20, 8)
    - (fn.isTest ? 40 : 0);
}

const ranked = [...productionFunctions].sort((a, b) => b.score - a.score || a.path.localeCompare(b.path) || a.line - b.line);
ranked.forEach((fn, index) => { fn.rank = index + 1; });

const functionRecords = rawFunctions.map(fn => {
  const reasons = [];
  if (fn.flowIds.length) reasons.push(`Participates in ${fn.flowIds.length} modeled user flow${fn.flowIds.length === 1 ? '' : 's'}`);
  if (fn.fanIn) reasons.push(`Called by approximately ${fn.fanIn} analyzed function${fn.fanIn === 1 ? '' : 's'}`);
  if (fn.fanOut) reasons.push(`Coordinates approximately ${fn.fanOut} analyzed call${fn.fanOut === 1 ? '' : 's'}`);
  if (fn.crossesBoundary) reasons.push('Crosses or coordinates an architectural boundary');
  if (fn.tauriCommand) reasons.push('Exposed as a Tauri command entry point');
  if (!reasons.length) reasons.push('Focused helper with limited architectural connectivity');
  return {
    id: `fn-${slug(fn.path)}-${fn.line}-${slug(fn.name)}`,
    type: 'function', name: fn.name, language: fn.language, fileId: `file-${slug(fn.path)}`,
    componentId: fn.componentId, layerId: fn.layerId,
    source: { path: fn.path, startLine: fn.line, endLine: fn.endLine, symbol: fn.name },
    signature: fn.signature, async: fn.async, public: fn.public, tauriCommand: fn.tauriCommand,
    test: fn.isTest, callers: fn.fanIn, callees: fn.callees, calleeCount: fn.fanOut,
    flowIds: fn.flowIds, sideEffects: sideEffects(fn), importanceScore: Number(fn.score.toFixed(2)),
    importanceRank: fn.rank || null, importanceReasons: reasons,
    responsibility: `Implements part of ${model.entities.find(item => item.id === fn.componentId)?.name || 'the application'}.`,
    inputs: fn.signature.includes('(') ? 'See the source signature; parameter types are preserved there.' : 'No explicit parameters detected.',
    output: fn.language === 'Rust' && fn.signature.includes('->') ? fn.signature.split('->').slice(1).join('->').trim().replace(/\s*\{$/, '') : fn.async ? 'Promise / asynchronous result' : 'See implementation and call sites.'
  };
});

const functionsByFile = new Map();
for (const fn of functionRecords) {
  if (!functionsByFile.has(fn.source.path)) functionsByFile.set(fn.source.path, []);
  functionsByFile.get(fn.source.path).push(fn);
}

function importance(path) {
  if (['frontend/app.js', 'src-tauri/src/repository.rs', 'src-tauri/src/lib.rs', 'frontend/graph-model.js'].includes(path)) return 'CORE';
  if (/^(frontend\/(index\.html|styles\.css|console-context\.js)|src-tauri\/(Cargo\.toml|tauri\.conf\.json|build\.rs)|tests\/)/.test(path)) return 'SUPPORTING';
  return 'UTILITY';
}

function fileOwner(path) {
  if (path === 'frontend/app.js' || path === 'frontend/console-context.js' || path === 'frontend/submodule-versions.js') return 'frontend-controller';
  if (path === 'frontend/index.html') return 'frontend-shell';
  if (path === 'frontend/styles.css') return 'presentation-style';
  if (path === 'frontend/graph-model.js') return 'graph-model';
  if (path === 'src-tauri/src/repository.rs') return 'repository-module';
  if (path === 'src-tauri/src/lib.rs' || path === 'src-tauri/src/main.rs') return 'tauri-boundary';
  if (path.startsWith('tests/')) return 'tests';
  if (path.startsWith('src-tauri/')) return 'delivery';
  return owningEntity(path);
}

function dependencies(path) {
  const deps = [];
  if (path === 'frontend/app.js') deps.push('frontend/graph-model.js', 'frontend/console-context.js', 'Tauri command API');
  if (path === 'frontend/index.html') deps.push('frontend/styles.css', 'frontend/graph-model.js', 'frontend/console-context.js', 'frontend/app.js');
  if (path === 'src-tauri/src/lib.rs') deps.push('src-tauri/src/repository.rs', 'Tauri runtime');
  if (path === 'src-tauri/src/repository.rs') deps.push('git2/libgit2', 'system Git', 'filesystem', 'Tauri runtime');
  if (path.startsWith('tests/graph-model')) deps.push('frontend/graph-model.js');
  if (path.startsWith('tests/console-context')) deps.push('frontend/console-context.js');
  return deps;
}

const fileRecords = sourceFiles.map(path => {
  const text = sourceText.get(path) || '';
  const ownerEntityId = fileOwner(path);
  const functions = (functionsByFile.get(path) || []).filter(fn => !fn.test).sort((a, b) => (a.importanceRank || 9999) - (b.importanceRank || 9999));
  const componentIds = [...new Set(functions.map(fn => fn.componentId))];
  const [role, description] = descriptions[path] || ['Supporting project file', 'Supports building, testing, launching or maintaining GitDrillDown.'];
  return {
    id: `file-${slug(path)}`, type: 'file', name: path.split('/').at(-1), path,
    layerId: layerForEntity(ownerEntityId), componentId: ownerEntityId, componentIds, importance: importance(path),
    role, description, why: `${role} keeps this responsibility traceable to one source location.`,
    lines: text ? text.split(/\r?\n/).length : null,
    dependsOn: dependencies(path),
    usedBy: model.relationships.filter(rel => rel.to === ownerEntityId).map(rel => model.entities.find(item => item.id === rel.from)?.name).filter(Boolean),
    importantFunctionIds: functions.slice(0, 12).map(fn => fn.id),
    functionCount: functions.length,
    externalDependencies: dependencies(path).filter(value => !value.includes('/')),
    flowIds: [...new Set(functions.flatMap(fn => fn.flowIds))],
    source: { path, startLine: 1, endLine: text ? text.split(/\r?\n/).length : undefined }
  };
});

model.schemaVersion = '2.0';
model.metadata.analyzedCommit = runGit(['rev-parse', 'HEAD']);
model.metadata.generatedAt = today;
model.metadata.viewerUpdatedAt = today;
model.metadata.workingTreeState = runGit(['status', '--short']) ? 'Source tree had uncommitted changes during analysis; the commit SHA identifies the stable baseline.' : 'Clean';
model.metadata.sourceFiles = fileRecords.length;
model.metadata.productionFunctions = ranked.length;
model.primaryLayers = [
  { entityId: 'presentation', label: 'GUI', purpose: 'Shows repository state and turns user intent into explicit actions.', input: 'Clicks, paths, branches and commit messages.', output: 'Tauri commands and rendered feedback.', why: 'Keeps interaction and presentation separate from native Git work.', color: 'gui' },
  { entityId: 'tauri-boundary', label: 'TAURI INTERFACE', purpose: 'Carries typed requests and results between JavaScript and Rust.', input: 'Named commands with serialized arguments.', output: 'Serialized results or explicit errors.', why: 'The WebView cannot safely perform native repository operations directly.', color: 'bridge' },
  { entityId: 'backend', label: 'BACKEND', purpose: 'Validates context and performs Git, filesystem and provider operations.', input: 'Repository-scoped command requests.', output: 'Repository data, mutations and process results.', why: 'Centralizes native access, safety rules, locks and caches.', color: 'backend' }
];
model.architectureStory = [
  'GitDrillDown is a desktop Git client whose interface runs as ordinary HTML, CSS and JavaScript inside a Tauri WebView.',
  'When the interface needs repository information or requests a change, it invokes a named Tauri command with the exact repository and path context.',
  'Rust validates that context, coordinates caches and write locks, then uses libgit2, the filesystem or a carefully bounded external process.',
  'The result crosses the same boundary back to JavaScript, where stale-request guards prevent an older response from replacing the screen for a newer context.'
];
model.files = fileRecords;
model.functions = functionRecords;
model.modelGuide.entityTypes = [...new Set([...(model.modelGuide.entityTypes || []), 'file', 'function'])];

await writeFile(modelPath, `${JSON.stringify(model, null, 2)}\n`, 'utf8');
console.log(`Updated architecture.json: ${fileRecords.length} files, ${ranked.length} ranked production functions, ${functionRecords.length} total functions.`);

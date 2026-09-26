// Every command that mutates the repository (index, HEAD, branches, stashes,
// submodules, remotes, files on disk...) is refused centrally, right here,
// while state.statusReady is false — right after opening a repository,
// before its background status fetch completes. This used to be a manual
// `if (!state.statusReady) ...` check duplicated at the top of every
// individual action's own function; several were missing it entirely
// (delete, folder-scoped commit, stash actions, some branch/submodule
// actions) simply because nothing forced every future one to remember it.
// One list here, checked once, covers every call site — present and future —
// without relying on each new mutating action to add its own copy.
const MUTATING_COMMANDS = new Set([
  'stage_files', 'unstage_files', 'stage_all', 'commit_files', 'commit_staged', 'commit_path',
  'remove_git_path', 'delete_local_path', 'switch_branch', 'create_branch', 'create_branch_at_commit', 'checkout_commit', 'rename_branch', 'delete_branch',
  'stash_changes', 'stash_file', 'pop_stash', 'drop_stash', 'restore_stash_paths', 'abort_stash_conflict',
  'restore_file', 'restore_remote_file', 'restore_folder', 'restore_exact_checkpoint', 'add_submodule', 'init_submodule', 'switch_submodule_version', 'reset_submodule', 'reset_submodule_branch_to_upstream', 'change_submodule_url',
  'commit_submodule', 'push_submodule', 'pull_submodule', 'force_push_submodule', 'fetch_submodule',
  'create_submodule_branch', 'merge_branch', 'open_merge_tool', 'resolve_conflict', 'complete_merge', 'abort_merge',
  'sync_repository', 'publish_branch', 'fetch_remote', 'fetch_all_remotes', 'fetch_project', 'write_text_file', 'run_git_command', 'run_terminal_command',
]);
// While an embedded Terminal command is running, every mutation *and* switching to
// a different repository (which would otherwise let that command's delayed
// result try to reload/affect a repository the user isn't even looking at
// anymore) are refused the same centralized way. The two console commands are
// excluded here deliberately — runTerminalFromConsole's own check is what
// actually prevents a second console command from starting, before this
// wrapper is ever reached; blocking it here too would just refuse the
// legitimate call that's about to set consoleCommandRunning in the first place.
const REPO_SWITCH_COMMANDS = new Set(['load_repository', 'open_repository_fast']);
const EMBEDDED_CONSOLE_COMMANDS = new Set(['run_git_command', 'run_terminal_command']);
const rawInvoke = window.__TAURI__?.core?.invoke;
const invoke = rawInvoke && ((command, args) => {
  if (MUTATING_COMMANDS.has(command) && !state.statusReady) {
    const message = 'Still loading status — please wait a moment.';
    status(message, 'error');
    jsPerfLog(`invoke wrapper REFUSED ${command} (statusReady=false)`, 0);
    return Promise.reject(message);
  }
  if (!EMBEDDED_CONSOLE_COMMANDS.has(command) && state.consoleCommandRunning && (MUTATING_COMMANDS.has(command) || REPO_SWITCH_COMMANDS.has(command))) {
    const message = 'A Terminal command is still running — please wait for it to finish.';
    status(message, 'error');
    jsPerfLog(`invoke wrapper REFUSED ${command} (consoleCommandRunning)`, 0);
    return Promise.reject(message);
  }
  // Same idea as state.statusReady above, scoped to whichever submodule is
  // currently being browsed: the first entry into a submodule shows its
  // filesystem-only listing immediately (see openDirectory) while its one
  // real status scan runs in the background — mutating anything before that
  // scan lands would act on a status this app hasn't actually confirmed yet.
  if (MUTATING_COMMANDS.has(command) && state.activeSubmodule && !state.activeSubmodule.statusReady) {
    const message = 'Still checking this submodule\'s status — please wait a moment.';
    status(message, 'error');
    jsPerfLog(`invoke wrapper REFUSED ${command} (activeSubmodule status not ready)`, 0);
    return Promise.reject(message);
  }
  return rawInvoke(command, args);
});
const $ = selector => document.querySelector(selector);
// Lane colors only — never branch identity, never commit state. Deliberately no
// red: that's reserved elsewhere in the app for danger/conflict/delete states,
// so a lane must never look like an error.
const palette = [
  '#58a6ff', '#39c5cf', '#48cc7e', '#f0b65a', '#b294ff', '#e17fd5',
  '#7ee787', '#d2a8ff', '#ffa657', '#79c0ff', '#a5d6ff', '#b4f1cf',
];
const directoryCache = new Map();
let submoduleMenuData = null;
let submoduleMenuEntry = null;
let versionFilter = 'branch';
const recentRepos = JSON.parse(localStorage.getItem('recentRepos') || '[]');
const SAVED_TERMINAL_COMMANDS_KEY = 'git-drilldown-saved-terminal-commands';
const SAVED_ACTIONS_KEY = 'git-drilldown-saved-actions';
function normalizeSavedAction(item) {
  if (!item || typeof item.name !== 'string') return null;
  const commands = Array.isArray(item.commands) ? item.commands : typeof item.command === 'string' ? [item.command] : [];
  const cleaned = commands.map(command => String(command).trim()).filter(Boolean);
  return cleaned.length ? { name: item.name.trim() || 'Untitled action', commands: cleaned } : null;
}
function loadSavedActions() {
  try {
    const parsed = JSON.parse(localStorage.getItem(SAVED_ACTIONS_KEY) || localStorage.getItem(SAVED_TERMINAL_COMMANDS_KEY) || '[]');
    return Array.isArray(parsed) ? parsed.map(normalizeSavedAction).filter(Boolean) : [];
  } catch { return []; }
}
function saveSavedActions() {
  localStorage.setItem(SAVED_ACTIONS_KEY, JSON.stringify(state.savedActions));
}

function addRecentRepo(path, name) {
  const existing = recentRepos.findIndex(r => r.path === path);
  if (existing >= 0) recentRepos.splice(existing, 1);
  recentRepos.unshift({ path, name, date: new Date().toISOString() });
  if (recentRepos.length > 10) recentRepos.pop();
  localStorage.setItem('recentRepos', JSON.stringify(recentRepos));
  renderRecentRepos();
}

const state = { repository: null, branches: [], commits: [], allCommits: [], changes: [], selectedCommit: null, view: 'explorer', currentPath: '', entries: [], selectedEntry: null, historyScope: '', historyKind: '', commanderPath: '', commanderRows: [], remoteRef: '', compareMode: 'local-drive', remotes: [], editingPath: '', editorOriginal: '', publish: null, changesScope: 'global', commanderFocus: '', comparingRow: null, hasStash: false, stashes: [], editingConflict: null, mergeTarget: null,
  // Set only while viewing a submodule's Submodule Map — holds *its own*
  // repository/branches/commits/changes/stashes/primaryBranch entirely
  // separately from the fields above, which always stay the parent
  // repository's. Explorer, Commander, Remotes and the breadcrumb read only
  // the fields above, never this — see activeGraphData(), openSubmoduleGraph
  // and leaveSubmoduleGraph.
  submoduleGraph: null,
  consoleMode: 'console', consoleTranscript: [], consoleCmdHistory: [], consoleDrafts: { commands: '', console: '', saved: '' }, consoleScopeOverride: null, graphPrimaryBranch: null, publishUpto: null, branchStartMarker: null, savedActions: loadSavedActions(), folderRestore: null,
  // False only right after openRepositoryFast, until its background
  // refresh_status completes — mutations (stage/unstage, delete, commit,
  // switching branch) are refused while this is false, since they'd act on
  // an index/status this app hasn't actually read yet. Every other load
  // path (loadRepository, used for Refresh and after any action) already
  // includes real status and leaves this true.
  // Non-null while state.currentPath is inside a submodule whose own status
  // hasn't been (or hasn't yet finished being) scanned this session:
  // { path: submodule's parent-relative path, statusReady: bool }. See
  // openDirectory's submodule branch and the invoke wrapper's gate above.
  // The repository's known submodule paths (from load_repository/
  // open_repository_fast's own index read, landed here by the same
  // Object.assign(state, data) every load already does — hence snake_case,
  // matching every other backend-sourced field name in this file) — purely
  // client-side lookup so recognizing "this navigation is inside a
  // submodule" costs nothing extra on ordinary (non-submodule) folder
  // clicks. See submoduleBoundaryFor.
  submodule_paths: [],
  drillDownNotes: {}, drillDownNotesRepositoryPath: '', drillDownNotesError: '', submoduleCompare: null, submoduleCompareAnchor: null, submoduleRevisionPicker: null,
  statusReady: true, consoleCommandRunning: false, activeSubmodule: null, localDriveGitRefreshPending: false };
const previewData = {
  repository: { name: 'vehicle-control', path: '/projects/vehicle-control', current_branch: 'feature/diagnostics' },
  branches: [
    { name: 'feature/diagnostics', current: true, remote: false },
    { name: 'main', current: false, remote: false },
    { name: 'release/2.4', current: false, remote: false },
    { name: 'origin/main', current: false, remote: true }
  ],
  commits: [
    { id: 'a39f21d', parents: ['9d2ac84'], subject: 'P:1842 Validate diagnostic event configuration', author: 'Andrei Pop', date: 'Today, 14:32', refs: ['HEAD', 'feature/diagnostics'], lane: 0 },
    { id: '9d2ac84', parents: ['eb81910'], subject: 'Add DEM configuration parser', author: 'Andrei Pop', date: 'Today, 11:08', refs: [], lane: 0 },
    { id: 'eb81910', parents: ['73da102', '24f5a88'], subject: 'Merge release/2.4 into main', author: 'Maria Ionescu', date: 'Yesterday', refs: ['main'], lane: 0 },
    { id: '24f5a88', parents: ['51bca30'], subject: 'Prepare release configuration', author: 'Victor Ene', date: 'Aug 12', refs: ['release/2.4'], lane: 1 },
    { id: '73da102', parents: ['51bca30'], subject: 'Refactor service dispatcher', author: 'Maria Ionescu', date: 'Aug 12', refs: [], lane: 0 },
    { id: '51bca30', parents: [], subject: 'Initial project structure', author: 'Maria Ionescu', date: 'Aug 09', refs: [], lane: 0 }
  ],
  changes: [
    { status: 'M', path: 'src/dem/configuration.c', staged: false },
    { status: 'A', path: 'tests/dem_configuration_test.c', staged: true }
  ],
  entries: [
    { name: 'src', relative_path: 'src', kind: 'folder', status: '•', tracked: true, size: 0, modified: 1786638900 },
    { name: 'diagnostics-core', relative_path: 'diagnostics-core', kind: 'submodule', status: '', tracked: true, size: 0, modified: 1786552500 },
    { name: 'tests', relative_path: 'tests', kind: 'folder', status: '•', tracked: true, size: 0, modified: 1786466100 },
    { name: 'Cargo.toml', relative_path: 'Cargo.toml', kind: 'file', status: 'M', tracked: true, size: 1834, modified: 1786638900 },
    { name: 'README.md', relative_path: 'README.md', kind: 'file', status: '', tracked: true, size: 5240, modified: 1786380000 },
    { name: 'vehicle-control.code-workspace', relative_path: 'vehicle-control.code-workspace', kind: 'file', status: '??', tracked: false, size: 386, modified: 1786638000 }
  ]
};
const refs = {
  repoName: $('#repoName'), repoPath: $('#repoPath'), branches: $('#branches'), graph: $('#graph'),
  graphView: $('#graphView'), emptyState: $('#emptyState'), laneLegend: $('#laneLegend'),
  details: $('#detailsPanel'), currentBranch: $('#currentBranch'), statusText: $('#statusText'),
  statusDot: $('#statusDot'), search: $('#search'), graphSubtitle: $('#graphSubtitle'),
  statusFooter: $('#statusFooter'), statusCommandHint: $('#statusCommandHint'), commandHistoryDialog: $('#commandHistoryDialog'), commandHistoryList: $('#commandHistoryList'),
  changeBadge: $('#changeBadge'), workspaceSubtitle: $('#workspaceSubtitle'), changes: $('#changes'),
  changesDrawer: $('#changesDrawer'), changesSummary: $('#changesSummary'), commitMessage: $('#commitMessage'),
  defaultCommitMessage: $('#defaultCommitMessage'), defaultCommitPolarionLink: $('#defaultCommitPolarionLink'),
  commitButton: $('#commitButton'), selectionText: $('#selectionText'), browserDialog: $('#browserDialog'),
  browserNotice: $('#browserNotice'), explorerView: $('#explorerView'), fileList: $('#fileList'),
  breadcrumbs: $('#breadcrumbs'), viewTitle: $('#viewTitle'), goUp: $('#goUp'), reloadFolder: $('#reloadFolder'), runCurrentUtrud: $('#runCurrentUtrud'),
  submoduleMenu: $('#submoduleMenu'), submoduleVersions: $('#submoduleVersions'), submoduleMenuName: $('#submoduleMenuName'), currentSubmoduleVersion: $('#currentSubmoduleVersion'), submoduleVersionSearch: $('#submoduleVersionSearch'), submoduleOpenGraph: $('#submoduleOpenGraph'),
  commitScope: $('#commitScope'), showPathHistory: $('#showPathHistory'), commitScopeDialog: $('#commitScopeDialog'), commitScopeName: $('#commitScopeName'), scopeCommitMessage: $('#scopeCommitMessage'), confirmScopeCommit: $('#confirmScopeCommit'),
  folderRestoreDialog: $('#folderRestoreDialog'), folderRestorePath: $('#folderRestorePath'), folderRestoreSubtitle: $('#folderRestoreSubtitle'), folderRestoreModeHead: $('#folderRestoreModeHead'), folderRestoreModeCommit: $('#folderRestoreModeCommit'), folderRestoreCommitPicker: $('#folderRestoreCommitPicker'), folderRestoreCommitList: $('#folderRestoreCommitList'), refreshFolderRestoreCommits: $('#refreshFolderRestoreCommits'), folderRestoreClean: $('#folderRestoreClean'), folderRestorePreview: $('#folderRestorePreview'), folderRestoreStatus: $('#folderRestoreStatus'), previewFolderRestore: $('#previewFolderRestore'), confirmFolderRestore: $('#confirmFolderRestore'),
  commanderView: $('#commanderView'), commanderRows: $('#commanderRows'), commanderBreadcrumbs: $('#commanderBreadcrumbs'), remoteRef: $('#remoteRef'), gitComparePanel: $('#gitComparePanel'), localDrivePanel: $('#localDrivePanel'), compareModeGit: $('#compareModeGit'), compareModeDrive: $('#compareModeDrive'), compareModeSubmodule: $('#compareModeSubmodule'), submoduleComparePanel: $('#submoduleComparePanel'), subCompareSubmodule: $('#subCompareSubmodule'), subCompareSubmoduleOptions: $('#subCompareSubmoduleOptions'), subCompareLeftRef: $('#subCompareLeftRef'), subCompareRightRef: $('#subCompareRightRef'), subComparePickLeft: $('#subComparePickLeft'), subComparePickRight: $('#subComparePickRight'), subCompareSwap: $('#subCompareSwap'), subCompareRefresh: $('#subCompareRefresh'), subCompareExact: $('#subCompareExact'), subCompareCommits: $('#subCompareCommits'), subCompareBreadcrumbs: $('#subCompareBreadcrumbs'), subCompareRows: $('#subCompareRows'), subRevisionDialog: $('#subCompareRevisionDialog'), subRevisionDialogSide: $('#subRevisionDialogSide'), subRevisionDialogTitle: $('#subRevisionDialogTitle'), subRevisionSearch: $('#subRevisionSearch'), subRevisionSearchAll: $('#subRevisionSearchAll'), subRevisionResults: $('#subRevisionResults'), subRevisionHelp: $('#subRevisionHelp'), compareDialog: $('#compareDialog'), compareTitle: $('#compareTitle'), compareSubtitle: $('#compareSubtitle'), localCompare: $('#localCompare'), remoteCompare: $('#remoteCompare'),
  remotesView: $('#remotesView'), remoteCards: $('#remoteCards'), editorDialog: $('#editorDialog'), editorTitle: $('#editorTitle'), editorPath: $('#editorPath'), editorContent: $('#editorContent'), locationRepository: $('#locationRepository'), locationBranch: $('#locationBranch'), locationPath: $('#locationPath'), leaveSubmoduleGraph: $('#leaveSubmoduleGraph'), publishDialog: $('#publishDialog'), publishBranch: $('#publishBranch'), publishRemote: $('#publishRemote'), publishCommits: $('#publishCommits'), publishSummary: $('#publishSummary'), publishDestination: $('#publishDestination'), publishBadge: $('#publishBadge'), publishSubtitle: $('#publishSubtitle'), cloneDialog: $('#cloneDialog'), cloneUrl: $('#cloneUrl'), cloneParent: $('#cloneParent'), cloneName: $('#cloneName'), cloneBranch: $('#cloneBranch'), cloneRecurseSubmodules: $('#cloneRecurseSubmodules'), confirmClone: $('#confirmClone'), submoduleDialog: $('#submoduleDialog'), submoduleUrl: $('#submoduleUrl'), submoduleParent: $('#submoduleParent'), submoduleName: $('#submoduleName'), submoduleUsername: $('#submoduleUsername'), submoduleToken: $('#submoduleToken'), submoduleAddStatus: $('#submoduleAddStatus'), confirmAddSubmodule: $('#confirmAddSubmodule'), operationToast: $('#operationToast'), drawerScopeTitle: $('#drawerScopeTitle'),
  mergeBranchDialog: $('#mergeBranchDialog'), mergeBranchSubtitle: $('#mergeBranchSubtitle'), mergeBranchCurrent: $('#mergeBranchCurrent'), mergeBranchSource: $('#mergeBranchSource'), mergeBranchStatus: $('#mergeBranchStatus'), confirmMergeBranch: $('#confirmMergeBranch'),
  stashesDialog: $('#stashesDialog'), stashesList: $('#stashesList'),
  togglePrStatus: $('#togglePrStatus'), prStatusArrow: $('#prStatusArrow'), prStatusPanel: $('#prStatusPanel'),
  toggleSubmodulePrStatus: $('#toggleSubmodulePrStatus'), submodulePrStatusArrow: $('#submodulePrStatusArrow'), submodulePrStatusPanel: $('#submodulePrStatusPanel'), submodulePrStatusLabel: $('#submodulePrStatusLabel'),
  newBranchDialog: $('#newBranchDialog'), newBranchFrom: $('#newBranchFrom'), newBranchOriginStatus: $('#newBranchOriginStatus'), newBranchName: $('#newBranchName'), newBranchStatus: $('#newBranchStatus'), confirmNewBranch: $('#confirmNewBranch'),
  conflictsDialog: $('#conflictsDialog'), conflictsTitle: $('#conflictsTitle'), conflictsSubtitle: $('#conflictsSubtitle'), conflictsList: $('#conflictsList'), conflictsCommitMessage: $('#conflictsCommitMessage'), conflictsCommitMessageLabel: $('#conflictsCommitMessageLabel'), conflictsLocalNote: $('#conflictsLocalNote'), conflictsStatus: $('#conflictsStatus'), confirmCompleteMerge: $('#confirmCompleteMerge'), abortMergeButton: $('#abortMerge'),
  mergeConflictsBanner: $('#mergeConflictsBanner'), mergeConflictsSubtitle: $('#mergeConflictsSubtitle')
};

function esc(value = '') { return String(value).replace(/[&<>'"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;',"'":'&#39;','"':'&quot;'}[c])); }
function commitSubjectHtml(subject = '') {
  // Commit messages carry Polarion work item IDs like "P:OMBMS-21610" (project
  // code, dash, number) — e.g. "P:OMBMS-21610 - 0ADOBD - DEM/FIM ...". The
  // Polarion URL needs the project code on its own (for the /project/ segment)
  // plus the full ID again for the workitem id. Project codes can start with
  // a digit too (e.g. "P:0ADOPM-1803"), so the first character must allow
  // digits, not just letters — a letter-only first character was silently
  // failing to recognize those as links.
  let cursor = 0; const parts = [];
  for (const match of subject.matchAll(/P:([A-Za-z0-9][A-Za-z0-9_]*-\d+)/g)) {
    parts.push(esc(subject.slice(cursor, match.index)));
    const workitemId = match[1]; const project = workitemId.split('-')[0];
    const url = `https://polarion.vitesco.io/polarion/#/project/${project}/workitem?id=${workitemId}`;
    parts.push(`<a class="polarion-link" href="${esc(url)}" target="_blank" rel="noreferrer" title="Open Polarion ${workitemId}">${esc(match[0])}</a>`);
    cursor = match.index + match[0].length;
  }
  parts.push(esc(subject.slice(cursor))); return parts.join('');
}
function status(message, kind = '') {
  refs.statusText.textContent = message; refs.statusDot.className = `status-dot ${kind}`;
  // 'busy' means something is still in flight — the command that will
  // eventually explain it hasn't been recorded yet, so there is nothing
  // new to show until whatever comes after (success/error/plain) calls
  // this again. Discreet by design: never awaited, never lets a failure
  // here surface as if it were the actual action's own error.
  if (kind !== 'busy') refreshCommandHint();
}
let toastTimer; function showOperationToast(message, kind = '') { clearTimeout(toastTimer); refs.operationToast.textContent = message; refs.operationToast.className = `operation-toast ${kind}`; refs.operationToast.hidden = false; toastTimer = setTimeout(() => { refs.operationToast.hidden = true; }, 7000); }

function normalizeNotePath(path = '') {
  return String(path).replace(/\\/g, '/').replace(/^\/+|\/+$/g, '').split('/').filter(part => part && part !== '.').join('/');
}
function noteForPath(path) {
  return state.drillDownNotes?.[normalizeNotePath(path)] || '';
}
function entrySupportsPersonalNote(entry) {
  return entry && ['folder', 'submodule'].includes(entry.kind);
}
function entryHasPersonalNote(entry) {
  return entrySupportsPersonalNote(entry) && Boolean(noteForPath(entry.relative_path));
}
async function ensureDrillDownNotesLoaded(repositoryPath, force = false) {
  if (!repositoryPath) return;
  if (!force && state.drillDownNotesRepositoryPath === repositoryPath) return;
  state.drillDownNotesRepositoryPath = repositoryPath;
  state.drillDownNotes = {};
  state.drillDownNotesError = '';
  if (!invoke) return;
  try {
    const loaded = await invoke('load_drill_down_notes', { repositoryPath });
    state.drillDownNotes = loaded?.notes || {};
    state.drillDownNotesError = loaded?.error || '';
    if (state.drillDownNotesError) status(state.drillDownNotesError, 'error');
  } catch (error) {
    state.drillDownNotes = {};
    state.drillDownNotesError = String(error);
    status(`Personal notes unavailable: ${String(error)}`, 'error');
  }
}
async function editEntryPersonalNote(entry) {
  if (!entrySupportsPersonalNote(entry)) return;
  const current = noteForPath(entry.relative_path);
  const text = await customPrompt(`Personal note for ${entry.relative_path}:`, current, { title: current ? 'Edit personal note' : 'Add personal note', okLabel: 'Save note', multiline: true });
  if (text === null || text === current) return;
  if (!invoke) {
    const key = normalizeNotePath(entry.relative_path);
    if (text.trim()) state.drillDownNotes[key] = text.trim(); else delete state.drillDownNotes[key];
    render(); renderEntryDetails({ ...entry });
    return;
  }
  try {
    const saved = await invoke('set_drill_down_note', { repositoryPath: state.repository.path, relativePath: entry.relative_path, note: text });
    state.drillDownNotes = saved?.notes || {};
    state.drillDownNotesError = saved?.error || '';
    render(); renderEntryDetails({ ...entry });
    status(text.trim() ? `${entry.name}: personal note saved` : `${entry.name}: personal note removed`);
  } catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}
async function deleteEntryPersonalNote(entry) {
  if (!entrySupportsPersonalNote(entry) || !noteForPath(entry.relative_path)) return;
  if (!await customConfirm(`Delete the personal note for "${entry.relative_path}"?`, { title: 'Delete personal note', danger: true, okLabel: 'Delete note' })) return;
  if (!invoke) {
    delete state.drillDownNotes[normalizeNotePath(entry.relative_path)];
    render(); renderEntryDetails({ ...entry });
    return;
  }
  try {
    const saved = await invoke('delete_drill_down_note', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    state.drillDownNotes = saved?.notes || {};
    state.drillDownNotesError = saved?.error || '';
    render(); renderEntryDetails({ ...entry });
    status(`${entry.name}: personal note deleted`);
  } catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}
function renderPersonalNoteSection(entry) {
  if (!entrySupportsPersonalNote(entry)) return '';
  const note = noteForPath(entry.relative_path);
  if (!note) {
    return `<div class="detail-section personal-note-section"><h3>PERSONAL NOTE</h3><button class="personal-note-add" data-note-action="edit">＋ Add note</button></div>`;
  }
  const preview = note.length > 140 ? `${note.slice(0, 140).trimEnd()}…` : note;
  return `<div class="detail-section personal-note-section"><h3>PERSONAL NOTE</h3><p>${esc(preview)}</p><div class="personal-note-actions"><button data-note-action="edit">Edit</button><button data-note-action="delete">Delete</button></div></div>`;
}

// Report: show the real git commands the app runs, quietly, next to the
// status text — not a replacement for the Terminal's own transcript (that
// already covers commands the user typed there directly), just a glanceable
// trace of what this app itself just did. Double-click the footer for the
// fuller, still-discreet history (openCommandHistoryDialog below).
async function refreshCommandHint() {
  if (!invoke) return;
  try {
    const [latest] = await invoke('recent_git_commands');
    if (!latest) { refs.statusCommandHint.hidden = true; return; }
    refs.statusCommandHint.textContent = `${latest.repo_hint}: ${latest.command}`;
    refs.statusCommandHint.classList.toggle('status-command-failed', !latest.success);
    refs.statusCommandHint.hidden = false;
  } catch { /* quiet by design — never disturb the action this rode along with */ }
}

function commandHistoryRowHtml(entry) {
  const ago = entry.seconds_ago < 60 ? `${Math.max(0, Math.round(entry.seconds_ago))}s ago`
    : entry.seconds_ago < 3600 ? `${Math.round(entry.seconds_ago / 60)}m ago`
    : `${Math.round(entry.seconds_ago / 3600)}h ago`;
  // A dedicated class, not a reuse of .publish-commit: that class assumes a
  // 4-column grid (index/checkbox, dot, 1fr content, badge) and a clickable
  // row (cursor:pointer, hover highlight) for toggling what gets pushed —
  // neither applies here, this list is a plain, unclickable 2-column read
  // history. .excluded's dim-to-de-prioritize styling is also the wrong
  // signal for "this command failed", which should stand out, not fade out.
  return `<div class="command-history-row ${entry.success ? '' : 'failed'}"><span>${entry.success ? '✓' : '✗'}</span><div><strong><code>${esc(entry.command)}</code></strong><small>${esc(entry.repo_hint)} · ${ago}</small></div></div>`;
}

async function openCommandHistoryDialog() {
  if (!invoke) { refs.commandHistoryList.innerHTML = '<div class="empty-change">No commands recorded yet this session.</div>'; refs.commandHistoryDialog.showModal(); return; }
  refs.commandHistoryList.innerHTML = '<div class="loading-row"><i class="spinner"></i>Loading…</div>';
  refs.commandHistoryDialog.showModal();
  try {
    const commands = await invoke('recent_git_commands');
    refs.commandHistoryList.innerHTML = commands.length ? commands.map(commandHistoryRowHtml).join('') : '<div class="empty-change">No commands recorded yet this session.</div>';
  } catch (error) { const message = handleError(error); showOperationToast(`Could not load command history: ${message}`, 'error'); }
}
refs.statusFooter.addEventListener('dblclick', openCommandHistoryDialog);

// Keep progress attached to the exact control the user pressed. The global
// status bar is useful context, but on a busy screen it is too easy to miss
// and the still-normal-looking button makes a slow Git operation look as if
// the click was ignored. This helper changes presentation only: it never
// starts, retries or cancels an operation, and restores the original control
// if the surrounding view was not replaced by the operation's refresh.
function beginButtonOperation(button, label) {
  if (!button) return () => {};
  const previous = { disabled: button.disabled, html: button.innerHTML, ariaBusy: button.getAttribute('aria-busy'), ariaLabel: button.getAttribute('aria-label'), title: button.title };
  const compact = button.textContent.trim().length <= 2;
  button.disabled = true;
  button.classList.add('action-running');
  button.classList.toggle('action-running-compact', compact);
  button.setAttribute('aria-busy', 'true');
  button.setAttribute('aria-label', label);
  button.title = label;
  button.innerHTML = `<i class="spinner" aria-hidden="true"></i>${compact ? '' : `<span>${esc(label)}</span>`}`;
  return () => {
    if (!button.isConnected) return;
    button.disabled = previous.disabled;
    button.classList.remove('action-running', 'action-running-compact');
    if (previous.ariaBusy == null) button.removeAttribute('aria-busy');
    else button.setAttribute('aria-busy', previous.ariaBusy);
    if (previous.ariaLabel == null) button.removeAttribute('aria-label');
    else button.setAttribute('aria-label', previous.ariaLabel);
    button.title = previous.title;
    button.innerHTML = previous.html;
  };
}

function localPathKey(path) {
  const value = String(path || '').replace(/\\/g, '/').replace(/\/+$/, '');
  return /^[A-Za-z]:\//.test(value) ? value.toLowerCase() : value;
}

function localPathIsInside(parent, candidate) {
  const parentKey = localPathKey(parent);
  const candidateKey = localPathKey(candidate);
  return Boolean(parentKey) && (candidateKey === parentKey || candidateKey.startsWith(`${parentKey}/`));
}

async function refreshGitAfterLocalDriveMutation(paths) {
  directoryCache.clear();
  const repositoryPath = state.repository?.path;
  if (!repositoryPath || !paths.some(path => localPathIsInside(repositoryPath, path))) return;
  // Do not launch an expensive repository scan while the dual-pane workspace
  // is being used: it made both panes flash and contend with navigation after
  // every copy/delete/new-folder command. Project Explorer consumes this flag
  // once, when the user returns to Git, and performs one consolidated refresh.
  state.localDriveGitRefreshPending = true;
}

const localDriveWorkspace = globalThis.LocalDriveWorkspace?.create({
  root: refs.localDrivePanel,
  invoke,
  notify: status,
  confirm: (message, options) => customConfirm(message, options),
  prompt: (message, defaultValue, options) => customPrompt(message, defaultValue, options),
  onMutation: paths => refreshGitAfterLocalDriveMutation(paths).catch(error => handleError(error)),
});

function handleError(error) {
  const msg = String(error).toLowerCase();
  if (msg.includes('timed out')) {
    // The backend only ever reports this after it has already force-killed
    // a genuinely stuck git process (10 minutes with zero progress) — by
    // this point nothing is still running, so it's safe to automatically
    // bring the repository view back to a clean, known state instead of
    // leaving it looking frozen. This should be rare: everything short of a
    // truly wedged process (a stalled network transfer, a held filesystem
    // lock) finishes on its own well before that backstop ever triggers.
    const friendly = 'That got stuck and was stopped automatically. Reloading the project…';
    status(friendly, 'error'); showOperationToast(friendly, 'error');
    if (state.repository) setTimeout(() => loadRepository(state.repository.path, { keepPath: true }), 300);
    return friendly;
  }
  if (msg.includes('no changes to commit')) { const friendly = 'Nothing to commit — this selection has no uncommitted local changes.'; status(friendly, 'error'); return friendly; }
  const adviceMap = {
    'no upstream': 'Go to Branch Map (Ctrl+Shift+G) → Right-click branch → Set upstream',
    'not a git': 'Open a valid Git repository with File > Open Repository',
    'merge conflict': 'Resolve conflicts manually in the files, then stage them',
    'diverged': 'To keep both histories, use “Merge branch…”. To throw away the local branch history, open “Change version” and use “Replace with remote…” on that branch.',
    'non-fast-forward': 'Use "Pull submodule" first. If it also refuses (diverged), resolve manually in a terminal: `git fetch`, `git merge origin/<branch>`, fix conflicts, commit, then push.',
    'authentication': 'Check your Git credentials and SSH keys',
    'permission denied': 'Check file permissions and access rights',
    'branch not found': 'Refresh (Ctrl+R) and verify the branch name',
    'remote not found': 'Add a remote in the Remotes view',
  };
  let advice = '';
  for (const [key, val] of Object.entries(adviceMap)) {
    if (msg.includes(key)) { advice = val; break; }
  }
  const fullMsg = advice ? `${error}\n💡 ${advice}` : String(error);
  status(fullMsg, 'error');
  return fullMsg;
}
function clearDetails(message) { refs.details.innerHTML = `<div class="details-empty"><div class="details-node"></div><strong>${esc(message)}</strong><span>Select an item in this view to see only relevant details and actions.</span></div>`; }

// window.confirm() / window.prompt() are unreliable inside Tauri's WKWebView (macOS) —
// they can silently no-op instead of showing anything, which looks like the app is
// broken. These custom dialogs use the same <dialog> element the rest of the app
// already relies on, so they are guaranteed to actually appear.
function customConfirm(message, options = {}) {
  return new Promise(resolve => {
    const dialog = $('#appConfirmDialog');
    $('#appConfirmTitle').textContent = options.title || 'Confirm';
    $('#appConfirmMessage').textContent = message;
    $('#appConfirmIcon').textContent = options.danger ? '!' : '?';
    const okButton = $('#appConfirmOk');
    okButton.textContent = options.okLabel || 'Confirm';
    okButton.className = options.danger ? 'danger' : 'confirm';
    const cleanup = (result) => { dialog.close(); okButton.removeEventListener('click', onOk); cancelButton.removeEventListener('click', onCancel); dialog.removeEventListener('cancel', onCancel); resolve(result); };
    const onOk = () => cleanup(true);
    const onCancel = () => cleanup(false);
    const cancelButton = $('#appConfirmCancel');
    okButton.addEventListener('click', onOk); cancelButton.addEventListener('click', onCancel); dialog.addEventListener('cancel', onCancel);
    dialog.showModal();
  });
}

function customPrompt(message, defaultValue = '', options = {}) {
  return new Promise(resolve => {
    const dialog = $('#appPromptDialog');
    $('#appPromptTitle').textContent = options.title || 'Enter value';
    $('#appPromptMessage').textContent = message;
    const singleInput = $('#appPromptInput');
    const textarea = $('#appPromptTextarea');
    singleInput.hidden = !!options.multiline;
    textarea.hidden = !options.multiline;
    const input = options.multiline ? textarea : singleInput;
    input.value = defaultValue;
    const okButton = $('#appPromptOk'); const cancelButton = $('#appPromptCancel');
    okButton.textContent = options.okLabel || 'OK';
    const cleanup = (result) => { dialog.close(); okButton.removeEventListener('click', onOk); cancelButton.removeEventListener('click', onCancel); input.removeEventListener('keydown', onKeydown); dialog.removeEventListener('cancel', onCancel); resolve(result); };
    const onOk = () => cleanup(input.value);
    const onCancel = () => cleanup(null);
    const onKeydown = (event) => { if (event.key === 'Enter' && (!options.multiline || event.metaKey || event.ctrlKey)) { event.preventDefault(); onOk(); } };
    okButton.addEventListener('click', onOk); cancelButton.addEventListener('click', onCancel); input.addEventListener('keydown', onKeydown); dialog.addEventListener('cancel', onCancel);
    dialog.showModal(); input.focus(); input.select();
  });
}

function openRepository() {
  if (!invoke) { refs.browserDialog.showModal(); return; }
  status('Choose a repository folder…', 'busy');
  jsPerfLog('choose_folder START', 0);
  const startedAt = performance.now();
  invoke('choose_folder')
    .then(path => { jsPerfLog(`choose_folder result (${(performance.now() - startedAt).toFixed(0)}ms): ${path || '(cancelled — no path chosen)'}`, 0); return path && openRepositoryFast(path); })
    .catch(error => { jsPerfLog(`choose_folder ERROR (${(performance.now() - startedAt).toFixed(0)}ms): ${String(error)}`, 0); handleError(error); });
}
function closeRepoPickerMenu() { $('#repoPickerMenu').hidden = true; }
function toggleRepoPickerMenu() { const menu = $('#repoPickerMenu'); menu.hidden = !menu.hidden; }
async function createRepositoryFromPicker() {
  if (!invoke) return refs.browserDialog.showModal();
  try { const path = await invoke('choose_folder'); if (path) { await invoke('init_repository', { path }); await openRepositoryFast(path); } } catch (error) { handleError(error); }
}

function validateCloneForm() { refs.confirmClone.disabled = !refs.cloneUrl.value.trim() || !refs.cloneParent.value.trim() || !refs.cloneName.value.trim(); }
function openCloneDialog() { refs.cloneUrl.value = ''; refs.cloneParent.value = ''; refs.cloneName.value = ''; refs.cloneBranch.value = ''; refs.cloneRecurseSubmodules.checked = false; refs.cloneName.dataset.edited = ''; validateCloneForm(); refs.cloneDialog.showModal(); refs.cloneUrl.focus(); }
async function chooseCloneParent() { if (!invoke) { refs.cloneParent.value = '/projects'; validateCloneForm(); return; } const path = await invoke('choose_folder'); if (path) { refs.cloneParent.value = path; validateCloneForm(); } }
async function confirmClone(event) {
  event.preventDefault(); const url = refs.cloneUrl.value.trim(); const parentPath = refs.cloneParent.value.trim(); const folderName = refs.cloneName.value.trim(); const branch = refs.cloneBranch.value.trim(); const recurseSubmodules = refs.cloneRecurseSubmodules.checked; if (!url || !parentPath || !folderName) return;
  if (!invoke) { refs.cloneDialog.close(); status(`Preview: cloned ${folderName}`); return; }
  refs.confirmClone.disabled = true; refs.confirmClone.textContent = 'Cloning…'; status(`Cloning ${folderName}…`, 'busy');
  try { const path = await invoke('clone_repository', { url, parentPath, folderName, branch: branch || null, recurseSubmodules }); refs.cloneDialog.close(); await openRepositoryFast(path); status(`Cloned and opened ${folderName}`); }
  catch (error) { status(String(error), 'error'); refs.confirmClone.disabled = false; }
  finally { refs.confirmClone.textContent = 'Clone repository'; }
}

function suggestedRepositoryName(url) { return url.trim().replace(/\/$/, '').split(/[/:]/).pop()?.replace(/\.git$/i, '') || ''; }
function validateSubmoduleForm() { refs.confirmAddSubmodule.disabled = !refs.submoduleUrl.value.trim() || !refs.submoduleName.value.trim(); if (refs.submoduleDialog.open && refs.submoduleName.value.trim()) { refs.submoduleAddStatus.textContent = `Will add at /${state.currentPath ? `${state.currentPath}/` : ''}${refs.submoduleName.value.trim()}`; refs.submoduleAddStatus.className = 'submodule-operation-status'; } }
function openAddSubmoduleDialog() {
  if (!state.repository || state.view !== 'explorer') return;
  const defaultSubmoduleUrl = '../../eng/';
  refs.submoduleUrl.value = defaultSubmoduleUrl; refs.submoduleName.value = ''; refs.submoduleUsername.value = ''; refs.submoduleToken.value = ''; refs.submoduleName.dataset.edited = '';
  refs.submoduleParent.value = state.currentPath ? `/${state.currentPath}` : '/ (repository root)';
  refs.submoduleAddStatus.textContent = `Destination: /${state.currentPath ? `${state.currentPath}/` : ''}…`; refs.submoduleAddStatus.className = 'submodule-operation-status';
  validateSubmoduleForm(); refs.submoduleDialog.showModal(); refs.submoduleUrl.focus(); refs.submoduleUrl.setSelectionRange(defaultSubmoduleUrl.length, defaultSubmoduleUrl.length);
}

function portableSubmoduleUrl(url) {
  const value = String(url || '').trim();
  const match = value.match(/^(?:https?:\/\/github\.vitesco\.io\/|ssh:\/\/(?:git@)?github\.vitesco\.io\/|git@github\.vitesco\.io:)([^/]+)\/([^/]+?)\/?$/i);
  if (!match) return value;
  return `../../${match[1]}/${match[2]}`;
}
async function confirmAddSubmodule() {
  const url = refs.submoduleUrl.value.trim(), folderName = refs.submoduleName.value.trim(), parentPath = state.currentPath, username = refs.submoduleUsername.value.trim(), accessToken = refs.submoduleToken.value;
  if (!url || !folderName || !state.repository) return;
  if (!invoke) { refs.submoduleDialog.close(); return status(`Preview: added ${folderName} in /${parentPath}`); }
  refs.confirmAddSubmodule.disabled = true; refs.confirmAddSubmodule.textContent = 'Adding…'; refs.submoduleAddStatus.textContent = `Cloning into /${parentPath ? `${parentPath}/` : ''}${folderName}…`; refs.submoduleAddStatus.className = 'submodule-operation-status busy'; status(`Cloning submodule ${folderName}…`, 'busy');
  try {
    const addedPath = await invoke('add_submodule', { repositoryPath: state.repository.path, parentPath, url, folderName, username, accessToken });
    refs.submoduleDialog.close(); directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: parentPath }); selectEntry(addedPath);
    state.changesScope = 'global'; refs.changesDrawer.classList.add('open');
    const message = `${addedPath} added. Commit .gitmodules + the submodule link, then Publish/Push that commit to the server.`; status(message); showOperationToast(message);
  } catch (error) { const message = String(error); refs.submoduleAddStatus.textContent = `Not added: ${message}`; refs.submoduleAddStatus.className = 'submodule-operation-status error'; status(message, 'error'); showOperationToast(`Submodule was not added: ${message}`, 'error'); refs.confirmAddSubmodule.disabled = false; }
  finally { refs.confirmAddSubmodule.textContent = 'Add submodule'; }
}

// Shared by loadRepository and openRepositoryFast — whichever of the two
// runs *most recently* wins. Without this, opening a repository (fast phase
// shown immediately) followed quickly by some action that itself calls the
// full, synchronous loadRepository (e.g. Refresh, or any mutation's own
// reload) could have the fast-open's background status fetch finish *after*
// that fuller load and stomp its already-complete, more accurate state with
// stale data.
let repoOpenGeneration = 0;

async function loadRepository(path, options = {}) {
  // Everything here, including the flush below, is inside this try —
  // flushPendingTogglesNow used to be called *before* this try started, so
  // its safety-net timeout (or any other rejection) would propagate as an
  // unhandled promise rejection straight out of loadRepository: no status
  // message, no log, the caller silently never reaching load_repository at
  // all — exactly the symptom reported ("Open Repository" doing nothing,
  // no open_repository_fast in the log, no visible error).
  try {
    // Must happen before anything else here reads/mutates state.repository —
    // this flushes (or discards, per flushPendingTogglesNow's own guards)
    // whatever's pending against the repository that's *currently* open,
    // before this call potentially switches to a different one or refreshes
    // this one out from under a still-in-flight checkbox click.
    await flushPendingTogglesNow({}, 'the Stage operation');
    const generation = ++repoOpenGeneration;
    status('Reading repository…', 'busy');
    const keepPath = options.keepPath ? state.currentPath : '';
    // Most callers already know exactly which folder they want open again after
    // the reload (the one the action just happened in) — without this, they'd
    // have to call openDirectory a second time themselves right after this
    // function's own internal one below, reloading the folder twice (once for
    // whatever keepPath resolves to, immediately discarded, then again for the
    // folder they actually wanted). Passing it here makes this the only load.
    const reopenPath = options.reopenPath !== undefined ? options.reopenPath : keepPath;
    // A plain refresh must not silently kick you out of whatever view you were
    // looking at (e.g. Branch Map) back to Project Explorer — that made a
    // just-refreshed history look unchanged when really you just weren't
    // looking at it anymore. Only reset the view for a genuine fresh open.
    const keepView = options.keepPath ? state.view : 'explorer';
    const data = await invoke('load_repository', { path, force: Boolean(options.force) });
    // A newer open/reload already started (and will do its own render) while
    // this one's backend call was in flight — applying this one now would
    // stomp whatever that newer one already showed.
    if (generation !== repoOpenGeneration) return false;
    // `data.commits` (assigned onto state.commits below) is always the full,
    // unscoped history — a scoped "History · <path>" view can't stay correctly
    // scoped through a refresh without re-querying that same scope, so it
    // falls back to the full Branch Map instead of showing stale-looking
    // scoped chrome over full data.
    directoryCache.clear(); Object.assign(state, data); state.allCommits = data.commits; state.historyScope = ''; state.historyKind = ''; state.view = keepView; state.commanderPath = options.keepPath ? state.commanderPath : ''; state.commanderRows = options.keepPath ? state.commanderRows : [];
    // load_repository always includes real, complete status — whatever
    // openRepositoryFast's still-pending background fetch was doing is moot now.
    state.statusReady = true;
    state.remoteRef = data.branches.find(branch => branch.remote)?.name || '';
    await ensureDrillDownNotesLoaded(data.repository.path);
    // Keep the parent's stash count current. The sidebar exposes Stash and
    // Stashes as separate actions even when this is zero; submodules have
    // their own independently loaded lists.
    state.hasStash = state.stashes.length > 0; updateStashUI();
    await openDirectory(reopenPath, { force: true }); status(`${data.commits.length} commits loaded`);
    addRecentRepo(path, data.repository.name);
    updatePublishIndicator();
    await checkForMergeConflicts();
    return true;
  } catch (error) { handleError(error); return false; }
}

async function refreshRepository(button = null) {
  if (!state.repository) return;
  const finishButton = beginButtonOperation(button, 'Refreshing…');
  try {
    status('Refreshing repository from disk…', 'busy');
    const refreshed = await loadRepository(state.repository.path, { keepPath: true, force: true });
    if (!refreshed) return;
    const msg = `Repository refreshed — ${state.commits.length} commits loaded.`;
    status(msg);
    showOperationToast(msg, 'success');
  } finally {
    finishButton();
  }
}

// The fast, read-only first phase for *opening a repository specifically* —
// see open_repository_fast's own doc comment for the full reasoning
// (skips the two genuinely expensive parts: the submodule reconciliation
// scan and the full status scan, measured 62x faster on a synthetic
// combined-stress case). Shows structure — folders, files, branches,
// commits — immediately; Stage/Delete/Commit/switch branch stay refused
// (state.statusReady = false) until the background completion below applies
// real status. Refresh and every action's own reload keep using the
// unchanged, fully synchronous loadRepository above, not this.
async function openRepositoryFast(path) {
  // Real "Open Repository" — always a genuine backend call, never served
  // from directoryCache (that cache only ever holds *folder listings* inside
  // an already-open repository, keyed by relative path within it; opening a
  // repository, this one included, has no cache-hit path at all). Logged
  // explicitly so a missing open_repository_fast line in the log is never
  // ambiguous with "it was served from somewhere else".
  jsPerfLog(`openRepositoryFast START (${path})`, 0);
  const openStarted = performance.now();
  // Everything, including the flush, is inside this try — see loadRepository's
  // identical comment for why: this call used to happen before any try
  // started here, so its safety-net timeout (or any other rejection) would
  // silently abort this function with no status message and no log entry —
  // exactly the reported symptom (choosing "Open Repository" again did
  // nothing, open_repository_fast never appeared in the log, no error shown).
  try {
    await flushPendingTogglesNow({}, 'the Stage operation');
    const generation = ++repoOpenGeneration;
    status('Opening repository…', 'busy');
    jsPerfLog(`invoke(open_repository_fast) START (${path})`, 0);
    const invokeStarted = performance.now();
    const data = await invoke('open_repository_fast', { path }).then(
      result => { jsPerfLog(`invoke(open_repository_fast) SUCCESS (${(performance.now() - invokeStarted).toFixed(0)}ms)`, 0); return result; },
      error => { jsPerfLog(`invoke(open_repository_fast) ERROR (${(performance.now() - invokeStarted).toFixed(0)}ms): ${String(error)}`, 0); throw error; },
    );
    if (generation !== repoOpenGeneration) { jsPerfLog('openRepositoryFast generation mismatch, discarding result', 0); return; }
    directoryCache.clear();
    closeSubmoduleGraph(); // a stale submodule context must never survive switching repositories
    Object.assign(state, data);
    state.changes = []; state.statusReady = false; state.activeSubmodule = null;
    state.graphPrimaryBranch = null; // a different repository's branches share nothing with the last one's picker choice
    state.allCommits = data.commits; state.historyScope = ''; state.historyKind = ''; state.view = 'explorer'; state.commanderPath = ''; state.commanderRows = [];
    state.remoteRef = data.branches.find(branch => branch.remote)?.name || '';
    await ensureDrillDownNotesLoaded(data.repository.path);
    state.hasStash = state.stashes.length > 0; updateStashUI();
    // Filesystem-only, no Git calls at all — see list_directory_fast's own
    // doc comment for exactly what this replaces: calling the *normal*
    // openDirectory('') here (root's scope is the empty string) was actually
    // triggering a full, unscoped status scan of its own, on top of the one
    // open_repository_fast was built to skip — a real perf log caught this
    // directly (2.67-4.58s). The root is now visible with zero Git work at all.
    await openDirectoryFast('');
    status(`${data.commits.length} commits loaded — checking status…`, 'busy');
    addRecentRepo(path, data.repository.name);
    updatePublishIndicator();
    await checkForMergeConflicts();
    completeRepositoryOpenStatus(state.repository.path, generation);
    jsPerfLog(`openRepositoryFast END (${(performance.now() - openStarted).toFixed(0)}ms)`, 0);
  } catch (error) { jsPerfLog(`openRepositoryFast ERROR (${(performance.now() - openStarted).toFixed(0)}ms): ${String(error)}`, 0); handleError(error); }
}

async function completeRepositoryOpenStatus(path, generation) {
  try {
    // The one and only full status scan in this whole flow.
    const changes = await invoke('refresh_status', { repositoryPath: path });
    // A newer open, or the real loadRepository, already ran (or is running)
    // since this fast-open started — its own state is what should stay on
    // screen, not this catching up a moment later and overwriting it.
    if (generation !== repoOpenGeneration || state.repository?.path !== path) return;
    state.changes = changes; state.statusReady = true;
    updateChangeBadge();
    // Reloads whatever folder is actually on screen with real status —
    // *not* a second full scan: refresh_status just populated the backend's
    // full-scan cache, and load_directory's own status lookup opportunistically
    // reuses a fresh full scan instead of running its own scoped one when
    // one's already sitting there (see worktree_status's own doc comment).
    await openDirectory(state.currentPath, { force: true });
    render();
    status(`${state.commits.length} commits loaded`);
  } catch (error) { handleError(error); }
}

function renderRecentRepos() {
  const list = $('#recentReposList');
  if (recentRepos.length === 0) { $('#recentReposBar').hidden = true; return; }
  $('#recentReposBar').hidden = false;
  list.innerHTML = recentRepos.map(repo => `<button class="recent-repo-btn" data-path="${esc(repo.path)}" title="${esc(repo.path)}">${esc(repo.name)}</button>`).join('');
  list.querySelectorAll('.recent-repo-btn').forEach(btn => btn.addEventListener('click', () => openRepositoryFast(btn.dataset.path)));
}

$('#closeRecentRepos').addEventListener('click', () => $('#recentReposBar').hidden = true);

// Drag & Drop - open files in editor
document.addEventListener('dragover', (e) => {
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  if (!e.dataTransfer?.files?.length) return;
  const files = Array.from(e.dataTransfer.files);
  files.forEach(file => {
    if (file.type.startsWith('text/') || file.name.endsWith('.md') || file.name.endsWith('.txt')) {
      const reader = new FileReader();
      reader.onload = (event) => {
        state.editingPath = file.name;
        state.editorOriginal = event.target.result;
        $('#editorTitle').textContent = `Edit ${file.name}`;
        $('#editorPath').textContent = file.name;
        $('#editorContent').value = event.target.result;
        $('#editorDialog').showModal();
      };
      reader.readAsText(file);
    }
  });
});

const updatePublishIndicatorGuard = createRequestGuard();
function publishAheadBehindText(info) {
  if (!info) return '';
  if (!info.remote_branch_exists) return 'new remote branch';
  const ahead = Number(info.ahead) || 0;
  const behind = Number(info.behind) || 0;
  if (ahead && behind) return `${ahead} ahead / ${behind} behind`;
  if (ahead) return `${ahead} ahead`;
  if (behind) return `${behind} behind`;
  return 'in sync';
}

async function updatePublishIndicator() {
  if (!invoke || !state.repository) return;
  const stillCurrent = updatePublishIndicatorGuard();
  try {
    state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path });
    if (!stillCurrent()) return;
    const remote = state.remotes[0]?.name, branch = state.repository.current_branch;
    if (!remote || !branch) { refs.publishBadge.textContent = '—'; refs.publishSubtitle.textContent = 'No remote or detached HEAD'; return; }
    const info = await invoke('publish_status', { repositoryPath: state.repository.path, branch, remote });
    if (!stillCurrent()) return;
    refs.publishBadge.textContent = info.commits.length;
    const comparison = publishAheadBehindText(info);
    refs.publishSubtitle.textContent = info.commits.length
      ? `${info.commits.length} commit${info.commits.length === 1 ? '' : 's'} to publish · ${comparison}`
      : `Everything is on the server · ${comparison}`;
  } catch (_) { if (stillCurrent()) { refs.publishBadge.textContent = '!'; refs.publishSubtitle.textContent = 'Cannot compare with server branch'; } }
}

// Never displays the literal string "HEAD" as if it were a branch name —
// see RepositoryInfo's own doc comment in repository.rs: current_branch is
// always "" when head_detached, specifically so callers can't accidentally
// do that. Shows the real commit instead, which is the only reliable
// "where am I" for a detached checkout (routine for a submodule right after
// `git submodule update` or "Reset submodule").
function describeBranch(repositoryInfo) {
  if (!repositoryInfo) return 'No branch';
  if (repositoryInfo.head_detached) return repositoryInfo.head_oid ? `Detached HEAD at ${repositoryInfo.head_oid.slice(0, 8)}` : 'Detached HEAD';
  return repositoryInfo.current_branch || 'No branch';
}

function render() {
  const loaded = Boolean(state.repository);
  document.body.classList.toggle('commander-mode', ['commander','remotes'].includes(state.view));
  if (loaded && !state.remoteRef) state.remoteRef = state.branches.find(branch => branch.remote)?.name || '';
  refs.emptyState.hidden = loaded; refs.explorerView.hidden = !loaded || state.view !== 'explorer'; refs.commanderView.hidden = !loaded || state.view !== 'commander'; refs.graphView.hidden = !loaded || state.view !== 'graph'; refs.remotesView.hidden = !loaded || state.view !== 'remotes';
  refs.repoName.textContent = loaded ? state.repository.name : 'Open a repository';
  refs.repoPath.textContent = loaded ? state.repository.path : 'Choose an existing Git folder';
  refs.currentBranch.textContent = loaded ? describeBranch(state.repository) : 'No branch';
  // "Submodule Reference Changes" (state.historyKind === 'submodule-refs')
  // gets its own explicit title, deliberately distinct from both "History
  // · <path>" (a plain folder/file's own path_history) and "Submodule
  // History · <name>" (the submodule's own Branch Map) — the whole point
  // of Message C's point 1 is that a user must never be unsure which of
  // these three they're looking at.
  refs.viewTitle.textContent = state.view === 'explorer' ? 'Project Explorer' : state.view === 'commander' ? 'Compare & Sync' : state.view === 'remotes' ? 'Remotes' : state.submoduleGraph ? `Submodule History · ${state.submoduleGraph.relativePath}` : state.historyKind === 'submodule-refs' ? `Submodule Reference Changes · ${state.historyScope}` : state.historyScope ? `History · ${state.historyScope}` : 'Repository History';
  refs.graphSubtitle.textContent = !loaded ? 'Navigate folders and inspect every item in your repository.' : state.view === 'explorer' ? `${state.entries.length} items in ${state.currentPath || state.repository.name}` : state.view === 'commander' ? (state.compareMode === 'local-drive' ? 'Two independent local folders. Copy safely without overwriting; delete through Trash/Recycle Bin.' : state.compareMode === 'submodule' ? 'Compare two exact revisions of the same submodule without checking anything out.' : 'Compare the workspace with a cached remote snapshot—no second checkout.') : state.view === 'remotes' ? 'Configured server locations and explicit fetch controls.' : state.historyKind === 'submodule-refs' ? `Parent-repository commits that changed this submodule's recorded version — not ${state.historyScope}'s own history` : state.historyScope ? `Commits touching ${state.historyScope}` : state.submoduleGraph ? 'Commits, branches and release tags for this submodule' : 'Commits, branches and release tags';
  refs.search.placeholder = state.view === 'explorer' ? 'Filter this folder' : state.view === 'commander' ? (state.compareMode === 'local-drive' ? 'Filter both local folders' : state.compareMode === 'submodule' ? 'Filter changed folders/files' : 'Filter comparison') : 'Find commit or author';
  refs.search.closest('label').hidden = state.view === 'remotes';
  const localDriveMode = state.view === 'commander' && state.compareMode === 'local-drive';
  if (!localDriveMode) localDriveWorkspace?.deactivate();
  refs.goUp.hidden = refs.reloadFolder.hidden = !['explorer','commander'].includes(state.view) || localDriveMode; refs.goUp.disabled = state.view === 'explorer' ? !state.currentPath : !state.commanderPath;
  refs.showPathHistory.hidden = state.view !== 'explorer' || !loaded;
  // Folder/file commit is now an item action in the right-side details
  // panel, where the selected scope is explicit. Keeping it in the header
  // made the toolbar crowded and too easy to confuse with a repository-wide
  // commit.
  refs.commitScope.hidden = true;
  // UTRUD is intentionally kept in the selected item details panel only.
  // In the header it competed with History/Commit/Add submodule and pushed
  // the toolbar outside the page on narrower windows.
  refs.runCurrentUtrud.hidden = true;
  $('#showFolderChanges').hidden = state.view !== 'explorer' || !loaded;
  $('#addSubmodule').hidden = state.view !== 'explorer' || !loaded;
  const folderChangeCount = loaded ? state.changes.filter(change => !state.currentPath || change.path === state.currentPath || change.path.startsWith(`${state.currentPath}/`)).length : 0;
  $('#showFolderChanges').textContent = `Folder changes · ${folderChangeCount}`;
  const activeScope = state.selectedEntry?.relative_path || state.currentPath;
  const activeName = state.selectedEntry?.name || (state.currentPath ? state.currentPath.split('/').pop() : state.repository?.name);
  refs.commitScope.textContent = state.selectedEntry ? `Commit ${activeName}` : state.currentPath ? 'Commit folder' : 'Commit repository';
  const scopeChangeCount = loaded ? state.changes.filter(change => !activeScope || change.path === activeScope || change.path.startsWith(`${activeScope}/`)).length : 0;
  refs.commitScope.disabled = !scopeChangeCount;
  refs.commitScope.title = scopeChangeCount ? '' : 'Nothing to commit here — no uncommitted local changes in this scope';
  $('#navExplorer').classList.toggle('active', state.view === 'explorer'); $('#navGraph').classList.toggle('active', state.view === 'graph');
  $('#navCommander').classList.toggle('active', state.view === 'commander');
  $('#navRemotes').classList.toggle('active', state.view === 'remotes');
  refs.locationRepository.textContent = state.submoduleGraph?.name || state.repository?.name || '—'; refs.locationBranch.textContent = describeBranch(state.submoduleGraph ? state.submoduleGraph.repository : state.repository); refs.locationPath.textContent = state.view === 'commander' ? (state.compareMode === 'local-drive' ? 'Local Drive' : state.compareMode === 'submodule' ? `Submodule compare /${state.commanderPath}` : `/${state.commanderPath}`) : state.view === 'explorer' ? `/${state.currentPath}` : state.view === 'graph' ? 'commit history' : 'remote configuration';
  refs.leaveSubmoduleGraph.hidden = !state.submoduleGraph;
  // Every view used to be rebuilt on every render() call regardless of which
  // one was actually visible — navigating folders in Explorer also rebuilt
  // the branch graph (up to 500 commits, plus a real layout pass reading
  // offsetTop/offsetHeight per row), the Commander comparison view, and the
  // Remotes view, none of which were even on screen. Only the view that's
  // actually active gets rebuilt now; switching to one calls render() again
  // right after anyway, which builds it fresh at that point.
  renderBranches();
  if (state.view === 'explorer') renderExplorer();
  if (state.view === 'commander') renderCommander();
  if (state.view === 'graph') renderGraph();
  if (state.view === 'remotes') renderRemotes();
  updateChangeBadge();
  updateStashUI();
  if (refs.changesDrawer.classList.contains('open')) renderChanges();
  // Independent of everything above: this only invalidates a displayed PR
  // result when its repository/branch changes. It never starts a network
  // request; Connect/Refresh inside the panel is the sole trigger.
  mainPrStatusPanel?.refreshIfContextChanged();
  renderSubmodulePrHeading();
  submodulePrStatusPanel?.refreshIfContextChanged();
}

function renderCommanderBreadcrumbs() {
  const target = state.compareMode === 'submodule' ? refs.subCompareBreadcrumbs : refs.commanderBreadcrumbs;
  const rootName = state.compareMode === 'submodule' ? (state.submoduleCompare?.name || 'Submodule') : (state.repository?.name || 'Repository');
  const parts = state.commanderPath ? state.commanderPath.split('/') : []; let accumulated = '';
  target.innerHTML = `<button class="crumb root" data-commander-path="">▰ ${esc(rootName)}</button>` + parts.map(part => {
    accumulated = accumulated ? `${accumulated}/${part}` : part; return `<span class="crumb-separator">›</span><button class="crumb" data-commander-path="${esc(accumulated)}">${esc(part)}</button>`;
  }).join('');
  target.querySelectorAll('[data-commander-path]').forEach(button => button.addEventListener('click', () => state.compareMode === 'submodule' ? openSubmoduleCompareDirectory(button.dataset.commanderPath) : openCommanderDirectory(button.dataset.commanderPath)));
}

function commanderSide(entry, side) {
  if (!entry) return `<span class="commander-side empty ${side}">— not present —</span>`;
  return `<span class="commander-side ${side}">${iconFor(entry)}<span class="commander-file-copy"><strong>${esc(entry.name)}</strong><small>${entry.kind}${entry.kind === 'file' ? ` · ${formatSize(entry.size)}` : ''}</small></span></span>`;
}

function renderCommander() {
  if (!state.repository) return;
  const localDrive = state.compareMode === 'local-drive';
  const submoduleMode = state.compareMode === 'submodule';
  const gitMode = !localDrive && !submoduleMode;
  refs.compareModeGit.classList.toggle('active', gitMode);
  refs.compareModeGit.setAttribute('aria-selected', String(gitMode));
  refs.compareModeDrive.classList.toggle('active', localDrive);
  refs.compareModeDrive.setAttribute('aria-selected', String(localDrive));
  refs.compareModeSubmodule.classList.toggle('active', submoduleMode);
  refs.compareModeSubmodule.setAttribute('aria-selected', String(submoduleMode));
  refs.gitComparePanel.hidden = !gitMode;
  refs.localDrivePanel.hidden = !localDrive;
  refs.submoduleComparePanel.hidden = !submoduleMode;
  if (localDrive) {
    localDriveWorkspace?.activate(state.repository.path);
    return;
  }
  localDriveWorkspace?.deactivate();
  if (submoduleMode) {
    renderSubmoduleCompare();
    return;
  }
  renderCommanderBreadcrumbs();
  const remoteBranches = state.branches.filter(branch => branch.remote);
  refs.remoteRef.innerHTML = remoteBranches.map(branch => `<option value="${esc(branch.name)}" ${branch.name === state.remoteRef ? 'selected' : ''}>${esc(branch.name)}</option>`).join('') || '<option value="">No remote refs</option>';
  const query = refs.search.value.trim().toLowerCase(); const rows = state.commanderRows.filter(row => !query || row.name.toLowerCase().includes(query));
  const upRow = state.commanderPath ? `<button class="commander-row commander-grid up-row" data-commander-up="1">
    <span class="commander-side local">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span><span class="compare-state"><i></i></span><span class="commander-side remote">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span>
  </button>` : '';
  refs.commanderRows.innerHTML = upRow + rows.map(row => `<button class="commander-row commander-grid ${row.relative_path === state.commanderFocus ? 'focused' : ''}" data-commander-entry="${esc(row.relative_path)}">
    ${commanderSide(row.local, 'local')}<span class="compare-state ${esc(row.status)}"><i></i>${esc(row.status.replace('-', ' '))}</span>${commanderSide(row.remote, 'remote')}</button>`).join('') || (state.commanderPath ? '' : '<div class="loading-row">No items to compare</div>');
  refs.commanderRows.querySelector('[data-commander-up]')?.addEventListener('click', () => { const parent = state.commanderPath.split('/').slice(0, -1).join('/'); openCommanderDirectory(parent); });
  refs.commanderRows.querySelectorAll('[data-commander-entry]').forEach(rowNode => {
    const row = state.commanderRows.find(item => item.relative_path === rowNode.dataset.commanderEntry);
    rowNode.addEventListener('dblclick', () => { const entry = row?.local || row?.remote; if (entry?.kind === 'folder') openCommanderDirectory(row.relative_path); });
    rowNode.addEventListener('click', () => { const localIsFile = !row?.local || row.local.kind === 'file'; const remoteIsFile = !row?.remote || row.remote.kind === 'file'; const eitherIsFile = row?.local?.kind === 'file' || row?.remote?.kind === 'file'; if (eitherIsFile && localIsFile && remoteIsFile) openFileCompare(row); });
  });
  const focused = refs.commanderRows.querySelector('.commander-row.focused'); if (focused) requestAnimationFrame(() => focused.scrollIntoView({ block: 'center' }));
}

function revisionDisplay(value = '') { return value ? value.slice(0, 8) : '—'; }
function compactRevisionValue(value = '') {
  const text = String(value || '').trim();
  return /^[0-9a-f]{16,40}$/i.test(text) ? text.slice(0, 12) : text;
}
function compareStatusText(status = '', mode = state.compareMode) {
  if (mode === 'submodule') {
    if (status === 'local-only') return 'left only';
    if (status === 'remote-only') return 'right only';
  }
  return String(status).replace('-', ' ');
}
function submoduleRevisionOptionLabel(item = {}) {
  const kind = item.kind === 'parent-current' ? 'parent + current' : item.kind === 'remote' ? 'remote branch' : item.kind || 'revision';
  const subject = item.subject ? ` · ${item.subject}` : '';
  const name = item.name && item.kind !== 'commit' ? `${item.name} · ` : '';
  return `${kind} · ${name}${revisionDisplay(item.revision)}${subject}`;
}
function submoduleRevisionOptionValue(item = {}) {
  return item.kind === 'commit' || item.kind === 'parent' || item.kind === 'current' || item.kind === 'parent-current'
    ? compactRevisionValue(item.revision || item.name || '')
    : item.name || compactRevisionValue(item.revision || '');
}
function submoduleRevisionOptionsFromVersions(versions = [], seenCommitRevisions = new Set()) {
  const seenKeys = new Set();
  const options = [];
  versions.forEach(item => {
    if (!item?.revision) return;
    if (item.kind === 'commit' && seenCommitRevisions.has(item.revision)) return;
    const value = submoduleRevisionOptionValue(item);
    const key = `${item.kind || 'revision'}|${value}|${item.revision}`;
    if (!value || seenKeys.has(key)) return;
    seenKeys.add(key);
    options.push({ value, label: submoduleRevisionOptionLabel(item), kind: item.kind || 'revision', revision: item.revision, subject: item.subject || '', name: item.name || '', author: item.author || '', date: item.date || '' });
  });
  return options;
}
function buildSubmoduleCompareOptions(data) {
  if (!data) return [];
  const options = [];
  const seenCommitRevisions = new Set();
  const parent = data.parent_revision || '';
  const current = data.current_revision || '';
  if (parent && current && parent === current) {
    options.push({ value: compactRevisionValue(parent), label: submoduleRevisionOptionLabel({ kind: 'parent-current', revision: parent }), kind: 'parent-current', revision: parent, subject: '' });
    seenCommitRevisions.add(parent);
  } else {
    if (parent) { options.push({ value: compactRevisionValue(parent), label: submoduleRevisionOptionLabel({ kind: 'parent', revision: parent }), kind: 'parent', revision: parent, subject: '' }); seenCommitRevisions.add(parent); }
    if (current) { options.push({ value: compactRevisionValue(current), label: submoduleRevisionOptionLabel({ kind: 'current', revision: current }), kind: 'current', revision: current, subject: '' }); seenCommitRevisions.add(current); }
  }
  return options.concat(submoduleRevisionOptionsFromVersions(data.versions || [], seenCommitRevisions));
}
function renderSubmoduleCompareOptions() {
  refs.subCompareSubmoduleOptions.innerHTML = submoduleCompareCandidates()
    .map(option => `<option value="${esc(option.path)}" label="${esc(option.label)}"></option>`)
    .join('');
}
function renderSubmoduleCompareCommitList(compare) {
  // The folder/file comparison is the primary task here. The commit-between
  // list made the pane visually noisy on real submodules; keep the data in
  // state for later use, but do not spend screen space on it here.
  refs.subCompareCommits.hidden = true;
  refs.subCompareCommits.innerHTML = '';
}
function renderSubmoduleCompare() {
  const compare = state.submoduleCompare;
  refs.subCompareSubmodule.value = compare?.submodulePath || '';
  refs.subCompareSubmodule.title = compare ? `${compare.name} · ${compare.submodulePath}` : 'Type a submodule name or path';
  refs.subCompareLeftRef.value = compare?.leftRef || '';
  refs.subCompareRightRef.value = compare?.rightRef || '';
  renderSubmoduleCompareOptions();
  renderCommanderBreadcrumbs();
  refs.subCompareExact.textContent = compare?.leftRevision && compare?.rightRevision
    ? `Comparing ${revisionDisplay(compare.leftRevision)} → ${revisionDisplay(compare.rightRevision)}. These are exact Git revisions; no checkout is performed.`
    : compare ? 'Choose left and right revisions, then Compare.' : 'Select a submodule and choose Compare submodule.';
  renderSubmoduleCompareCommitList(compare);
  const rowsSource = compare?.rows || [];
  const query = refs.search.value.trim().toLowerCase(); const rows = rowsSource.filter(row => !query || row.name.toLowerCase().includes(query));
  const upRow = state.commanderPath ? `<button class="commander-row commander-grid up-row" data-commander-up="1">
    <span class="commander-side local">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span><span class="compare-state"><i></i></span><span class="commander-side remote">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span>
  </button>` : '';
  refs.subCompareRows.innerHTML = upRow + rows.map(row => `<button class="commander-row commander-grid ${row.relative_path === state.commanderFocus ? 'focused' : ''}" data-sub-compare-entry="${esc(row.relative_path)}">
    ${commanderSide(row.local, 'local')}<span class="compare-state ${esc(row.status)}"><i></i>${esc(compareStatusText(row.status, 'submodule'))}</span>${commanderSide(row.remote, 'remote')}</button>`).join('') || (state.commanderPath ? '' : '<div class="loading-row">No differences in this folder</div>');
  refs.subCompareRows.querySelector('[data-commander-up]')?.addEventListener('click', () => { const parent = state.commanderPath.split('/').slice(0, -1).join('/'); openSubmoduleCompareDirectory(parent); });
  refs.subCompareRows.querySelectorAll('[data-sub-compare-entry]').forEach(rowNode => {
    const row = rowsSource.find(item => item.relative_path === rowNode.dataset.subCompareEntry);
    rowNode.addEventListener('dblclick', () => {
      const left = row?.local, right = row?.remote;
      if (left?.kind === 'folder' || right?.kind === 'folder') openSubmoduleCompareDirectory(row.relative_path);
      else if ((left?.kind === 'file' || right?.kind === 'file') && (!left || left.kind === 'file') && (!right || right.kind === 'file')) openSubmoduleRevisionFileCompare(row);
    });
    rowNode.addEventListener('click', () => {
      const left = row?.local, right = row?.remote;
      const eitherIsFile = left?.kind === 'file' || right?.kind === 'file';
      if (eitherIsFile && (!left || left.kind === 'file') && (!right || right.kind === 'file')) openSubmoduleRevisionFileCompare(row);
    });
  });
  const focused = refs.subCompareRows.querySelector('.commander-row.focused'); if (focused) requestAnimationFrame(() => focused.scrollIntoView({ block: 'center' }));
}

const openCommanderDirectoryGuard = createRequestGuard();
async function openCommanderDirectory(path) {
  if (!state.remoteRef) { status('No remote-tracking branch is available. Fetch the repository first.', 'error'); return; }
  state.commanderPath = path; refs.commanderRows.innerHTML = '<div class="loading-row"><i class="spinner"></i>Comparing local and remote…</div>';
  // Captured *after* commanderPath/view are updated, so the snapshot reflects
  // the folder this call is actually for.
  const stillCurrent = openCommanderDirectoryGuard();
  if (!invoke) { state.commanderRows = previewCommanderRows(); render(); return; }
  try {
    const result = await invoke('compare_remote_directory', { repositoryPath: state.repository.path, relativePath: path, remoteRef: state.remoteRef });
    if (!stillCurrent()) return;
    state.commanderRows = result.rows; render(); status(`Compared with ${state.remoteRef.slice(0, 40)}`);
  } catch (error) { if (stillCurrent()) { status(String(error), 'error'); refs.commanderRows.innerHTML = `<div class="loading-row">${esc(String(error))}</div>`; } }
}

const openSubmoduleCompareDirectoryGuard = createRequestGuard();
async function openSubmoduleCompareDirectory(path = state.commanderPath || '') {
  const compare = state.submoduleCompare;
  if (!compare?.submodulePath) { refs.subCompareRows.innerHTML = '<div class="loading-row">Select a submodule first.</div>'; return; }
  if (!compare.leftRef || !compare.rightRef) { refs.subCompareRows.innerHTML = '<div class="loading-row">Choose both revisions first.</div>'; return; }
  state.commanderPath = path;
  refs.subCompareRows.innerHTML = '<div class="loading-row"><i class="spinner"></i>Comparing submodule revisions…</div>';
  const stillCurrent = openSubmoduleCompareDirectoryGuard();
  if (!invoke) {
    compare.rows = previewCommanderRows();
    compare.leftRevision = compare.leftRef;
    compare.rightRevision = compare.rightRef;
    render();
    return;
  }
  try {
    const result = await invoke('compare_submodule_revisions_directory', {
      repositoryPath: state.repository.path,
      submodulePath: compare.submodulePath,
      relativePath: path,
      leftRef: compare.leftRef,
      rightRef: compare.rightRef,
    });
    if (!stillCurrent()) return;
    Object.assign(compare, {
      rows: result.rows || [],
      leftRevision: result.left_revision || result.leftRevision || '',
      rightRevision: result.right_revision || result.rightRevision || '',
      leftOnlyCommits: result.left_only_commits || result.leftOnlyCommits || [],
      rightOnlyCommits: result.right_only_commits || result.rightOnlyCommits || [],
    });
    state.commanderPath = result.relative_path || path;
    render();
    status(`Compared ${compare.name}: ${revisionDisplay(compare.leftRevision)} → ${revisionDisplay(compare.rightRevision)}`);
  } catch (error) {
    if (stillCurrent()) { status(String(error), 'error'); refs.subCompareRows.innerHTML = `<div class="loading-row">${esc(String(error))}</div>`; }
  }
}

async function openSubmoduleCompareFromEntry(entry, overrides = {}) {
  if (!entry || entry.kind !== 'submodule') return;
  const { innerPath = '', ...revisionOverrides } = overrides;
  state.compareMode = 'submodule';
  state.view = 'commander';
  refs.search.value = '';
  state.commanderPath = normalizeRepositoryRelativePath(innerPath);
  const existing = state.submoduleCompare?.submodulePath === entry.relative_path ? state.submoduleCompare : null;
  state.submoduleCompare = existing || {
    name: entry.name,
    submodulePath: entry.relative_path,
    leftRef: '',
    rightRef: '',
    leftRevision: '',
    rightRevision: '',
    revisionOptions: [],
    rows: [],
    leftOnlyCommits: [],
    rightOnlyCommits: [],
  };
  Object.assign(state.submoduleCompare, { name: entry.name, submodulePath: entry.relative_path }, revisionOverrides);
  render();
  if (!invoke) {
    const preview = {
      path: entry.relative_path,
      current_revision: revisionOverrides.rightRef || 'current-preview',
      current_branch: 'main',
      parent_revision: revisionOverrides.leftRef || 'parent-preview',
      versions: previewData.branches.map(branch => ({ name: branch.name, revision: branch.name === 'main' ? 'current-preview' : 'other-preview', kind: branch.remote ? 'remote' : 'branch', subject: 'Preview revision' })),
    };
    state.submoduleCompare.revisionOptions = buildSubmoduleCompareOptions(preview);
    state.submoduleCompare.leftRef ||= preview.parent_revision;
    state.submoduleCompare.rightRef ||= preview.current_revision;
    await openSubmoduleCompareDirectory(state.commanderPath);
    return;
  }
  try {
    status(`Loading revisions for ${entry.name}…`, 'busy');
    const data = await invoke('submodule_versions', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    if (!state.submoduleCompare || state.submoduleCompare.submodulePath !== entry.relative_path) return;
    state.submoduleCompare.revisionOptions = buildSubmoduleCompareOptions(data);
    state.submoduleCompare.leftRef = revisionOverrides.leftRef || state.submoduleCompare.leftRef || data.parent_revision || data.current_revision || 'HEAD';
    state.submoduleCompare.rightRef = revisionOverrides.rightRef || state.submoduleCompare.rightRef || data.current_revision || data.parent_revision || 'HEAD';
    render();
    await openSubmoduleCompareDirectory(state.commanderPath);
  } catch (error) { handleError(error); }
}

async function openSubmoduleCompareFromGraphCommit(commitId, rightRef = '') {
  if (!state.submoduleGraph) return;
  const entry = { kind: 'submodule', name: state.submoduleGraph.name, relative_path: state.submoduleGraph.relativePath };
  await openSubmoduleCompareFromEntry(entry, { leftRef: commitId, rightRef: rightRef || state.submoduleGraph.repository.head_oid || 'HEAD' });
}

async function openSubmoduleRevisionFileCompare(row) {
  const compare = state.submoduleCompare;
  if (!compare) return;
  state.comparingRow = row;
  const stillCurrent = openFileCompareGuard();
  const leftMissing = !row.local; const rightMissing = !row.remote;
  refs.compareTitle.textContent = row.name;
  refs.compareSubtitle.textContent = `${compare.name}: ${revisionDisplay(compare.leftRevision || compare.leftRef)} compared with ${revisionDisplay(compare.rightRevision || compare.rightRef)} · read-only`;
  refs.localCompare.textContent = refs.remoteCompare.textContent = 'Loading…';
  setCompareReadOnly(true, 'Read-only compare between two Git revisions. No file, index or checkout is changed.');
  refs.compareDialog.showModal();
  if (!invoke) { renderComparisonContents(leftMissing ? '' : 'left preview\n', rightMissing ? '' : 'right preview\n'); return; }
  try {
    const comparison = await invoke('compare_submodule_revision_file', {
      repositoryPath: state.repository.path,
      submodulePath: compare.submodulePath,
      relativePath: row.relative_path,
      leftRef: compare.leftRevision || compare.leftRef,
      rightRef: compare.rightRevision || compare.rightRef,
    });
    if (!stillCurrent() || state.comparingRow !== row) return;
    renderComparisonContents(comparison.local_content || (leftMissing ? '(file does not exist on left revision)' : ''), comparison.remote_content || (rightMissing ? '(file does not exist on right revision)' : ''));
  } catch (error) { if (stillCurrent() && state.comparingRow === row) { refs.localCompare.textContent = String(error); refs.remoteCompare.textContent = ''; } }
}

function previewCommanderRows() {
  return previewData.entries.map((entry, index) => ({ name: entry.name, relative_path: entry.relative_path, local: index === 2 ? null : entry, remote: index === 5 ? null : { ...entry, size: index === 3 ? 1720 : entry.size }, status: index === 5 ? 'local-only' : index === 3 ? 'modified' : index === 2 ? 'remote-only' : 'same' }));
}

const openFileCompareGuard = createRequestGuard();
async function openFileCompare(row) {
  state.comparingRow = row;
  const stillCurrent = openFileCompareGuard();
  const localMissing = !row.local; const remoteMissing = !row.remote;
  refs.compareTitle.textContent = row.name;
  refs.compareSubtitle.textContent = localMissing ? `Only exists on ${state.remoteRef} — not fetched locally yet` : remoteMissing ? `Only exists locally — not on ${state.remoteRef}` : `Local workspace compared with ${state.remoteRef}`;
  refs.localCompare.textContent = refs.remoteCompare.textContent = 'Loading…';
  setCompareReadOnly(false);
  setCompareActionStatus('Ready — choose one action for this file.');
  // Stage/Unstage/Discard need a local file; Restore-from-remote needs a remote file.
  $('#compareStage').disabled = localMissing; $('#compareUnstage').disabled = localMissing; $('#compareRestoreHead').disabled = localMissing; $('#compareRestoreRemote').disabled = remoteMissing;
  refs.compareDialog.showModal();
  if (!invoke) { renderComparisonContents('version = "0.2.0"\nfeatures = ["local"]', 'version = "0.1.0"\nfeatures = []'); return; }
  try {
    const comparison = await invoke('compare_file_contents', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef });
    if (!stillCurrent() || state.comparingRow !== row) return; // a newer file's compare (same guard) or the dialog moved on
    renderComparisonContents(comparison.local_content || (localMissing ? '(file does not exist locally)' : ''), comparison.remote_content);
  } catch (error) { if (stillCurrent() && state.comparingRow === row) { refs.localCompare.textContent = String(error); refs.remoteCompare.textContent = ''; } }
}

function setCompareActionStatus(message, kind = '') { const node = $('#compareActionStatus'); node.textContent = message; node.className = `compare-status-line ${kind}`.trim(); }
function setCompareHeadLabels(left = 'LOCAL', right = 'REMOTE') {
  const labels = document.querySelectorAll('.compare-head span');
  if (labels[0]) labels[0].textContent = left;
  if (labels[1]) labels[1].textContent = right;
}
function setCompareReadOnly(readOnly, message = '') {
  document.querySelector('.compare-actionbar').hidden = !!readOnly;
  $('#recoveryHelp').hidden = !!readOnly;
  setCompareHeadLabels(readOnly ? 'LEFT REVISION' : 'LOCAL', readOnly ? 'RIGHT REVISION' : 'REMOTE');
  if (readOnly) setCompareActionStatus(message || 'Read-only comparison.', 'busy');
}
function setCompareActionsDisabled(disabled) {
  if (disabled) { ['#compareRestoreRemote','#compareRestoreHead','#compareStage','#compareUnstage'].forEach(selector => { $(selector).disabled = true; }); return; }
  // Re-enabling must respect which side of the comparison actually exists.
  const row = state.comparingRow; const localMissing = !row?.local; const remoteMissing = !row?.remote;
  $('#compareStage').disabled = localMissing; $('#compareUnstage').disabled = localMissing; $('#compareRestoreHead').disabled = localMissing; $('#compareRestoreRemote').disabled = remoteMissing;
}

async function applyFileRecovery(forcedAction = '') {
  const action = forcedAction, row = state.comparingRow;
  if (!row) { setCompareActionStatus('No file is selected for comparison.', 'error'); return; }
  if (!action) { setCompareActionStatus('No action was received. Close Compare and open the file again.', 'error'); return; }
  const descriptions = { remote: `fetch ${state.remoteRef} and replace the working file with that server snapshot — may differ from your last local commit; staging is not changed`, head: 'discard working-file edits and restore your last local commit (HEAD) — does not fetch anything from the server', stage: 'add the current working-file content to the staging area', unstage: 'remove only the staging-area entry while keeping working-file edits' };
  if (['remote','head'].includes(action) && !await customConfirm(`This will ${descriptions[action]} for ${row.relative_path}. Continue?`, { title: action === 'head' ? 'Restore from last commit (HEAD)' : 'Restore from server', danger: true, okLabel: 'Continue' })) return;
  if (!invoke) { setCompareActionStatus(`Preview complete: ${descriptions[action]}`, 'success'); return; }
  try {
    setCompareActionsDisabled(true); setCompareActionStatus(action === 'remote' ? `Contacting server and fetching ${state.remoteRef}…` : `Applying ${action} to ${row.name}…`, 'busy');
    status(action === 'remote' ? `Fetching ${state.remoteRef} and restoring ${row.name}…` : `Applying action to ${row.name}…`, 'busy');
    if (action === 'remote') {
      await invoke('restore_remote_file', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef });
      const comparison = await invoke('compare_file_contents', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef }); renderComparisonContents(comparison.local_content, comparison.remote_content); refs.compareSubtitle.textContent = `LOCAL NOW MATCHES ${state.remoteRef} · fetched from server`; directoryCache.clear();
      const msg = `${row.name}: the file on disk was overwritten with the ${state.remoteRef} version. Yes — the project was updated.`;
      setCompareActionStatus(`Success: ${row.name} now contains the fetched server version.`, 'success'); status(msg); showOperationToast(msg, 'success'); return;
    }
    if (action === 'head') await invoke('restore_file', { repositoryPath: state.repository.path, relativePath: row.relative_path, sourceRef: 'HEAD' }); else await invoke(action === 'stage' ? 'stage_files' : 'unstage_files', { path: state.repository.path, files: [row.relative_path] });
    const folder = state.commanderPath; refs.compareDialog.close(); await loadRepository(state.repository.path); state.view = 'commander'; state.commanderPath = folder; state.commanderFocus = row.relative_path; await openCommanderDirectory(folder);
    const doneMsg = action === 'head' ? `${row.name}: reverted to HEAD. The project on disk was updated.` : action === 'stage' ? `${row.name}: staged.` : `${row.name}: unstaged, edits kept on disk.`;
    status(doneMsg); showOperationToast(doneMsg, 'success');
  }
  catch (error) { const message = String(error); setCompareActionStatus(`Failed: ${message}`, 'error'); status(message, 'error'); showOperationToast(`Failed: ${message}`, 'error'); }
  finally { setCompareActionsDisabled(false); }
}
function updateRecoveryHelp(action = '') { const help = { stage: '`git add` – Stage the current file content', unstage: 'UNSTAGE (`git restore --staged`) – unstages only, your edits on disk stay exactly as they are', head: 'RESTORE FROM LAST COMMIT (`git checkout HEAD -- file`) – no network access; discards edits using what you already have locally', remote: `RESTORE FROM SERVER – fetches ${state.remoteRef || 'remote'} first, then overwrites the file with that server version (can differ from your last local commit if the server has newer changes)` }; if (action) { $('#recoveryHelp').textContent = help[action]; } else { const allOptions = `Stage: ${help.stage} • Unstage: ${help.unstage} • Restore from last commit: ${help.head} • Restore from server: ${help.remote}`; $('#recoveryHelp').textContent = allOptions; } }

function renderComparisonContents(localText, remoteText) {
  const diffEngine = window.LocalDriveDiff || globalThis.LocalDriveDiff;
  const diff = diffEngine?.buildLineDiff
    ? diffEngine.buildLineDiff(localText, remoteText, undefined, 'exact')
    : fallbackComparisonDiff(localText, remoteText);
  const renderSide = side => diff.rows.map(row => {
    const text = side === 'left' ? row.left : row.right;
    const lineNumber = side === 'left' ? row.leftNumber : row.rightNumber;
    if (text === null) return `<span class="filler-line"><i class="line-number"></i></span>`;
    const remoteSide = side === 'right';
    const cls = row.same ? 'same-line' : `diff-line${remoteSide ? ' remote-line' : ''}`;
    const displayText = String(text).replace(/\r$/, '');
    return `<span class="${cls}"><i class="line-number">${lineNumber ?? ''}</i>${esc(displayText) || ' '}</span>`;
  }).join('');
  refs.localCompare.innerHTML = renderSide('left');
  refs.remoteCompare.innerHTML = renderSide('right');
}

function fallbackComparisonDiff(localText, remoteText) {
  const localLines = String(localText ?? '').split('\n');
  const remoteLines = String(remoteText ?? '').split('\n');
  const count = Math.max(localLines.length, remoteLines.length);
  let leftNumber = 0, rightNumber = 0;
  return {
    rows: Array.from({ length: count }, (_, index) => {
      const left = index < localLines.length ? localLines[index] : null;
      const right = index < remoteLines.length ? remoteLines[index] : null;
      const leftKey = left === null ? null : String(left).replace(/\r$/, '');
      const rightKey = right === null ? null : String(right).replace(/\r$/, '');
      if (left !== null) leftNumber++;
      if (right !== null) rightNumber++;
      return {
        left, right,
        leftNumber: left === null ? null : leftNumber,
        rightNumber: right === null ? null : rightNumber,
        same: left !== null && right !== null && leftKey === rightKey,
      };
    }),
  };
}

function formatSize(bytes) {
  if (!bytes) return '—'; if (bytes < 1024) return `${bytes} B`; if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`; return `${(bytes / 1048576).toFixed(1)} MB`;
}

function formatModified(seconds) {
  if (!seconds) return '—'; return new Intl.DateTimeFormat(undefined, { month: 'short', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(new Date(seconds * 1000));
}

function iconFor(entry) {
  if (entry.kind === 'folder') return '<span class="entry-icon folder">▰</span>';
  if (entry.kind === 'submodule') return '<span class="entry-icon submodule">◇</span>';
  if (entry.kind === 'symlink') return '<span class="entry-icon symlink">↗</span>';
  if (['deleted', 'deleted-folder', 'deleted-submodule'].includes(entry.kind)) return '<span class="entry-icon deleted">×</span>';
  return `<span class="entry-icon file">${esc((entry.name.split('.').pop() || '').slice(0, 3).toUpperCase())}</span>`;
}

// Only ever a real answer when the backend actually checked this submodule's
// own repository (submodule_checked) — a clean, fully-synced submodule
// deliberately skips that check to stay fast on a folder with many
// submodules, so it must fall back to the generic hint, not guess "detached"
// or a branch name it never verified.
function submoduleHeadHint(entry) {
  if (entry.submodule_initialized === false) return 'Git submodule · not initialized';
  if (!entry.submodule_checked) return 'Independent Git repository';
  return entry.submodule_current_branch ? `Independent Git repository · ${entry.submodule_current_branch}` : 'Independent Git repository · detached';
}

function untrackedItemLabel(entry) {
  if (!entry || entry.tracked) return '';
  if (entry.kind === 'folder') return 'New folder';
  if (entry.kind === 'symlink') return 'New symlink';
  if (entry.kind === 'submodule') return 'New submodule';
  return 'New file';
}

function entryKindHint(entry) {
  if (entry.kind === 'submodule') return submoduleHeadHint(entry);
  if (entry.kind === 'deleted-submodule') return 'Deleted Git submodule';
  if (entry.kind === 'deleted-folder') return 'Deleted tracked folder';
  if (entry.kind === 'deleted') return 'Deleted tracked file';
  return untrackedItemLabel(entry) || entry.kind;
}

function gitState(entry) {
  const statusStates = {
    M: { code: 'ML', label: 'Modified locally', tone: 'changed' },
    A: { code: 'AL', label: 'Staged new file', tone: 'changed' },
    D: { code: 'DL', label: 'Deleted locally', tone: 'changed' },
    R: { code: 'RL', label: 'Renamed locally', tone: 'changed' },
    U: { code: 'CF', label: 'Conflict', tone: 'changed' },
    '??': { code: 'UN', label: 'New file', tone: 'untracked' },
    '•': { code: 'MI', label: 'Modified files inside', tone: 'changed' },
  };
  const submoduleCodes = {
    changes_inside: 'CI',
    local_commit_push_needed: 'LP',
    sync_needed: 'SYNC',
    detached_choose_branch: 'BR',
    origin_missing: 'NO',
    on_origin_stage_project: 'NVS',
    on_origin_commit_project: 'NVC',
    project_commit_push_needed: 'PP',
    unavailable: 'ERR',
    not_initialized: 'NI',
    status_unknown: '?',
  };
  const states = [];
  const addState = (code, label, tone = '') => {
    if (!states.some(state => state.code === code && state.label === label)) states.push({ code, label, tone });
  };
  // Not the same as tracked === false ("genuinely untracked", a real answer)
  // — this is list_directory_fast's placeholder data, where tracked/status
  // mean nothing at all yet. Showing "New, untracked" here would be an
  // outright wrong answer, not just an imprecise one.
  if (entry.status_known === false) addState('…', 'Loading…', 'loading');
  // The label is wrapped in its own <span> (not just a bare text node next
  // to the dot) so an overly long one truncates itself with an ellipsis in
  // this fixed-width grid column instead of overflowing and squashing the
  // dot or deforming the row — "Contains unpushed commits" on a folder used
  // to do exactly that.
  else if (!entry.tracked) addState('UN', untrackedItemLabel(entry) || 'New file', 'untracked');
  else if (entry.kind === 'submodule') {
    if (entry.submodule_initialized === false) addState('NI', 'Not initialized', 'changed');
    const stateView = SubmoduleStateModel.presentation(entry.submodule_state);
    if (entry.submodule_initialized !== false && stateView.actionable) addState(submoduleCodes[entry.submodule_state] || '?', stateView.row || stateView.short, stateView.tone);
    else if (entry.submodule_initialized !== false && entry.status) {
      const state = statusStates[entry.status] || { code: entry.status, label: entry.status, tone: 'changed' };
      addState(state.code, state.label, state.tone);
    }
  } else if (entry.status) {
    const state = entry.status === '??'
      ? { code: 'UN', label: untrackedItemLabel(entry) || 'New file', tone: 'untracked' }
      : statusStates[entry.status] || { code: entry.status, label: entry.status, tone: 'changed' };
    addState(state.code, state.label, state.tone);
  }
  // Fully committed (no working-tree status at all) but that commit hasn't
  // reached the branch's upstream yet — a real, distinct state from both
  // "clean" and "modified": nothing here needs a commit, it needs a push.
  if (entry.kind !== 'submodule' && entry.unpushed && !states.some(state => ['LP', 'PP'].includes(state.code))) addState('NP', entry.kind === 'folder' ? 'Contains unpushed commits' : 'Not pushed yet', 'unpushed');
  if (entry.stashed) addState('ST', ['folder', 'deleted-folder'].includes(entry.kind) ? 'Contains stashed changes' : 'Stashed version exists', 'stashed');
  if (!states.length) addState('TR', 'Tracked');

  const title = states.map(state => `${state.code}: ${state.label}`).join(' · ');
  const tone = states.find(state => state.tone)?.tone || '';
  // Keep the familiar, readable wording whenever there is only one fact to
  // report. Abbreviations are only useful when two or more independent Git
  // facts must share this narrow column (for example ML + ST + NP).
  const badgeFor = state => `<b class="git-code-badge ${esc(state.tone)}" title="${esc(`${state.code}: ${state.label}`)}">${esc(state.code)}</b>`;
  const primaryState = states.find(state => !['ST', 'NP'].includes(state.code)) || states[0];
  const extraStates = states.filter(state => state !== primaryState);
  const content = states.length === 1
    ? `<em>${esc(states[0].label)}</em>`
    : `<em>${esc(primaryState.label)}</em>${extraStates.length ? `<span class="git-badge-tray">${extraStates.map(badgeFor).join('')}</span>` : ''}`;
  return `<span class="git-state ${esc(tone)} ${states.length > 1 ? 'combined' : ''}" title="${esc(title)}"><i class="git-dot"></i><span>${content}</span></span>`;
}

function entryGitSummary(entry) {
  if (!entry) return 'Unknown';
  if (entry.status_known === false) return 'Loading Git status…';
  if (!entry.tracked) return `${untrackedItemLabel(entry)} — not tracked yet`;
  if (entry.kind === 'submodule') {
    if (entry.submodule_initialized === false) return 'Submodule not initialized';
    const stateView = SubmoduleStateModel.presentation(entry.submodule_state);
    if (stateView?.actionable) return stateView.short;
  }
  if (entry.status === '??') return untrackedItemLabel(entry) || 'New file';
  if (entry.status === 'A') return 'Staged new file';
  if (entry.status === 'M') return entry.kind === 'folder' ? 'Modified files inside' : 'Modified locally';
  if (entry.status === 'D') return 'Deleted locally';
  if (entry.status === 'R') return 'Renamed locally';
  if (entry.status === 'U') return 'Conflict';
  if (entry.status === '•') return 'Modified files inside';
  if (entry.status) return entry.status;
  if (entry.unpushed) return entry.kind === 'folder' ? 'Clean — contains unpushed commits' : 'Committed, not pushed yet';
  return 'Tracked, clean';
}

function renderBreadcrumbs() {
  const parts = state.currentPath ? state.currentPath.split('/') : [];
  let accumulated = '';
  refs.breadcrumbs.innerHTML = `<button class="crumb root" data-path="">▰ ${esc(state.repository?.name || 'Repository')}</button>` + parts.map(part => {
    accumulated = accumulated ? `${accumulated}/${part}` : part;
    return `<span class="crumb-separator">›</span><button class="crumb" data-path="${esc(accumulated)}">${esc(part)}</button>`;
  }).join('');
  refs.breadcrumbs.querySelectorAll('[data-path]').forEach(crumb => crumb.addEventListener('click', () => openDirectory(crumb.dataset.path)));
}

// A folder with many thousands of entries used to build one real DOM node
// per entry unconditionally — fine up to a few hundred, but a genuinely
// enormous single folder (an unusual layout, but real ones exist: a flat
// asset dump, a generated-output directory) made this innerHTML assignment
// itself the dominant cost, freezing the window for it. True scroll
// virtualization (recycling a small pool of DOM rows as the user scrolls)
// would need a reliable fixed row height to do the scroll-offset math
// against, but .file-row is only min-height in CSS — close enough visually,
// not something worth trusting pixel-for-pixel. This caps how many rows
// this function ever hands to the DOM in one shot instead, the same
// explicit-continuation shape already used for a huge commit graph
// (GRAPH_COMMIT_WINDOW/"Load older", above) rather than an automatic
// infinite scroll: nothing here is silently hidden, it just isn't built
// into the DOM until asked for. Ordinary folders (the overwhelming common
// case) never come close to this and never notice it exists.
const EXPLORER_DOM_ROW_CAP = 500;

let explorerRenderState = { path: null, query: null, entries: null, showAll: false, pendingShowAll: false };
let lastExplorerClick = { path: null, time: 0 };
const EXPLORER_DOUBLE_CLICK_MS = 800;
function renderExplorer() {
  if (!state.repository) return;
  renderBreadcrumbs();
  const query = refs.search.value.trim().toLowerCase();
  const samePlace = explorerRenderState.path === state.currentPath && explorerRenderState.query === query;
  // If nothing but the selection changed (a plain click, no folder reload,
  // no new search, and no "Show all" click either) — update just the
  // "selected" class in place instead of rebuilding every row's DOM node.
  // Rebuilding on every click replaces the very button the user just
  // clicked — on Windows/WebView2 that resets the browser's double-click
  // sequence (it requires both clicks to land on the same element), so a
  // folder needed two double-clicks to open. macOS's WebKit is more
  // lenient about this, which is why it only showed up there.
  if (samePlace && explorerRenderState.entries === state.entries && !explorerRenderState.pendingShowAll) {
    refs.fileList.querySelectorAll('[data-entry]').forEach(row => row.classList.toggle('selected', state.selectedEntry?.relative_path === row.dataset.entry));
    return;
  }
  // A background refresh of the very same folder/search (a status scan
  // landing, a git operation completing) must not silently re-collapse an
  // already-expanded huge folder back under the cap; leaving this folder
  // (or starting a new search in it) is the only thing that should.
  const showAll = samePlace && (explorerRenderState.showAll || explorerRenderState.pendingShowAll);
  explorerRenderState = { path: state.currentPath, query, entries: state.entries, showAll, pendingShowAll: false };
  const buildStarted = performance.now();
  const allEntries = state.entries.filter(entry => !query || entry.name.toLowerCase().includes(query));
  const capped = !showAll && allEntries.length > EXPLORER_DOM_ROW_CAP;
  const entries = capped ? allEntries.slice(0, EXPLORER_DOM_ROW_CAP) : allEntries;
  const upRow = state.currentPath ? `<button class="file-row file-grid up-row" data-go-up="1">
    <span class="file-main"><span class="entry-icon folder">▲</span><span class="entry-copy"><span class="entry-name">..</span><span class="entry-hint">Parent folder</span></span></span>
    <span></span><span></span><span></span>
  </button>` : '';
  const rowsHtml = entries.map(entry => `<button class="file-row file-grid ${entry.status || !entry.tracked ? 'has-change' : ''} ${entryHasPersonalNote(entry) ? 'has-personal-note' : ''} ${state.selectedEntry?.relative_path === entry.relative_path ? 'selected' : ''}" data-entry="${esc(entry.relative_path)}">
    <span class="file-main">${iconFor(entry)}<span class="entry-copy"><span class="entry-name">${esc(entry.name)}${entryHasPersonalNote(entry) ? '<b class="personal-note-dot" title="Personal note">✎</b>' : ''}${entry.kind === 'submodule' ? '<b class="inline-submodule-badge">SUBMODULE</b>' : ''}</span><span class="entry-hint">${esc(entryKindHint(entry))}</span></span>${['folder','submodule'].includes(entry.kind) ? '<span class="folder-arrow">›</span>' : ''}</span>
    ${gitState(entry)}<span class="file-size">${entry.kind === 'file' ? formatSize(entry.size) : '—'}</span><span class="file-modified">${formatModified(entry.modified)}</span>
  </button>`).join('') || (state.currentPath ? '' : '<div class="empty-change">This folder is empty</div>');
  const showAllStub = capped ? `<div class="history-truncated-stub"><span>Showing ${EXPLORER_DOM_ROW_CAP} of ${allEntries.length} items</span><button id="explorerShowAll">Show all ${allEntries.length}</button></div>` : '';
  jsPerfLog(`renderExplorer build HTML (${state.currentPath || '/'}, ${entries.length}${capped ? `/${allEntries.length}` : ''} rows)`, performance.now() - buildStarted);
  // Isolated from the string-building above and attachFileListDelegation
  // below, so a slow render can be attributed to whichever of the three it
  // actually is instead of one lump "renderExplorer" number (the caller,
  // fetchAndRenderDirectory, separately times the backend/IPC round trip
  // this runs after — see its own jsPerfLog calls).
  const domStarted = performance.now();
  refs.fileList.innerHTML = upRow + rowsHtml + showAllStub;
  jsPerfLog(`renderExplorer DOM write (${state.currentPath || '/'}, ${entries.length} nodes)`, performance.now() - domStarted);
  // Single delegated listener on the container instead of one click +
  // one contextmenu listener per row: attaching thousands of individual
  // listeners (one pair per entry) was itself a real, measurable cost on
  // folders with many items — this makes listener setup O(1) per render
  // instead of O(entries), same behavior either way since we only ever
  // care which row the event happened inside.
  attachFileListDelegation();
  if (capped) { $('#explorerShowAll')?.addEventListener('click', () => { explorerRenderState.pendingShowAll = true; renderExplorer(); }); }
}

let fileListDelegationAttached = false;
function attachFileListDelegation() {
  if (fileListDelegationAttached) return;
  fileListDelegationAttached = true;
  refs.fileList.addEventListener('click', event => {
    const upRow = event.target.closest('[data-go-up]');
    if (upRow) {
      const now = Date.now();
      const isDoubleClick = lastExplorerClick.path === '..' && now - lastExplorerClick.time < EXPLORER_DOUBLE_CLICK_MS;
      lastExplorerClick = { path: '..', time: now };
      if (isDoubleClick) { lastExplorerClick = { path: null, time: 0 }; openDirectory(state.currentPath.split('/').slice(0, -1).join('/')); }
      return;
    }
    const row = event.target.closest('[data-entry]');
    if (!row) return;
    // Single click selects, double click opens a folder/submodule — the
    // familiar file-explorer convention. The browser's native `dblclick`
    // event requires both clicks to land on the very same DOM element within
    // its own timing window; since a click here re-renders (replacing rows
    // in the general case, and always doing real async work), Chromium/
    // WebView2 was unreliable about recognizing the second click as part of
    // the same double-click on Windows — macOS's WebKit is more lenient.
    // Detecting the double-click ourselves — by comparing the clicked path
    // and a timestamp, not the DOM node — sidesteps that entirely.
    const now = Date.now();
    const isDoubleClick = lastExplorerClick.path === row.dataset.entry && now - lastExplorerClick.time < EXPLORER_DOUBLE_CLICK_MS;
    lastExplorerClick = { path: row.dataset.entry, time: now };
    const entry = state.entries.find(item => item.relative_path === row.dataset.entry);
    if (isDoubleClick && entry && ['folder', 'submodule'].includes(entry.kind)) {
      lastExplorerClick = { path: null, time: 0 };
      if (pendingEntryDetailsTimeout) { clearTimeout(pendingEntryDetailsTimeout); pendingEntryDetailsTimeout = null; }
      openDirectory(entry.relative_path);
      return;
    }
    // Folders/submodules defer their entry_details fetch (see selectEntry) —
    // this first click is the common case where a second one follows right
    // after to navigate in, at which point the branch above cancels this
    // before it ever reaches the backend.
    selectEntry(row.dataset.entry, { deferMs: entry && ['folder', 'submodule'].includes(entry.kind) ? EXPLORER_DOUBLE_CLICK_MS : 0 });
  });
  refs.fileList.addEventListener('contextmenu', event => {
    const row = event.target.closest('[data-entry]');
    if (!row) return;
    const entry = state.entries.find(item => item.relative_path === row.dataset.entry);
    if (entry?.kind !== 'submodule') return;
    event.preventDefault(); selectEntry(entry.relative_path); openSubmoduleMenu(entry, event.clientX, event.clientY);
  });
}

// Bumped on every call and captured per-call below — if the user opens folder
// A then quickly folder B before A's backend call returns, A's result would
// otherwise arrive *after* B's and overwrite the (correct, already-showing)
// folder B listing with stale folder A data. Whichever call's result comes
// back is only applied if no newer navigation has started since.
// Fire-and-forget: writes into the same perf log the backend uses (see
// frontend_perf_log's doc comment in repository.rs). Never awaited and never
// throws into the caller — a logging failure (or running in browser-preview
// mode with no `invoke`) must never affect the actual operation being timed.
function jsPerfLog(label, elapsedMs) { if (invoke) invoke('frontend_perf_log', { label, elapsedMs }).catch(() => {}); }

// A full repository path can reveal more than a perf log needs to (usernames
// in home directory paths, internal project folder structure) — logs that
// need to identify *which* repository, not spell out exactly where it lives
// on disk, use just the last path segment (the folder/repo name) instead.
function anonymizeForLog(path) { if (!path) return '(none)'; return path.split(/[\\/]/).filter(Boolean).pop() || '(root)'; }

// The same staleness-guard idiom already used for Explorer folder loads
// (explorerRequestSeq below), generalized for every other async UI load
// whose result can outlive the context it was requested for. Call the
// returned begin() synchronously, before the first await — it captures a
// generation number *and* a snapshot of the identity fields a stale
// response must never be applied across (repository, submodule, folder,
// branch); call the function IT returns (stillCurrent()) after every await,
// before touching state or the DOM. Each caller keeps its own instance (a
// module-level `const xGuard = createRequestGuard();`) so one loader's
// requests never interfere with another's, and a *newer* call through the
// same instance invalidates an older one even if the identity fields
// happen to still match (for example, a duplicate click before the previous
// request returned).
function createRequestGuard() {
  let generation = 0;
  function snapshot() {
    const graphRepo = state.submoduleGraph ? state.submoduleGraph.repository : state.repository;
    return {
      repository: state.repository?.path || null,
      submodule: state.submoduleGraph?.repository?.path || null,
      folder: state.view === 'commander' ? state.commanderPath : state.currentPath,
      branch: graphRepo?.current_branch || null,
    };
  }
  return function begin() {
    const mine = ++generation;
    const context = snapshot();
    return function stillCurrent() {
      if (mine !== generation) return false;
      const now = snapshot();
      return context.repository === now.repository && context.submodule === now.submodule && context.folder === now.folder && context.branch === now.branch;
    };
  };
}

let explorerRequestSeq = 0;
// Purely client-side, no backend call: state.submodule_paths comes from
// load_repository/open_repository_fast's own index read, so this costs
// nothing beyond an array scan — every ordinary (non-submodule) folder click
// resolves to null here without ever reaching the backend for the question.
function normalizeRepositoryRelativePath(path = '') {
  return String(path || '').trim().replace(/\\/g, '/').replace(/^\/+|\/+$/g, '');
}
function parentPathOf(relativePath = '') {
  return normalizeRepositoryRelativePath(relativePath).split('/').slice(0, -1).join('/');
}
function submoduleNameFromPath(path = '') {
  const parts = normalizeRepositoryRelativePath(path).split('/').filter(Boolean);
  return parts.at(-1) || 'Submodule';
}
function knownSubmodulePaths({ longestFirst = true } = {}) {
  const paths = new Set((state.submodule_paths || []).map(normalizeRepositoryRelativePath).filter(Boolean));
  (state.entries || []).forEach(entry => { if (entry.kind === 'submodule') paths.add(normalizeRepositoryRelativePath(entry.relative_path)); });
  const sorted = [...paths].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' }));
  return longestFirst ? sorted.sort((a, b) => b.length - a.length || a.localeCompare(b, undefined, { sensitivity: 'base' })) : sorted;
}
function submoduleBoundaryFor(path) {
  const normalizedPath = normalizeRepositoryRelativePath(path);
  return knownSubmodulePaths().find(sub => normalizedPath === sub || normalizedPath.startsWith(`${sub}/`)) || null;
}
function innerPathForSubmodule(submodulePath, path) {
  const sub = normalizeRepositoryRelativePath(submodulePath);
  const normalizedPath = normalizeRepositoryRelativePath(path);
  if (!sub || normalizedPath === sub || !normalizedPath.startsWith(`${sub}/`)) return '';
  return normalizedPath.slice(sub.length + 1);
}
function submoduleEntryForPath(path) {
  const submodulePath = normalizeRepositoryRelativePath(path);
  const existing = state.selectedEntry?.kind === 'submodule' && normalizeRepositoryRelativePath(state.selectedEntry.relative_path) === submodulePath
    ? state.selectedEntry
    : (state.entries || []).find(entry => entry.kind === 'submodule' && normalizeRepositoryRelativePath(entry.relative_path) === submodulePath);
  return existing || { kind: 'submodule', name: submoduleNameFromPath(submodulePath), relative_path: submodulePath };
}
function submoduleCompareCandidates() {
  return knownSubmodulePaths({ longestFirst: false }).map(path => ({ path, name: submoduleNameFromPath(path), label: `${submoduleNameFromPath(path)} · ${path}` }));
}
function currentSubmoduleCompareContext() {
  if (state.submoduleGraph?.relativePath) {
    return { entry: { kind: 'submodule', name: state.submoduleGraph.name || submoduleNameFromPath(state.submoduleGraph.relativePath), relative_path: state.submoduleGraph.relativePath }, innerPath: '' };
  }
  const selected = state.selectedEntry;
  const selectedPath = normalizeRepositoryRelativePath(selected?.relative_path || '');
  let submodulePath = selected?.kind === 'submodule' ? selectedPath : '';
  const folderPath = selected?.kind === 'file' ? parentPathOf(selectedPath) : selectedPath;
  if (!submodulePath && folderPath) submodulePath = submoduleBoundaryFor(folderPath);
  if (!submodulePath) submodulePath = submoduleBoundaryFor(state.currentPath);
  if (!submodulePath) return null;
  const openPath = folderPath && (folderPath === submodulePath || folderPath.startsWith(`${submodulePath}/`)) ? folderPath : state.currentPath;
  return { entry: submoduleEntryForPath(submodulePath), innerPath: innerPathForSubmodule(submodulePath, openPath) };
}

// The actual load_directory round-trip + render. Keep this as one coherent
// backend read even inside a submodule: a previous fast-paint/background-scan
// experiment caused flicker and unreliable-feeling navigation on Windows.
async function fetchAndRenderDirectory(path, requestId, options) {
  if (!options.force && directoryCache.has(path)) { state.entries = directoryCache.get(path); render(); return; }
  refs.fileList.innerHTML = '<div class="loading-row"><i class="spinner"></i>Loading folder…</div>';
  if (!invoke) { state.entries = previewData.entries; directoryCache.set(path, state.entries); render(); return; }
  try {
    const invokeStarted = performance.now();
    const entries = await invoke('load_directory', { repositoryPath: state.repository.path, relativePath: path, force: !!options.invalidateGit });
    jsPerfLog(`openDirectory invoke(load_directory) (${path || '/'})`, performance.now() - invokeStarted);
    directoryCache.set(path, entries);
    if (requestId !== explorerRequestSeq) return;
    const renderStarted = performance.now();
    state.entries = entries; render();
    jsPerfLog(`openDirectory render() (${path || '/'}, ${entries.length} entries)`, performance.now() - renderStarted);
  } catch (error) {
    if (requestId !== explorerRequestSeq) return;
    status(String(error), 'error'); refs.fileList.innerHTML = `<div class="empty-change">${esc(String(error))}</div>`;
  }
}

// options.force only bypasses the *frontend* directoryCache (re-read this
// folder from the backend instead of trusting whatever was cached from an
// earlier navigation) — it does NOT, by itself, mean "the backend's Git
// status is stale". Those are two different questions: right after
// refresh_status/load_repository just computed a full, current status scan,
// every caller here wants a repaint with that freshly-scanned data (bypass
// the frontend cache, since it may hold pre-mutation entries) but must NOT
// throw that same-second scan away and pay for a second one. Only
// options.invalidateGit (the explicit "Reload folder" button — the one
// place the user is explicitly saying "show me whatever's on disk right
// now, I don't trust anything cached") asks the backend to invalidate and
// rescan. Conflating the two here previously meant every post-mutation
// repaint re-triggered a full backend rescan a few hundred milliseconds
// after the mutation's own reload had just paid for one.
async function openDirectory(path, options = {}) {
  if (!state.repository) return;
  state.currentPath = path; state.selectedEntry = null;
  const requestId = ++explorerRequestSeq;

  // During fast repository open, the expensive full status scan is already
  // running in the background. Do not let quick navigation into `work/` (or
  // any other folder) start its own `load_directory` status scan while that
  // first scan is still in flight; Windows logs showed exactly that turning a
  // normal folder click into a 20-48s wait. Keep navigation responsive with
  // the same filesystem-only listing used for the initial root paint. When
  // completeRepositoryOpenStatus finishes, it reloads the *current* folder
  // once with real Git status.
  if (!state.statusReady && !options.force && !options.invalidateGit) {
    const boundary = submoduleBoundaryFor(path);
    state.activeSubmodule = boundary ? { path: boundary, statusReady: false } : null;
    return paintDirectoryFast(path, requestId);
  }

  const boundary = submoduleBoundaryFor(path);
  if (!boundary) { state.activeSubmodule = null; return fetchAndRenderDirectory(path, requestId, options); }
  state.activeSubmodule = { path: boundary, statusReady: true };
  return fetchAndRenderDirectory(path, requestId, options);
}

// Filesystem-only variant of openDirectory used for the very first render of
// the repository root while opening it (see list_directory_fast's doc
// comment). It never populates directoryCache, so a later, real
// openDirectory(force:false) for the same folder can never accidentally
// serve this incomplete data back as if it were a real, status-complete
// listing.
async function paintDirectoryFast(path, requestId) {
  refs.fileList.innerHTML = '<div class="loading-row"><i class="spinner"></i>Loading folder…</div>';
  if (!invoke) { state.entries = previewData.entries; render(); return; }
  try {
    const entries = await invoke('list_directory_fast', { repositoryPath: state.repository.path, relativePath: path });
    if (requestId !== explorerRequestSeq) return;
    state.entries = entries; render();
  } catch (error) {
    if (requestId !== explorerRequestSeq) return;
    status(String(error), 'error'); refs.fileList.innerHTML = `<div class="empty-change">${esc(String(error))}</div>`;
  }
}

async function openDirectoryFast(path) {
  if (!state.repository) return;
  state.currentPath = path; state.selectedEntry = null;
  const requestId = ++explorerRequestSeq;
  await paintDirectoryFast(path, requestId);
}

const openSubmoduleMenuGuard = createRequestGuard();
function closeSubmoduleMenu() {
  refs.submoduleMenu.hidden = true;
  submoduleMenuData = null;
}
async function refreshSubmoduleMenu(button = null) {
  if (!submoduleMenuEntry || refs.submoduleMenu.hidden) return;
  const entry = submoduleMenuEntry;
  const entryPath = entry.relative_path;
  const stillCurrent = openSubmoduleMenuGuard();
  const finishButton = button ? beginButtonOperation(button, '↻') : () => {};
  refs.currentSubmoduleVersion.textContent = 'Refreshing…';
  refs.submoduleVersions.innerHTML = '<div class="version-loading"><i class="spinner"></i>Refreshing branches, tags and commits…</div>';
  if (!invoke) { renderSubmoduleVersions(); finishButton(); return; }
  try {
    const data = await invoke('submodule_versions', { repositoryPath: state.repository.path, relativePath: entryPath });
    if (!stillCurrent() || refs.submoduleMenu.hidden || submoduleMenuEntry?.relative_path !== entryPath) return;
    submoduleMenuData = data;
    renderSubmoduleVersions();
    if (button) status(`${entry.name}: submodule versions refreshed`);
  } catch (error) {
    if (stillCurrent() && !refs.submoduleMenu.hidden && submoduleMenuEntry?.relative_path === entryPath) refs.submoduleVersions.innerHTML = `<div class="version-loading">${esc(String(error))}</div>`;
  } finally { finishButton(); }
}
async function openSubmoduleMenu(entry, x, y) {
  submoduleMenuEntry = entry;
  openSubmoduleMenuGuard();
  refs.submoduleMenu.hidden = false;
  // Keep the wider, readable selector fully inside the viewport. Its old
  // 440px positioning clamp was left behind after the contents grew, so the
  // recovery and Checkout buttons could overlap text or extend off-screen.
  const menuWidth = Math.min(860, innerWidth - 32);
  const menuHeight = Math.min(760, innerHeight - 32);
  refs.submoduleMenu.style.left = `${Math.max(16, Math.min(x, innerWidth - menuWidth - 16))}px`;
  refs.submoduleMenu.style.top = `${Math.max(16, Math.min(y, innerHeight - menuHeight - 16))}px`;
  refs.submoduleMenuName.textContent = entry.name; refs.currentSubmoduleVersion.textContent = 'Loading…';
  refs.submoduleVersionSearch.value = '';
  refs.submoduleVersions.innerHTML = '<div class="version-loading"><i class="spinner"></i>Reading branches, tags and commits…</div>';
  if (!invoke) {
    submoduleMenuData = { path: entry.relative_path, current_revision: 'a39f21d81ce0', current_branch: 'main', parent_revision: 'a39f21d81ce0', current_containing_branches: ['main', 'origin/main'], history_context_branch: 'main', history_limit: 100, versions: [
      { name: 'main', revision: 'a39f21d81ce0', kind: 'branch', current: true, subject: 'Stable diagnostics API', author: 'Andrei Pop', date: '2026-08-14' },
      { name: 'release/2.4', revision: 'bd51e40ca112', kind: 'branch', current: false, subject: 'Release configuration', author: 'Maria Ionescu', date: '2026-08-12' },
      { name: 'origin/feature/events', revision: 'de91822aef33', kind: 'remote', current: false, subject: 'Add event mapping', author: 'Victor Ene', date: '2026-08-11' },
      { name: 'v1.0', revision: 'a39f21d81ce0', kind: 'tag', current: true, subject: 'Stable diagnostics API', author: 'Andrei Pop', date: '2026-08-14', attached_branch: 'main' },
      { name: 'v0.9', revision: 'bd51e40ca112', kind: 'tag', current: false, subject: 'Release configuration', author: 'Maria Ionescu', date: '2026-08-12', attached_branch: null },
      { name: 'a39f21d', revision: 'a39f21d81ce0', kind: 'commit', current: true, subject: 'Stable diagnostics API', author: 'Andrei Pop', date: '2026-08-14' },
      { name: 'bd51e40', revision: 'bd51e40ca112', kind: 'commit', current: false, subject: 'Release configuration', author: 'Maria Ionescu', date: '2026-08-12' }
    ] }; renderSubmoduleVersions(); return;
  }
  await refreshSubmoduleMenu();
}

function renderSubmoduleVersions() {
  if (!submoduleMenuData) return;
  const current = submoduleCurrentPresentation(submoduleMenuData);
  refs.currentSubmoduleVersion.textContent = current.text;
  $('#submoduleVersionHelp').textContent = current.help;
  const newVersionButton = $('#submoduleMenuNewBranch');
  newVersionButton.textContent = versionFilter === 'tag' ? '＋ Tag current commit…' : '＋ New branch…';
  newVersionButton.title = versionFilter === 'tag' ? `Create a tag pointing exactly at the active commit ${String(submoduleMenuData.current_revision || '').slice(0, 8)}` : 'Create a new branch in this submodule, from its current commit';
  const historyTab = document.querySelector('[data-version-filter="commit"]');
  historyTab.textContent = submoduleMenuData.current_branch ? 'Current branch history' : 'Current checkout history';
  const query = refs.submoduleVersionSearch.value.trim().toLowerCase();
  refs.submoduleVersionSearch.placeholder = versionFilter === 'complete'
    ? 'Open the complete history map to search every branch and tag…'
    : versionFilter === 'commit' ? 'Search loaded history by SHA, message or author…' : versionFilter === 'tag' ? 'Search tags, SHAs or messages…' : 'Search branches, SHAs or messages…';
  const matches = item => matchesSubmoduleVersion(item, query);
  const currentContext = submoduleCurrentContextHtml(submoduleMenuData);
  const detached = !submoduleMenuData.current_branch;
  let html;
  if (versionFilter === 'branch') {
    // Point 3: a local branch and its own tracking remote are one thing, not
    // two confusing, duplicate-looking rows — only a remote-tracking branch
    // with no local counterpart gets its own "Remote only" section.
    const { local, remoteOnly } = groupSubmoduleBranchVersions(submoduleMenuData.versions);
    const localRows = local.filter(matches);
    const remoteRows = remoteOnly.filter(matches);
    // Reset to upstream's own gate also needs to know about a dirty working
    // tree, not just committed ahead/behind divergence — submodule_is_dirty
    // (already computed for this exact row by load_directory, whenever there
    // was anything here to explain) is the same signal the Explorer's own
    // "Modified"/"New version" distinction already relies on.
    const renderBranch = item => submoduleVersionRowHtml({ ...item, checkout_detached: detached, dirty: item.current && !!submoduleMenuEntry?.submodule_is_dirty });
    const rows = localRows.map(renderBranch).join('')
      + (remoteRows.length ? `<div class="version-section-heading">REMOTE ONLY</div>${remoteRows.map(renderBranch).join('')}` : '');
    html = currentContext + (rows || `<div class="version-loading">${query ? 'No matches' : 'No branches found'}</div>`);
  } else if (versionFilter === 'complete') {
    html = `${currentContext}<div class="complete-history-card"><h3>Complete submodule history</h3><p>The current-history tab follows only the active branch context. Open the complete map to see commits, branch tips and tags across this submodule.</p><button type="button" data-open-complete-history>Open complete history map ↗</button></div>`;
  } else {
    // containing_branches now comes straight from the backend for every
    // commit row (not just the active checkout) — see submodule_versions_inner.
    const versions = submoduleMenuData.versions
      .filter(item => versionFilter === 'tag' ? item.kind === 'tag' : item.kind === 'commit')
      .filter(matches);
    const rows = versions.map(submoduleVersionRowHtml).join('') || `<div class="version-loading">${query ? 'No matches' : versionFilter === 'tag' ? 'No tags in this submodule' : 'No versions found'}</div>`;
    const historyContext = submoduleMenuData.history_context_branch
      ? `History of ${submoduleMenuData.history_context_branch}. The active checkout is marked CURRENT even when it is inside the branch rather than at its tip.`
      : 'No known branch contains this detached checkout. History starts at the active commit.';
    html = versionFilter === 'commit'
      ? `${currentContext}<div class="version-history-limit">${esc(historyContext)} Showing up to ${Number(submoduleMenuData.history_limit) || 100} commits; search covers this loaded history.</div>${rows}`
      : rows;
  }
  refs.submoduleVersions.innerHTML = html;
  refs.submoduleVersions.querySelector('[data-open-complete-history]')?.addEventListener('click', () => {
    if (submoduleMenuEntry) { closeSubmoduleMenu(); openSubmoduleGraph(submoduleMenuEntry); }
  });
  refs.submoduleVersions.querySelectorAll('[data-switch-version]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    const row = button.closest('[data-revision]');
    switchSubmoduleVersion(row.dataset.revision, row.dataset.versionKind, row.dataset.name, button);
  }));
  refs.submoduleVersions.querySelectorAll('[data-reset-upstream]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    discardSubmoduleBranchAndUseUpstream(button.dataset, button);
  }));
  // The active branch is exactly what submoduleMenuEntry already names —
  // reuses the same preview-then-confirm flow as "Push submodule" elsewhere
  // (Explorer context menu, command palette), not a second push path.
  refs.submoduleVersions.querySelectorAll('[data-push-version]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    if (submoduleMenuEntry) pushSubmodule(submoduleMenuEntry);
  }));
  refs.submoduleVersions.querySelectorAll('[data-copy-sha]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    navigator.clipboard?.writeText(button.dataset.copySha).then(() => status('Copied SHA to clipboard')).catch(() => {});
  }));
  refs.submoduleVersions.querySelectorAll('[data-tag-version]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    const row = button.closest('[data-revision]');
    if (submoduleMenuEntry && row) openCreateSubmoduleTagDialog(submoduleMenuEntry, row.dataset.revision, row.querySelector('.version-subject')?.textContent || '');
  }));
}

async function switchSubmoduleVersion(revision, kind, name, button = null) {
  if (!invoke) { refs.currentSubmoduleVersion.textContent = `preview · ${revision.slice(0, 8)}`; return; }
  const finishButton = beginButtonOperation(button, 'Switching…');
  try {
    status('Switching submodule version…', 'busy');
    await invoke('switch_submodule_version', { repositoryPath: state.repository.path, relativePath: submoduleMenuData.path, revision, versionKind: kind, name: name || '' });
    const folder = state.currentPath; const data = await invoke('load_repository', { path: state.repository.path, force: false });
    Object.assign(state, data); state.view = 'explorer'; directoryCache.clear(); closeSubmoduleMenu(); await openDirectory(folder, { force: true });
    const target = kind === 'branch' ? `branch "${name}"` : kind === 'remote' ? `remote branch "${name}" (detached at that commit)` : kind === 'tag' ? `tag "${name}" (detached at that commit)` : `commit ${revision.slice(0, 8)} (detached — not on any branch)`;
    const successMsg = `Submodule switched to ${target}. It now shows as "Modified" here — that's expected: the project hasn't recorded the new pointer yet. Select the submodule and use "Commit this item" to save it.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(`Could not switch version: ${message}`, 'error'); }
  finally { finishButton(); }
}

async function discardSubmoduleBranchAndUseUpstream({ name, upstream, ahead, behind }, button = null) {
  if (!submoduleMenuData) return;
  const counts = `${Number(ahead) || 0} ahead / ${Number(behind) || 0} behind`;
  const confirmed = await customConfirm(
    `Replace local branch "${name}" with ${upstream} (${counts})?\n\nThis is not Push, Pull, or the normal way to record a new submodule version. It is destructive recovery: it permanently deletes this branch's local-only commits, staged files, and uncommitted edits, deletes untracked and ignored files, then checks out the remote version.\n\nThe parent project's recorded submodule version is not changed automatically.`,
    { title: 'Replace local branch with remote', okLabel: `Replace with ${upstream}`, danger: true }
  );
  if (!confirmed) return;
  if (!invoke) return status(`Preview: discard local ${name} and use ${upstream}`);
  const finishButton = beginButtonOperation(button, 'Resetting…');
  try {
    status(`Fetching ${upstream} and replacing local ${name}…`, 'busy');
    const result = await invoke('reset_submodule_branch_to_upstream', { repositoryPath: state.repository.path, relativePath: submoduleMenuData.path, branchName: name });
    const folder = state.currentPath;
    const data = await invoke('load_repository', { path: state.repository.path, force: false });
    Object.assign(state, data); state.view = 'explorer'; directoryCache.clear(); closeSubmoduleMenu();
    await openDirectory(folder, { force: true });
    const message = `${submoduleMenuEntry?.name || 'Submodule'}: ${result.branch} now matches ${result.upstream} @ ${result.revision.slice(0, 8)}. Local-only commits and changes were discarded.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(`Could not replace the local branch: ${message}`, 'error'); }
  finally { finishButton(); }
}

// Cancelable handle for the deferred entry_details fetch below — shared so
// the double-click handler that navigates into a folder can cancel a
// still-pending one before it ever calls the backend.
let pendingEntryDetailsTimeout = null;

async function selectEntry(path, options = {}) {
  state.selectedEntry = state.entries.find(entry => entry.relative_path === path);
  if (state.selectedEntry?.kind === 'file' && refs.editorDialog.open) {
    openEditor(state.selectedEntry);
  }
  render();
  if (pendingEntryDetailsTimeout) { clearTimeout(pendingEntryDetailsTimeout); pendingEntryDetailsTimeout = null; }
  // Deleted tracked paths are intentionally present as synthetic Explorer
  // rows (load_directory cannot discover them through read_dir because they
  // no longer exist). Do not ask entry_details to stat a path known to be
  // absent; render its useful Git actions immediately and fetch only the
  // history lookup, which works for a deleted path.
  if (['deleted', 'deleted-folder', 'deleted-submodule'].includes(state.selectedEntry?.kind)) {
    const deletedEntry = { ...state.selectedEntry, item_count: null, last_commit_id: undefined };
    renderEntryDetails(deletedEntry);
    if (invoke) invoke('entry_last_commit', { repositoryPath: state.repository.path, relativePath: path })
      .then(last => {
        if (state.selectedEntry?.relative_path !== path) return;
        const section = $('#entryLastCommitSection'); if (!section) return;
        section.innerHTML = renderEntryLastCommitInner({ ...deletedEntry, last_commit_id: last?.id ?? null, last_commit_subject: last?.subject ?? null, last_commit_author: last?.author ?? null, last_commit_date: last?.date ?? null });
      })
      .catch(() => { const section = $('#entryLastCommitSection'); if (section && state.selectedEntry?.relative_path === path) section.innerHTML = renderEntryLastCommitInner({ ...deletedEntry, last_commit_id: null }); });
    else renderEntryDetails({ ...deletedEntry, last_commit_id: null });
    return;
  }
  if (!invoke) return renderEntryDetails({ ...state.selectedEntry, item_count: state.selectedEntry.kind === 'folder' ? 12 : null, submodule_url: state.selectedEntry.kind === 'submodule' ? 'git@example.com:platform/diagnostics-core.git' : null, submodule_branch: state.selectedEntry.kind === 'submodule' ? 'main' : null, last_commit_id: 'a39f21d', last_commit_subject: 'P:423421431 test', last_commit_author: 'Andrei Pop', last_commit_date: '2026-08-14' });
  const fetchDetails = async () => {
    try {
      const details = await invoke('entry_details', { repositoryPath: state.repository.path, relativePath: path });
      // Same staleness guard already used below for entry_last_commit: the user
      // may have already clicked a different entry by the time this resolves —
      // don't let an older selection's details overwrite what's now showing.
      if (state.selectedEntry?.relative_path !== path) return;
      renderEntryDetails(details);
      // "Last commit touching this path" is fetched separately — it can be a
      // genuinely heavy history walk on a large repository, and blocking the
      // whole details panel on it made clicking around a large folder feel
      // stuck. Only patch it in if this is still the selected entry — the
      // user may well have already clicked elsewhere by the time it resolves.
      invoke('entry_last_commit', { repositoryPath: state.repository.path, relativePath: path })
        .then(last => {
          if (state.selectedEntry?.relative_path !== path) return;
          const section = $('#entryLastCommitSection'); if (!section) return;
          section.innerHTML = renderEntryLastCommitInner({ ...details, last_commit_id: last?.id ?? null, last_commit_subject: last?.subject ?? null, last_commit_author: last?.author ?? null, last_commit_date: last?.date ?? null });
        })
        .catch(() => { const section = $('#entryLastCommitSection'); if (section && state.selectedEntry?.relative_path === path) section.innerHTML = renderEntryLastCommitInner({ ...details, last_commit_id: null }); });
    } catch (error) { handleError(error); }
  };
  // Selecting a folder/submodule this way happens on the *first* click of
  // what's very often actually a double-click to navigate into it — without
  // this, that first click always fired a real backend scan
  // (entry_details -> cached_git_metadata, a full scoped status scan) for a
  // details panel that gets thrown away a moment later when the second
  // click navigates away. Deferring it, and letting the double-click
  // handler cancel it outright, means a genuine double-click costs one scan
  // (openDirectory's) instead of two.
  if (options.deferMs) { pendingEntryDetailsTimeout = setTimeout(() => { pendingEntryDetailsTimeout = null; fetchDetails(); }, options.deferMs); }
  else { await fetchDetails(); }
}

function selectedScope() {
  return { path: state.selectedEntry?.relative_path || state.currentPath || '', name: state.selectedEntry?.name || (state.currentPath ? state.currentPath.split('/').pop() : state.repository?.name || 'repository') };
}

function scopeHasChanges(scope) {
  if (state.changes.some(change => !scope.path || change.path === scope.path || change.path.startsWith(`${scope.path}/`))) return true;
  // A file inside a submodule never shows up in state.changes — the parent's
  // own status scan can't see inside a submodule's own index at all, only the
  // submodule as one opaque entry. The currently selected entry's own status
  // (sourced correctly from the submodule's own repo when it's inside one) is
  // already accurate, so fall back to trusting that directly.
  const entry = state.selectedEntry;
  if (entry && entry.relative_path === scope.path) return Boolean(entry.status) || !entry.tracked;
  return false;
}

function openScopeCommit() {
  const scope = selectedScope();
  if (!scopeHasChanges(scope)) { const msg = `Nothing to commit — "${scope.name}" has no uncommitted local changes.`; status(msg); showOperationToast(msg, 'error'); return; }
  refs.commitScopeName.textContent = scope.name; refs.scopeCommitMessage.value = refs.defaultCommitMessage.value.trim(); refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); refs.commitScopeDialog.showModal(); refs.scopeCommitMessage.focus();
}

async function commitSelectedScope(event) {
  event.preventDefault(); const scope = selectedScope(); const message = refs.scopeCommitMessage.value.trim(); if (!message) return;
  if (!invoke) { refs.commitScopeDialog.close(); status(`Preview: committed ${scope.name}`); return; }
  if (!scopeHasChanges(scope)) { const msg = `Nothing to commit — "${scope.name}" has no uncommitted local changes.`; refs.commitScopeDialog.close(); status(msg); showOperationToast(msg, 'error'); return; }
  refs.confirmScopeCommit.disabled = true; refs.confirmScopeCommit.textContent = 'Committing…';
  try {
    await invoke('commit_path', { repositoryPath: state.repository.path, relativePath: scope.path, message });
    const folder = state.currentPath; refs.commitScopeDialog.close(); const data = await invoke('load_repository', { path: state.repository.path, force: false });
    Object.assign(state, data); state.allCommits = data.commits; state.selectedEntry = null; directoryCache.clear(); await openDirectory(folder, { force: true });
    const successMsg = `Committed "${scope.name}". Push when you're ready to send it to the server.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
  finally { refs.confirmScopeCommit.textContent = 'Commit selection'; refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); }
}

const showSelectedHistoryGuard = createRequestGuard();
async function showSelectedHistory() {
  const scope = selectedScope();
  // path_history walks the *parent* repository, filtered to commits whose
  // diff touched this path — for a path that is (or is inside) a submodule,
  // that only ever finds the parent's own gitlink-update commits, never a
  // single commit that actually happened inside the submodule's own
  // repository. This used to be exactly the "View history" bug already
  // fixed for the detail-action button (see handleDetailAction's own
  // comment) — this is the *other* way to reach the same call
  // (#showPathHistory, wired unconditionally below) that bug fix never
  // covered. submoduleBoundaryFor resolves both "a submodule row is
  // directly selected" and "currently browsing inside one" to the same
  // real submodule root.
  const submoduleRoot = submoduleBoundaryFor(scope.path);
  if (submoduleRoot) { return openSubmoduleGraph({ relative_path: submoduleRoot, name: submoduleRoot.split('/').pop() }); }
  const stillCurrent = showSelectedHistoryGuard();
  if (!invoke) { state.historyScope = scope.name; state.historyKind = 'path'; state.view = 'graph'; render(); return; }
  try {
    status(`Loading history for ${scope.name}…`, 'busy');
    const commits = await invoke('path_history', { repositoryPath: state.repository.path, relativePath: scope.path });
    if (!stillCurrent()) return; // repository/folder/branch changed, or a newer history request superseded this one
    state.commits = commits; state.historyScope = scope.name; state.historyKind = 'path'; state.view = 'graph'; refs.search.value = ''; render(); status(`${state.commits.length} commits for ${scope.name}`);
  } catch (error) { if (stillCurrent()) handleError(error); }
}

// Message C's second, deliberately separate concept: "which commits in the
// *parent* project changed this submodule's recorded version" — a real,
// legitimate, but genuinely different question from "Submodule Branch
// Map" (the submodule's own history, resolved in its own repository).
// Reuses path_history exactly as it already worked (it's the parent
// scoped to this one gitlink path — correct for *this* question, unlike
// showSelectedHistory's now-fixed submodule guard which routes *away* from
// it), but through its own explicit action and its own explicit title
// (render()'s own viewTitle/graphSubtitle logic reads state.historyKind),
// so a user only ever lands here on purpose, never by accident while
// asking for the submodule's own history.
const showSubmoduleReferenceChangesGuard = createRequestGuard();
async function showSubmoduleReferenceChanges(entry) {
  const stillCurrent = showSubmoduleReferenceChangesGuard();
  const parentRepositoryPath = state.repository?.path;
  const relativePath = entry.relative_path;
  const name = entry.name;
  if (!invoke) { state.historyScope = name; state.historyKind = 'submodule-refs'; state.view = 'graph'; render(); return; }
  try {
    status(`Loading parent-repository reference changes for ${name}…`, 'busy');
    const commits = await invoke('path_history', { repositoryPath: parentRepositoryPath, relativePath });
    if (!stillCurrent()) return;
    state.commits = commits; state.historyScope = name; state.historyKind = 'submodule-refs'; state.view = 'graph'; refs.search.value = ''; render();
    status(`${state.commits.length} parent-repository commit${state.commits.length === 1 ? '' : 's'} changed ${name}'s recorded version`);
  } catch (error) { if (stillCurrent()) handleError(error); }
}

const SPEC_ID_PATTERN = /(?:^|[^A-Z0-9])([A-Z0-9]{8}\.[A-Z0-9]{3})(?![A-Z0-9])/g;
function extractSpecIds(text = '') {
  const seen = new Set();
  const matches = [...String(text).matchAll(SPEC_ID_PATTERN)].map(match => match[1]);
  return matches.filter(value => {
    if (seen.has(value)) return false;
    seen.add(value);
    return true;
  });
}
function specDetailRows(...texts) {
  const specs = extractSpecIds(texts.filter(Boolean).join(' '));
  if (!specs.length) return '';
  return `<span>Spec</span><strong class="spec-list">${specs.map(spec => `<code>${esc(spec)}</code>`).join(' ')}</strong>`;
}
function compactRepositoryLabel(url = '') {
  const trimmed = String(url).trim().replace(/\.git$/, '').replace(/\/$/, '');
  if (!trimmed) return 'Open repository';
  try {
    const parsed = new URL(trimmed);
    return `${parsed.host}${parsed.pathname}`.replace(/\.git$/, '').replace(/\/$/, '');
  } catch (_) {
    const scpLike = trimmed.match(/^[^@]+@([^:]+):(.+)$/);
    if (scpLike) return `${scpLike[1]}/${scpLike[2]}`.replace(/\.git$/, '').replace(/\/$/, '');
    return trimmed.replace(/^(ssh:\/\/git@|https?:\/\/)/, '').replace(/\.git$/, '');
  }
}
function submoduleRepositoryLinkHtml(entry) {
  if (!entry.submodule_web_url) return esc(entry.submodule_url || 'Not configured');
  const label = compactRepositoryLabel(entry.submodule_web_url);
  return `<a href="${esc(entry.submodule_web_url)}" class="submodule-repository-link" data-submodule-path="${esc(entry.relative_path)}" title="${esc(entry.submodule_web_url)}">${esc(label)} ↗</a>`;
}
function submoduleRepositoryRowsHtml(entry) {
  const remote = entry.submodule_url || 'Not configured';
  const github = entry.submodule_web_url ? `<span>GitHub</span><strong>${submoduleRepositoryLinkHtml(entry)}</strong>` : '';
  return `<span>Remote</span><strong>${esc(remote)}</strong>${github}`;
}
function tagListHtml(tags = []) {
  const visible = tags.slice(0, 3);
  const more = tags.length > visible.length ? ` <small>+${tags.length - visible.length} more</small>` : '';
  return `${visible.map(tag => `<code>${esc(tag)}</code>`).join(' ')}${more}`;
}

// "Last commit touching this path" can be a genuinely heavy history walk on
// a large repository — computed separately from the rest of entry_details
// (see selectEntry) so it never blocks the fast details from showing.
// `entry.last_commit_id === undefined` means "not fetched yet" (as opposed
// to `null`, a real "nothing found"), so this can tell "still loading" apart
// from "loaded, no commit".
function renderEntryLastCommitInner(entry) {
  const title = entry.kind === 'submodule' ? 'PROJECT COMMIT (gitlink update)' : 'LAST COMMIT';
  if (entry.last_commit_id === undefined) return `<h3>${title}</h3><div class="detail-grid"><span>Commit</span><strong><i class="spinner"></i> Loading…</strong></div>`;
  return `<h3>${title}</h3><div class="detail-grid"><span>Commit</span><strong>${entry.last_commit_id ? `<a href="#" class="commit-server-link" data-commit-id="${esc(entry.last_commit_id)}" title="Open this commit on the server">${esc(entry.last_commit_id.slice(0, 8))} ↗</a>` : 'No commit'}</strong><span>Message</span><strong>${commitSubjectHtml(entry.last_commit_subject || '—')}</strong>${specDetailRows(entry.last_commit_subject)}<span>Author</span><strong>${esc(entry.last_commit_author || '—')}</strong><span>Date</span><strong>${esc(entry.last_commit_date || '—')}</strong></div>`;
}

function submoduleCheckoutBadgeHtml(entry) {
  if (entry.kind !== 'submodule') return '';
  if (entry.submodule_initialized === false) return '<span class="submodule-checkout-pill not-initialized">Not initialized</span>';
  const branch = entry.submodule_current_branch || '';
  return branch
    ? `<span class="submodule-checkout-pill attached" title="${esc(`This submodule is currently checked out on branch ${branch}`)}">On branch · ${esc(branch)}</span>`
    : '<span class="submodule-checkout-pill detached" title="This submodule is on a detached commit. Create or switch to a local branch before pushing.">Detached HEAD</span>';
}

function renderEntryDetails(entry) {
  const deletedEntry = ['deleted', 'deleted-folder', 'deleted-submodule'].includes(entry.kind);
  const kindLabel = entry.kind === 'submodule' ? 'Git submodule' : entry.kind === 'deleted-submodule' ? 'Deleted Git submodule' : entry.kind === 'deleted-folder' ? 'Deleted tracked folder' : entry.kind === 'deleted' ? 'Deleted tracked file' : entry.kind.charAt(0).toUpperCase() + entry.kind.slice(1);
  const submoduleState = entry.kind === 'submodule' ? SubmoduleStateModel.presentation(entry.submodule_initialized === false ? 'not_initialized' : entry.submodule_state) : null;
  const changeBanner = submoduleState?.actionable
    ? `<div class="local-change-banner"><i></i><div><strong>${esc(submoduleState.short)}</strong><span>${esc(submoduleState.detail)}</span></div></div>`
    : deletedEntry
      ? '<div class="local-change-banner"><i></i><div><strong>Deleted locally</strong><span>Git still tracks this item, but it no longer exists on disk. Commit this deletion to record it, or restore it from HEAD if the removal was accidental.</span></div></div>'
    : entry.status || !entry.tracked
      ? `<div class="local-change-banner"><i></i><div><strong>${entry.tracked ? 'Modified locally' : untrackedItemLabel(entry)}</strong><span>${entry.tracked ? 'This item differs from the committed repository state.' : 'This item is new on disk. Stage it to include it in the next commit.'}</span></div></div>`
      : '';
  const submoduleBranchName = entry.kind === 'submodule' ? (entry.submodule_current_branch || '') : '';
  const canCommitInsideSubmodule = entry.kind === 'submodule' && entry.submodule_state === 'changes_inside' && Boolean(submoduleBranchName);
  const canStashInsideSubmodule = entry.kind === 'submodule' && entry.submodule_is_dirty;
  const canPushSubmodule = entry.kind === 'submodule' && entry.submodule_initialized !== false && Boolean(submoduleBranchName);
  const detachedDirtySubmodule = entry.kind === 'submodule' && entry.submodule_initialized !== false && !submoduleBranchName && entry.submodule_is_dirty;
  const detachedDirtyBanner = detachedDirtySubmodule
    ? '<div class="detached-work-banner"><i></i><div><strong>Detached HEAD with local changes</strong><span>Create a branch here before Commit or Push. Your files stay as they are; the branch only gives these changes a safe name.</span></div><button data-detail-action="subnewbranch">Create branch here…</button></div>'
    : '';
  const submoduleCommitTooltip = canCommitInsideSubmodule
    ? `Commit uncommitted changes inside the checked-out branch "${submoduleBranchName}"`
    : entry.kind === 'submodule' && entry.submodule_state === 'changes_inside'
      ? 'Commit requires an attached local branch. Use New branch or Change version first; detached HEAD commits are blocked.'
      : 'No uncommitted files inside this submodule';
  const submodulePushTooltip = canPushSubmodule
    ? `Push the checked-out branch "${submoduleBranchName}" to this submodule's own remote`
    : 'Push requires an attached local branch. Create or switch to a branch first; detached HEAD commits are blocked.';
  const submoduleInitButton = entry.kind === 'submodule' && entry.submodule_initialized === false
    ? '<button data-detail-action="subinit" data-tooltip="Clone/check out this submodule at the commit recorded by the parent project. Equivalent to git submodule update --init for this path.">Initialize submodule</button>'
    : '';
  refs.details.innerHTML = `<div class="entry-details"><div class="entry-preview ${esc(entry.kind)}">${entry.kind === 'submodule' ? '◇' : entry.kind === 'folder' ? '▰' : '▤'}</div>
    <h2>${esc(entry.name)}</h2><div class="entry-path">${esc(entry.relative_path)}</div>${entry.kind === 'submodule' ? `<div class="submodule-badges"><span class="submodule-badge">◇ Git submodule</span>${submoduleCheckoutBadgeHtml(entry)}</div>` : ''}
    ${changeBanner}
    ${detachedDirtyBanner}
    <div class="context-actions">${deletedEntry ? '' : entry.kind === 'file' ? '<button data-detail-action="edit">Edit local file</button>' : ''}${entry.kind === 'submodule' ? '' : '<button data-detail-action="server">Open on server ↗</button>'}${entry.kind === 'submodule' ? '' : '<button data-detail-action="history">View history</button>'}${entry.kind === 'folder' ? '<button data-detail-action="restorefolder" class="danger-action-soft" data-tooltip="Restore only this folder from HEAD or a selected commit. Does not move HEAD or switch branch.">↶ Restore folder…</button><button data-detail-action="stashwork" data-tooltip="Stash uncommitted changes in the current repository. This is repository-scoped, not only this folder. If there is nothing local to save, Git Drill Down will refuse and explain why.">Stash repository changes</button>' : ''}${entry.status || !entry.tracked ? '<button data-detail-action="commit">Commit this item</button>' : ''}${entry.kind === 'deleted' ? '<button data-detail-action="head" data-tooltip="Restore this deleted file from your last local commit (HEAD)">↶ Restore from last commit (HEAD)</button>' : ''}${['folder','submodule'].includes(entry.kind) ? '<button data-detail-action="utrud" data-tooltip="Launches UTRUD with this folder path. UTRUD decides whether the selected folder is valid. Windows only.">▶ Run UTRUD</button>' : ''}${entry.kind === 'file' && (entry.status || !entry.tracked) ? `<button data-detail-action="stage" data-tooltip="git add — add this file's current content to staging">＋ Stage this file</button><button data-detail-action="unstage" data-tooltip="Unstage — git restore --staged. Removes only the staging entry; your edits on disk are kept exactly as they are.">− Unstage</button><button data-detail-action="stashfile" data-tooltip="Sets this file aside in this repository's own stash. Parent projects and submodules have separate stash lists.">⇕ Stash this file</button><button data-detail-action="head" class="danger-action-soft" data-tooltip="Restore from your last local commit (HEAD) — git checkout HEAD -- file. Permanently discards ALL edits; the file on disk becomes identical to what you last committed. Cannot be undone.">↶ Restore from last commit (HEAD)</button><button data-detail-action="compare" data-tooltip="Open side-by-side compare with restore options">⇄ Compare with remote</button>` : ''}${entry.kind === 'submodule' ? `${submoduleInitButton}<button data-detail-action="subserver">Open submodule repository ↗</button><button data-detail-action="subcompare" data-tooltip="Compare two exact revisions of this submodule without checkout">Compare submodule…</button><button data-detail-action="subgraph" data-tooltip="Open this submodule's own branch/commit history — never the parent project's">Submodule Branch Map</button><button data-detail-action="subrefchanges" data-tooltip="A different, narrower question: which commits in the PARENT project changed this submodule's recorded version. Not the submodule's own history.">Submodule Reference Changes</button><button data-detail-action="subnewbranch" data-tooltip="Create a new local branch in this submodule, starting from its current commit, and switch to it">＋ New branch…</button><button data-detail-action="versions">Change version</button><button data-detail-action="substash" ${canStashInsideSubmodule ? '' : 'disabled'} data-tooltip="${canStashInsideSubmodule ? 'Set aside uncommitted files inside this submodule only. The parent project is untouched.' : 'No uncommitted local files inside this submodule to stash.'}">Stash submodule changes</button><button data-detail-action="substashes" data-tooltip="View and restore this submodule's own stashes. The parent project's stash list is separate.">Submodule stashes</button><button data-detail-action="subcommit" ${canCommitInsideSubmodule ? '' : 'disabled'} data-tooltip="${esc(submoduleCommitTooltip)}">Commit submodule</button><button data-detail-action="subreset" class="danger-action-soft" ${entry.status ? '' : 'disabled'} data-tooltip="${entry.status ? 'Discard local changes and restore the exact submodule commit recorded by the parent project. This leaves detached HEAD, like git submodule update.' : 'The submodule already uses the version recorded by the parent project'}">↺ Restore project version…</button><button data-detail-action="subpull" data-tooltip="Fast-forward pull — brings in new commits from the submodule's remote. Refuses if it would require a manual merge.">Pull submodule</button><button data-detail-action="submerge" data-tooltip="Merge a branch into this submodule's current branch, with conflict resolution if needed">Merge branch…</button><button data-detail-action="subpush" ${canPushSubmodule ? '' : 'disabled'} data-tooltip="${esc(submodulePushTooltip)}">Push submodule</button><button data-detail-action="subforcepush" ${canPushSubmodule ? '' : 'disabled'} class="danger-action-soft" data-tooltip="${canPushSubmodule ? '⚠️ Overwrites the remote branch with your local history, discarding any commits there are not in yours. Only safe if nobody else uses that remote.' : esc(submodulePushTooltip)}">Force push submodule…</button><button data-detail-action="subfetch">Fetch submodule</button><button data-detail-action="location">Replace repository URL</button>` : ''}${deletedEntry ? '' : '<button class="danger-action" data-detail-action="delete">Delete…</button>'}</div>
    <div class="detail-section"><h3>GENERAL</h3><div class="detail-grid"><span>Type</span><strong>${kindLabel}</strong><span>Git</span><strong>${esc(entryGitSummary(entry))}</strong>
    ${entry.item_count != null ? `<span>Items</span><strong>${entry.item_count}</strong>` : `<span>Size</span><strong>${formatSize(entry.size)}</strong>`}<span>Modified</span><strong>${formatModified(entry.modified)}</strong></div></div>
    ${renderPersonalNoteSection(entry)}
    ${entry.kind === 'submodule' ? `<div class="detail-section"><h3>SUBMODULE</h3><div class="detail-grid">${submoduleRepositoryRowsHtml(entry)}<span>Current checkout</span><strong>${submoduleBranchName ? `On branch ${esc(submoduleBranchName)}` : entry.submodule_initialized === false ? 'Not initialized' : 'Detached HEAD'}</strong><span>Default branch</span><strong>${esc(entry.submodule_branch || 'Default')}</strong><span>Status</span><strong>${esc(submoduleState.short)}</strong></div>
    ${entry.submodule_unpushed_commits?.length ? `<div class="submodule-push-banner"><i></i><span>${entry.submodule_unpushed_commits.length} commit${entry.submodule_unpushed_commits.length === 1 ? '' : 's'} not yet pushed to its own remote:</span></div><div class="submodule-unpushed-list">${entry.submodule_unpushed_commits.map(commit => `<div class="submodule-unpushed-commit"><strong>${commitSubjectHtml(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div>`).join('')}</div>` : entry.submodule_push_status ? `<div class="submodule-push-banner"><i></i><span>${esc(entry.submodule_push_status)}</span></div>` : ''}</div>` : ''}
    ${entry.kind === 'submodule' ? `<div class="detail-section"><h3>SUBMODULE COMMIT (actual change)</h3><div class="detail-grid"><span>Commit</span><strong>${entry.submodule_commit_id ? `<a href="#" class="commit-server-link" data-commit-id="${esc(entry.submodule_commit_id)}" data-submodule-path="${esc(entry.relative_path)}" title="Open this commit on the submodule's own server">${esc(entry.submodule_commit_id.slice(0, 8))} ↗</a>` : 'No commit'}</strong>${entry.submodule_commit_tags?.length ? `<span>Tags</span><strong class="spec-list">${tagListHtml(entry.submodule_commit_tags)}</strong>` : ''}<span>Message</span><strong>${commitSubjectHtml(entry.submodule_commit_subject || '—')}</strong>${specDetailRows(entry.submodule_commit_subject, ...(entry.submodule_commit_tags || []))}<span>Author</span><strong>${esc(entry.submodule_commit_author || '—')}</strong><span>Date</span><strong>${esc(entry.submodule_commit_date || '—')}</strong></div></div>` : ''}
    <div class="detail-section" id="entryLastCommitSection">${renderEntryLastCommitInner(entry)}</div></div>`;
  refs.details.querySelectorAll('[data-detail-action]').forEach(button => button.addEventListener('click', () => { Promise.resolve(handleDetailAction(button.dataset.detailAction, entry, button)).catch(error => handleError(error)); }));
  refs.details.querySelectorAll('[data-note-action]').forEach(button => button.addEventListener('click', () => { Promise.resolve(button.dataset.noteAction === 'delete' ? deleteEntryPersonalNote(entry) : editEntryPersonalNote(entry)).catch(error => handleError(error)); }));
}

function folderRestoreSourceRevision() {
  if (!refs.folderRestoreModeCommit.checked) return 'HEAD';
  return state.folderRestore?.selectedCommit || '';
}

function changesForPathScope(scopePath) {
  const prefix = scopePath ? `${scopePath}/` : '';
  return (state.changes || []).filter(change => !scopePath || change.path === scopePath || change.path.startsWith(prefix));
}

function folderRestoreResultMessage(name, sourceLabel, remainingCount) {
  if (!remainingCount) return `${name}: restore finished. No local changes remain in this folder.`;
  if (sourceLabel === 'HEAD') {
    return `${name}: restore finished, but ${remainingCount} local change${remainingCount === 1 ? '' : 's'} still remain. Open “Folder changes” to see what Git still reports.`;
  }
  return `${name}: restored from ${sourceLabel}. ${remainingCount} local change${remainingCount === 1 ? '' : 's'} ${remainingCount === 1 ? 'is' : 'are'} expected until you commit this restored snapshot.`;
}

function updateFolderRestoreActionState() {
  const model = state.folderRestore;
  if (!model) return;
  const sourceRevision = folderRestoreSourceRevision();
  refs.confirmFolderRestore.disabled = !sourceRevision || model.loadingCommits;
  refs.confirmFolderRestore.textContent = model.preview ? 'Restore folder' : 'Preview & restore';
}

function renderFolderRestoreCommits() {
  const model = state.folderRestore;
  if (!model) return;
  if (model.loadingCommits) {
    refs.folderRestoreCommitList.innerHTML = '<div class="folder-restore-empty"><i class="spinner"></i> Loading folder history…</div>';
    return;
  }
  if (!model.commits.length) {
    refs.folderRestoreCommitList.innerHTML = '<div class="folder-restore-empty">No commits found for this folder.</div>';
    return;
  }
  refs.folderRestoreCommitList.innerHTML = model.commits.map(commit => {
    const selected = commit.id === model.selectedCommit;
    return `<button type="button" class="folder-restore-commit ${selected ? 'selected' : ''}" data-folder-restore-commit="${esc(commit.id)}"><strong>${commitSubjectHtml(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author || 'Unknown')} · ${esc(commit.date || '—')}</small></button>`;
  }).join('');
}

function renderFolderRestorePreview(preview = state.folderRestore?.preview) {
  if (!preview) {
    refs.folderRestorePreview.innerHTML = '<p>Choose a restore source, then preview the exact scoped operation.</p>';
    updateFolderRestoreActionState();
    return;
  }
  const tracked = preview.tracked_changes || [];
  const clean = preview.clean_candidates || [];
  const trackedHtml = tracked.length
    ? `<ul>${tracked.slice(0, 40).map(change => `<li><b>${esc(change.status)}</b> ${esc(change.path)}</li>`).join('')}${tracked.length > 40 ? `<li>…and ${tracked.length - 40} more</li>` : ''}</ul>`
    : '<p>No tracked file difference was detected for this source. Local tracked edits may still be replaced when restoring to HEAD.</p>';
  const cleanHtml = refs.folderRestoreClean.checked
    ? clean.length
      ? `<ul class="danger-list">${clean.slice(0, 40).map(path => `<li>${esc(path)}</li>`).join('')}${clean.length > 40 ? `<li>…and ${clean.length - 40} more</li>` : ''}</ul>`
      : '<p>No untracked, non-ignored items would be cleaned.</p>'
    : '<p>Clean folder is off. Untracked files will be left in place.</p>';
  refs.folderRestorePreview.innerHTML = `<div class="folder-restore-preview-card"><span>SOURCE</span><strong>${esc(preview.source_revision === 'HEAD' ? 'HEAD' : preview.source_id.slice(0, 8))} · ${commitSubjectHtml(preview.source_subject || 'No message')}</strong><small>${esc(preview.source_author || 'Unknown')} · ${esc(preview.source_date || '—')}</small></div><div class="folder-restore-preview-grid"><section><h4>Tracked changes to prepare</h4>${trackedHtml}</section><section><h4>Untracked clean preview</h4>${cleanHtml}</section></div>`;
  updateFolderRestoreActionState();
}

async function loadFolderRestoreCommits() {
  const model = state.folderRestore;
  if (!model || !invoke) return;
  model.loadingCommits = true;
  renderFolderRestoreCommits();
  try {
    const commits = await invoke('path_history', { repositoryPath: state.repository.path, relativePath: model.entry.relative_path });
    if (state.folderRestore !== model) return;
    model.commits = commits || [];
    if (!model.selectedCommit && model.commits[0]) model.selectedCommit = model.commits[0].id;
    renderFolderRestoreCommits();
  } catch (error) {
    refs.folderRestoreCommitList.innerHTML = `<div class="folder-restore-empty error">${esc(String(error))}</div>`;
  } finally {
    if (state.folderRestore === model) model.loadingCommits = false;
    updateFolderRestoreActionState();
  }
}

function openFolderRestoreDialog(entry) {
  if (entry.kind !== 'folder') return;
  state.folderRestore = { entry, commits: [], selectedCommit: '', loadingCommits: false, preview: null };
  refs.folderRestorePath.textContent = entry.relative_path;
  refs.folderRestorePath.title = entry.relative_path;
  refs.folderRestoreSubtitle.textContent = 'Restore index + working tree for this folder only. Branch and HEAD stay unchanged.';
  refs.folderRestoreModeHead.checked = true;
  refs.folderRestoreModeCommit.checked = false;
  refs.folderRestoreCommitPicker.hidden = true;
  refs.folderRestoreClean.checked = true;
  refs.folderRestoreStatus.textContent = '';
  renderFolderRestorePreview(null);
  refs.folderRestoreDialog.showModal();
}

async function previewFolderRestore() {
  const model = state.folderRestore;
  if (!model) return;
  const sourceRevision = folderRestoreSourceRevision();
  if (!sourceRevision) { refs.folderRestoreStatus.textContent = 'Choose a commit first.'; return; }
  if (!invoke) {
    model.preview = { folder: model.entry.relative_path, source_revision: sourceRevision, source_id: sourceRevision, source_subject: 'Preview only', source_author: 'Git DrillDown', source_date: '', tracked_changes: [], clean_candidates: [] };
    renderFolderRestorePreview();
    return;
  }
  const finishPreviewButton = beginButtonOperation(refs.previewFolderRestore, 'Previewing…');
  refs.confirmFolderRestore.disabled = true;
  refs.folderRestoreStatus.textContent = 'Preparing safe preview…';
  status(`Previewing restore for ${model.entry.name}…`, 'busy');
  try {
    model.preview = await invoke('preview_folder_restore', { repositoryPath: state.repository.path, relativePath: model.entry.relative_path, sourceRevision, cleanUntracked: refs.folderRestoreClean.checked });
    refs.folderRestoreStatus.textContent = 'Preview ready. Review the scope before restoring.';
    renderFolderRestorePreview();
    status(`${model.entry.name}: restore preview ready`);
  } catch (error) {
    model.preview = null;
    renderFolderRestorePreview(null);
    refs.folderRestoreStatus.textContent = String(error);
    status(`Restore preview failed for ${model.entry.name}`, 'error');
  } finally {
    finishPreviewButton();
  }
}

async function confirmFolderRestore() {
  const model = state.folderRestore;
  if (!model) return;
  const finishConfirmButton = beginButtonOperation(refs.confirmFolderRestore, model.preview ? 'Restoring…' : 'Previewing…');
  if (!model.preview) {
    refs.folderRestoreStatus.textContent = 'Preparing preview before restore…';
    await previewFolderRestore();
    if (!model.preview) { finishConfirmButton(); updateFolderRestoreActionState(); return; }
  }
  const cleanPaths = refs.folderRestoreClean.checked ? (model.preview.clean_candidates || []) : [];
  const sourceLabel = refs.folderRestoreModeCommit.checked ? model.preview.source_id.slice(0, 8) : 'HEAD';
  const cleanNote = cleanPaths.length ? `\n\nUntracked items to delete:\n${cleanPaths.slice(0, 12).join('\n')}${cleanPaths.length > 12 ? `\n…and ${cleanPaths.length - 12} more` : ''}` : '';
  const ok = await customConfirm(`This will replace only:\n${model.entry.relative_path}\n\nSource: ${sourceLabel} — ${model.preview.source_subject}\n\nIndex and working tree will both be updated. HEAD, branch and other folders will not be moved.${cleanNote}`, { title: 'Restore folder', danger: true, okLabel: 'Restore folder' });
  if (!ok) { finishConfirmButton(); updateFolderRestoreActionState(); return; }
  if (!invoke) { finishConfirmButton(); refs.folderRestoreDialog.close(); status(`Preview: restore ${model.entry.relative_path}`); return; }
  const restoredPath = model.entry.relative_path;
  const restoredName = model.entry.name;
  refs.confirmFolderRestore.innerHTML = '<i class="spinner" aria-hidden="true"></i><span>Restoring…</span>';
  refs.confirmFolderRestore.setAttribute('aria-label', 'Restoring…');
  refs.confirmFolderRestore.title = 'Restoring…';
  refs.folderRestoreStatus.textContent = 'Restoring selected folder…';
  status(`Restoring ${restoredName} from ${sourceLabel}…`, 'busy');
  try {
    await invoke('restore_folder', { repositoryPath: state.repository.path, relativePath: restoredPath, sourceRevision: model.preview.source_id, cleanPaths });
    refs.folderRestoreDialog.close();
    const reopenPath = state.currentPath;
    directoryCache.clear();
    const refreshStarted = performance.now();
    await refreshStatusAndFolder(state.repository.path, reopenPath);
    jsPerfLog('restoreFolder refreshStatusAndFolder', performance.now() - refreshStarted);
    const freshEntry = state.entries.find(entry => entry.relative_path === restoredPath);
    if (freshEntry) await selectEntry(restoredPath);
    const remainingCount = changesForPathScope(restoredPath).length;
    const message = folderRestoreResultMessage(restoredName, sourceLabel, remainingCount);
    status(message, sourceLabel === 'HEAD' && remainingCount ? 'error' : '');
    showOperationToast(`${message}\nNothing was committed or pushed.`, sourceLabel === 'HEAD' && remainingCount ? 'error' : 'success');
  } catch (error) {
    refs.folderRestoreStatus.textContent = String(error);
    handleError(error);
    try { state.changes = await invoke('refresh_status', { repositoryPath: state.repository.path }); render(); } catch (_) {}
  } finally {
    finishConfirmButton();
    updateFolderRestoreActionState();
  }
}

async function initializeSubmodule(entry, button = null) {
  if (!state.repository || entry?.kind !== 'submodule') return;
  const finishButton = button ? beginButtonOperation(button, 'Initializing…') : () => {};
  try {
    status(`Initializing submodule ${entry.relative_path}…`, 'busy');
    await invoke('init_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear();
    await refreshStatusAndFolder(state.repository.path, state.currentPath);
    if (state.selectedEntry?.relative_path === entry.relative_path || !state.selectedEntry) await selectEntry(entry.relative_path);
    const message = `Submodule ${entry.name} initialized.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) {
    handleError(error);
  } finally {
    finishButton();
  }
}

async function handleDetailAction(action, entry, button) {
  if (action === 'edit') return openEditor(entry);
  if (action === 'open') return openDirectory(entry.relative_path);
  // "View history" for a submodule used to run path_history against the
  // *parent* repository, scoped to the submodule's own path — which shows
  // which PARENT commits touched the gitlink pointer, not the submodule's
  // own branches/commits at all. That's a real, different, narrower
  // question nobody was actually asking here; what "View history" on a
  // submodule row means is its own branch map, same as the dedicated
  // "Submodule Branch Map" button already gives. Rather than leave two
  // identically-behaving buttons on the same row, "View history" itself is
  // hidden for a submodule entry (see the context-actions template above) —
  // this branch stays only as a defensive fallback in case something else
  // still dispatches 'history' for a submodule.
  if (action === 'history') return entry.kind === 'submodule' ? openSubmoduleGraph(entry) : showSelectedHistory();
  if (action === 'commit') return openScopeCommit();
  if (action === 'restorefolder') return openFolderRestoreDialog(entry);
  if (action === 'versions') return await openSubmoduleMenu(entry, innerWidth - 480, 110);
  if (action === 'subcompare') return openSubmoduleCompareFromEntry(entry);
  if (action === 'subgraph') return openSubmoduleGraph(entry);
  if (action === 'subrefchanges') return showSubmoduleReferenceChanges(entry);
  // A submodule isn't a folder inside the parent — "Open on server" on one
  // must go to the submodule's own repository (a sibling of the parent when
  // its .gitmodules URL is relative), never <parent-url>/tree/…/<path>. For a
  // submodule this behaves identically to the dedicated "Open submodule
  // repository ↗" button below, so "Open on server" itself is hidden there
  // (see the context-actions template above) rather than leave two
  // identical buttons on the same row; this branch stays only as a
  // defensive fallback.
  if (action === 'server') return openEntryOnServer(entry, entry.kind === 'submodule');
  if (action === 'subserver') return openEntryOnServer(entry, true);
  if (action === 'subinit') return initializeSubmodule(entry, button);
  if (action === 'location') return replaceSubmoduleLocation(entry);
  if (action === 'delete') return deleteEntry(entry, button);
  if (action === 'utrud') return runUtrud(entry);
  if (action === 'compare') return compareEntryWithRemote(entry);
  if (action === 'subcommit') return commitSubmoduleChanges(entry);
  if (action === 'stashwork') return stashWork();
  if (action === 'substash') return stashSubmoduleWork(entry);
  if (action === 'substashes') return showSubmoduleStashes(entry);
  if (action === 'subreset') return resetSubmodule(entry);
  if (action === 'subpull') return pullSubmodule(entry);
  if (action === 'submerge') return openMergeBranchDialog(mergeTargetForSubmodule(entry));
  if (action === 'subpush') return pushSubmodule(entry);
  if (action === 'subforcepush') return forcePushSubmodule(entry);
  if (action === 'subfetch') return fetchSubmodule(entry);
  if (action === 'subnewbranch') return createSubmoduleBranch(entry);
  if (['head','stage','unstage'].includes(action)) return runEntryFileAction(entry, action);
  if (action === 'stashfile') return stashOneFile(entry.relative_path);
}

async function runUtrud(entry) {
  if (!['folder','submodule'].includes(entry.kind)) return;
  if (!invoke) return status(`Preview: launch UTRUD for ${entry.relative_path}`);
  try { const detail = await invoke('run_utrud', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); status(detail); showOperationToast(detail, 'success'); }
  catch (error) { handleError(error); }
}

async function createSubmoduleBranch(entry) {
  if (entry.kind !== 'submodule') return;
  openNewBranchDialog(entry);
}

// Submodule-tag-creation report, point 4: reads submoduleMenuData (already
// loaded to open the version-selector popup this is triggered from) for the
// current branch/detached state and target SHA, instead of a fresh backend
// call — that data is already exactly current, and this dialog only ever
// opens right after it was fetched.
function openCreateSubmoduleTagDialog(entry, targetRevision = '', targetSubject = '') {
  if (entry.kind !== 'submodule' || !submoduleMenuData) return;
  state.newTagTarget = entry;
  state.newTagRevision = targetRevision || submoduleMenuData.current_revision;
  $('#newTagSubmoduleName').textContent = `${entry.name} (submodule)`;
  const isCurrent = state.newTagRevision === submoduleMenuData.current_revision;
  const branchState = isCurrent ? (submoduleMenuData.current_branch ? `active branch ⑂ ${submoduleMenuData.current_branch}` : 'active detached HEAD') : 'selected from History';
  $('#newTagContext').textContent = `${branchState} · tag will point to ${state.newTagRevision.slice(0, 8)}${targetSubject ? ` · ${targetSubject}` : ''}`;
  $('#newTagDirtyNotice').hidden = !entry.status;
  $('#newTagName').value = ''; $('#newTagMessage').value = ''; $('#newTagPush').checked = false;
  $('#newTagStatus').textContent = ''; $('#confirmNewTag').disabled = true;
  $('#newTagDialog').showModal();
  $('#newTagName').focus();
}
$('#newTagName').addEventListener('input', () => { $('#confirmNewTag').disabled = !$('#newTagName').value.trim(); });
$('#confirmNewTag').addEventListener('click', async () => {
  const target = state.newTagTarget; if (!target) return;
  const name = $('#newTagName').value.trim(); if (!name) return;
  const confirmButton = $('#confirmNewTag');
  confirmButton.disabled = true; confirmButton.textContent = 'Creating…';
  try {
    const result = await invoke('create_submodule_tag', { repositoryPath: state.repository.path, relativePath: target.relative_path, tagName: name, message: $('#newTagMessage').value.trim(), push: $('#newTagPush').checked, targetRevision: state.newTagRevision || null });
    $('#newTagDialog').close();
    directoryCache.clear(); if (state.selectedEntry?.relative_path === target.relative_path) await selectEntry(target.relative_path);
    if (!refs.submoduleMenu.hidden && submoduleMenuEntry?.relative_path === target.relative_path) await refreshSubmoduleMenu();
    const pushNote = !$('#newTagPush').checked ? '' : result.pushed ? ` ${result.push_detail}` : ` Not pushed: ${result.push_detail}`;
    const msg = `${target.name}: tag "${name}" created at ${result.target.slice(0, 8)} (${result.annotated ? 'annotated' : 'lightweight'}).${pushNote}`;
    status(msg); showOperationToast(msg, !$('#newTagPush').checked || result.pushed ? 'success' : '');
  } catch (error) { $('#newTagStatus').textContent = String(error); confirmButton.disabled = false; }
  finally { confirmButton.textContent = 'Create tag'; }
});

async function openEntryOnServer(entry, submodule) {
  if (!invoke) return status(`Preview: open ${entry.name} on server`);
  try {
    if (submodule) {
      await invoke('open_submodule_on_server', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    } else await invoke('open_repository_item', { repositoryPath: state.repository.path, relativePath: entry.relative_path, kind: entry.kind });
  } catch (error) { handleError(error); }
}

async function replaceSubmoduleLocation(entry) {
  const suggestedUrl = portableSubmoduleUrl(entry.submodule_url || '');
  const url = await customPrompt(`Portable .gitmodules URL for submodule ${entry.name}:`, suggestedUrl, { title: 'Fix or replace repository URL' });
  if (!url || url.trim() === (entry.submodule_url || '')) return;
  if (!await customConfirm(`Update the source for ${entry.relative_path}?\n\n.gitmodules keeps the portable URL. The submodule's local origin is resolved from the parent repository, then fetched.`, { title: 'Fix or replace repository URL', danger: true, okLabel: 'Update URL' })) return;
  if (!invoke) return status(`Preview: change submodule URL to ${url.trim()}`);
  try { status('Changing submodule repository and fetching refs…', 'busy'); await invoke('change_submodule_url', { repositoryPath: state.repository.path, relativePath: entry.relative_path, url: url.trim() }); const folder = state.currentPath; directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: folder }); status(`${entry.name}: repository location updated`); }
  catch (error) { handleError(error); }
}

async function commitSubmoduleChanges(entry) {
  if (entry.kind !== 'submodule') return;
  const message = await customPrompt(`Commit message for changes in ${entry.name}:`, '', { title: 'Commit submodule' });
  if (!message?.trim()) return;
  if (!invoke) return status(`Preview: committed changes in ${entry.name}`);
  try {
    status(`Committing changes in ${entry.name}…`, 'busy');
    // Push-submodule-workflow report, point 1: "Commit submodule" creates
    // only a local commit — it must not automatically push (also_push:
    // false). This used to also silently create a parent commit right here
    // regardless, whether or not anything had been pushed anywhere — exactly
    // how "Publish main project" could end up shipping a gitlink that
    // pointed at a commit that only ever existed on this machine. Push
    // remains its own separate, explicit action ("Push submodule").
    const result = await invoke('commit_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path, message: message.trim(), alsoPush: false });
    directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: state.currentPath });
    const successMsg = `${entry.name}: committed inside the submodule. ${result.push_detail}`;
    status(successMsg); showOperationToast(successMsg);
  }
  catch (error) { const message2 = handleError(error); showOperationToast(`Commit failed: ${message2}`, 'error'); }
}

async function resetSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  const confirmed = await customConfirm(`Restore "${entry.name}" to the exact commit recorded by the parent project?\n\nThis discards dirty edits, staged files and local-only commits, deletes untracked and ignored files, and leaves the submodule in detached HEAD. It does NOT make a local branch match origin. This cannot be undone from here.`, { title: 'Restore project version', okLabel: 'Restore project version', danger: true });
  if (!confirmed) return;
  if (!invoke) return status(`Preview: reset ${entry.name}`);
  try {
    status(`Restoring the project-recorded version of ${entry.name}…`, 'busy');
    await invoke('reset_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: state.currentPath });
    const successMsg = `${entry.name}: restored to the commit recorded by the parent project (detached HEAD). Local work was discarded; no branch was matched to origin.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message2 = handleError(error); showOperationToast(`Reset failed: ${message2}`, 'error'); }
}

async function pullSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: pulled ${entry.name}`);
  try {
    status(`Pulling ${entry.name} from its remote…`, 'busy');
    await invoke('pull_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: state.currentPath });
    if (state.selectedEntry?.relative_path === entry.relative_path) await selectEntry(entry.relative_path);
    const successMsg = `${entry.name}: pulled the latest commits from its remote (fast-forward).`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) {
    const message = handleError(error);
    if (String(error).toLowerCase().includes('diverged')) {
      showOperationToast(`${message}\nKeep both histories with “Merge branch…”, or deliberately replace the local branch with the remote version via “Change version” → “Replace with remote…”.`, 'error');
    } else { showOperationToast(message, 'error'); }
  }
}

// ---- Merge & conflict resolution -----------------------------------------
// Works identically for the main repository and for a submodule — a submodule
// is just another repository at a different path, addressed the same way the
// rest of the app already does: (parent repositoryPath, submodule targetPath).
function mergeTargetForMain() { return { targetPath: '', label: state.repository.current_branch || 'current branch', isSubmodule: false }; }
function mergeTargetForSubmodule(entry) { return { targetPath: entry.relative_path, label: entry.name, isSubmodule: true }; }

function updateMergeDirectionPreview() {
  const source = refs.mergeBranchSource.value;
  const target = refs.mergeBranchCurrent.value || state.mergeTarget?.label || 'current branch';
  refs.mergeBranchStatus.textContent = source ? `Direction: ${source} → ${target}. The current branch is the only branch changed; nothing is pushed.` : '';
}

async function openMergeBranchDialog(target, preselectedSource = '') {
  state.mergeTarget = target;
  refs.mergeBranchCurrent.value = target.label;
  refs.mergeBranchSubtitle.textContent = target.isSubmodule ? `Merge a branch into "${target.label}" (submodule).` : "Bring another branch's commits into your current branch.";
  refs.mergeBranchStatus.textContent = '';
  refs.confirmMergeBranch.disabled = false; refs.confirmMergeBranch.textContent = 'Merge';
  if (!invoke) { refs.mergeBranchSource.innerHTML = '<option value="main">main</option>'; refs.mergeBranchDialog.showModal(); return; }
  try {
    const branches = target.isSubmodule
      ? (await invoke('submodule_repository', { repositoryPath: state.repository.path, relativePath: target.targetPath })).branches
      : state.branches;
    if (target.isSubmodule) {
      const current = branches.find(branch => branch.current);
      const currentName = current?.name || 'detached HEAD';
      refs.mergeBranchCurrent.value = `${currentName} · ${target.label}`;
      refs.mergeBranchSubtitle.textContent = `Merge a branch into the current checkout of submodule "${target.label}". Only this submodule repository changes.`;
    }
    const options = branches.filter(branch => !branch.current);
    refs.mergeBranchSource.innerHTML = options.map(branch => `<option value="${esc(branch.name)}">${esc(branch.name)}${branch.remote ? ' (remote)' : ''}</option>`).join('') || '<option value="" disabled>No other branches</option>';
    if (preselectedSource && options.some(branch => branch.name === preselectedSource)) refs.mergeBranchSource.value = preselectedSource;
    updateMergeDirectionPreview();
    refs.mergeBranchDialog.showModal();
  } catch (error) { handleError(error); }
}

async function refreshAfterMerge() {
  directoryCache.clear();
  await loadRepository(state.repository.path, { keepPath: true });
  await checkForMergeConflicts();
}

refs.confirmMergeBranch.addEventListener('click', async () => {
  const sourceRef = refs.mergeBranchSource.value; if (!sourceRef) return;
  const target = state.mergeTarget;
  if (!invoke) { refs.mergeBranchDialog.close(); status(`Preview: merged ${sourceRef}`); return; }
  refs.confirmMergeBranch.disabled = true; refs.confirmMergeBranch.textContent = 'Merging…';
  try {
    const outcome = await invoke('merge_branch', { repositoryPath: state.repository.path, targetPath: target.targetPath, sourceRef });
    if (outcome.status === 'conflicts') {
      refs.mergeBranchDialog.close();
      await refreshAfterMerge();
      openConflictsDialog(target, outcome.conflicts, outcome.message);
    } else {
      refs.mergeBranchDialog.close();
      status(outcome.message); showOperationToast(outcome.message, 'success');
      await refreshAfterMerge();
    }
  } catch (error) { refs.mergeBranchStatus.textContent = String(error); handleError(error); }
  finally { refs.confirmMergeBranch.disabled = false; refs.confirmMergeBranch.textContent = 'Merge'; }
});
refs.mergeBranchSource.addEventListener('change', updateMergeDirectionPreview);

function renderConflictsList(target, conflicts) {
  refs.conflictsList.innerHTML = conflicts.map(conflict => {
    const isSubmoduleConflict = conflict.kind === 'submodule';
    const itemLabel = isSubmoduleConflict ? 'submodule version' : 'file';
    const mineTip = isSubmoduleConflict ? 'Keep the submodule commit recorded by the current branch' : 'Keep your version of this file';
    const theirsTip = isSubmoduleConflict ? 'Use the submodule commit recorded by the incoming branch' : "Keep the incoming branch's version of this file";
    return `<div class="conflict-row" data-path="${esc(conflict.path)}">
    <div class="conflict-head"><span class="conflict-path">${esc(conflict.path)}</span><span class="conflict-state pending" data-tooltip="Git still reports this ${esc(itemLabel)} as unresolved in the index">${isSubmoduleConflict ? 'SUBMODULE VERSION' : 'UNRESOLVED'}</span></div>
    <div class="conflict-actions">
      <button data-resolve="ours" ${conflict.has_ours ? '' : 'disabled'} data-tooltip="${esc(mineTip)}">↤ Keep mine</button>
      <button data-resolve="theirs" ${conflict.has_theirs ? '' : 'disabled'} data-tooltip="${esc(theirsTip)}">↦ Keep theirs</button>
      <button data-resolve="mergetool" ${isSubmoduleConflict ? 'disabled' : ''} data-tooltip="Use Git's configured mergetool for this conflicted file">◇ Resolve with Git mergetool</button>
      <button data-resolve="manual" ${isSubmoduleConflict ? 'disabled' : ''} data-tooltip="Open the file (with conflict markers) and edit it yourself">✎ Edit manually</button>
    </div>
  </div>`;
  }).join('') || '<div class="empty-change">No unresolved conflicts remain. Files resolved through this dialog have been staged in Git; complete the merge when you are ready.</div>';
  refs.conflictsList.querySelectorAll('[data-resolve]').forEach(button => button.addEventListener('click', () => {
    const path = button.closest('.conflict-row').dataset.path; const kind = button.dataset.resolve;
    if (kind === 'manual') editConflictFile(target, path);
    else if (kind === 'mergetool') resolveConflictWithMergeTool(target, path, button);
    else resolveConflictAction(target, path, kind);
  }));
}

function conflictRepositoryPath(target) { return target?.repositoryPath || state.repository?.path; }

function openConflictsDialog(target, conflicts, introMessage) {
  state.mergeTarget = target;
  const isStash = target.kind === 'stash';
  refs.conflictsTitle.textContent = isStash ? 'Stash conflicts' : 'Merge conflicts';
  refs.conflictsSubtitle.textContent = introMessage || (isStash
    ? `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} need resolution before the stashed changes are fully applied.`
    : `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} need resolution before the merge can be completed.`);
  // A stash pop needs no merge commit — the resolved content just becomes
  // your regular working-tree changes, ready for your next normal commit —
  // so there's nothing to type a message for here.
  refs.conflictsCommitMessageLabel.hidden = isStash;
  refs.conflictsLocalNote.hidden = isStash;
  refs.conflictsCommitMessage.value = target.isSubmodule ? `Merge into ${target.label}` : `Merge into ${state.repository.current_branch}`;
  refs.confirmCompleteMerge.textContent = isStash ? 'Done' : 'Complete merge';
  refs.abortMergeButton.textContent = isStash ? 'Discard and keep the stash' : 'Abort merge';
  refs.conflictsStatus.textContent = '';
  refs.conflictsStatus.textContent = conflicts.length
    ? `${conflicts.length} unresolved file${conflicts.length === 1 ? '' : 's'}. Resolve each file, then complete or abort the merge.`
    : 'No unresolved conflicts remain. Complete the merge when ready.';
  renderConflictsList(target, conflicts);
  refs.conflictsDialog.showModal();
}

async function refreshConflictsDialog(target) {
  try {
    const conflicts = await invoke('list_conflicts', { repositoryPath: conflictRepositoryPath(target), targetPath: target.targetPath });
    renderConflictsList(target, conflicts);
    const isStash = target.kind === 'stash';
    refs.conflictsSubtitle.textContent = conflicts.length
      ? `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} still need resolution.`
      : (isStash ? 'All conflicts resolved — the stashed changes are now in your working tree.' : 'All conflicts resolved — ready to complete the merge.');
    refs.conflictsStatus.textContent = conflicts.length
      ? `${conflicts.length} unresolved file${conflicts.length === 1 ? '' : 's'} remain.`
      : (isStash ? 'Resolved — no merge commit is required for a stash.' : 'Resolved/staged — ready to complete the merge.');
    await checkForMergeConflicts();
  } catch (error) { handleError(error); }
}

async function resolveConflictAction(target, path, kind) {
  try {
    status(`Resolving ${path}…`, 'busy');
    await invoke('resolve_conflict', { repositoryPath: conflictRepositoryPath(target), targetPath: target.targetPath, relativePath: path, resolution: kind });
    const msg = `${path}: kept ${kind === 'ours' ? 'your' : 'the incoming'} version.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshConflictsDialog(target);
  } catch (error) { handleError(error); }
}

async function resolveConflictWithMergeTool(target, path, button) {
  if (!invoke) { status(`Preview: open mergetool for ${path}`); return; }
  const finishButton = beginButtonOperation(button, 'Opening tool…');
  try {
    status(`Opening Git mergetool for ${path}…`, 'busy');
    const message = await invoke('open_merge_tool', { repositoryPath: conflictRepositoryPath(target), targetPath: target.targetPath, relativePath: path });
    status(message); showOperationToast(`${message}\nIf the file is resolved, mark/save it or complete the merge when no conflicts remain.`, 'success');
    await refreshConflictsDialog(target);
  } catch (error) {
    const message = String(error);
    handleError(message);
    if (/merge\.tool|mergetool|tool-help|not configured|unknown tool|not available/i.test(message)) {
      const configure = await customConfirm(
        `Git could not start a merge tool for ${path}.\n\nYou can configure any Git-compatible mergetool (for example: bcomp, bc, meld, vimdiff, opendiff) and retry.\n\nConfigure merge.tool for this repository now?`,
        { title: 'Configure Git mergetool', okLabel: 'Configure tool' }
      );
      if (configure) {
        const tool = await customPrompt('Git mergetool name:', 'bcomp', { title: 'Set merge.tool', okLabel: 'Save and retry' });
        if (tool?.trim()) {
          const result = await invoke('run_git_command', { repositoryPath: conflictRepositoryPath(target), args: `config merge.tool ${tool.trim()}` });
          if (!result.success) throw new Error(result.stderr || result.stdout || `Could not configure merge.tool ${tool.trim()}`);
          showOperationToast(`Configured Git merge.tool = ${tool.trim()}. Retrying…`, 'success');
          await resolveConflictWithMergeTool(target, path, button);
        }
      }
    }
  }
  finally { finishButton(); }
}

function editConflictFile(target, path) {
  const joined = target.targetPath ? `${target.targetPath}/${path}` : path;
  state.editingConflict = { target, path };
  refs.editorTitle.textContent = `Resolve ${path}`;
  refs.editorPath.textContent = path;
  refs.editorContent.value = 'Loading…';
  refs.editorDialog.showModal();
  if (!invoke) { refs.editorContent.value = '<<<<<<< HEAD\n(your version)\n=======\n(their version)\n>>>>>>> branch\n'; return; }
  invoke('read_text_file', { repositoryPath: conflictRepositoryPath(target), relativePath: joined })
    .then(file => { refs.editorContent.value = file.content; refs.editorContent.focus(); })
    .catch(error => { refs.editorDialog.close(); handleError(error); });
}

refs.confirmCompleteMerge.addEventListener('click', async () => {
  const target = state.mergeTarget;
  if (target.kind === 'stash') {
    // No merge commit to make here — just confirm nothing is still
    // conflicted before letting the user walk away with it.
    try {
      const conflicts = await invoke('list_conflicts', { repositoryPath: conflictRepositoryPath(target), targetPath: target.targetPath });
      if (conflicts.length) { refs.conflictsStatus.textContent = `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} still need resolution first.`; return; }
      refs.conflictsDialog.close();
      const msg = 'Stash conflicts resolved — the changes are in your working tree, ready to commit.';
      status(msg); showOperationToast(msg, 'success');
      if (target.stashContext) await refreshAfterStashContext(target.stashContext); else await refreshAfterMerge();
    } catch (error) { refs.conflictsStatus.textContent = String(error); }
    return;
  }
  const message = refs.conflictsCommitMessage.value.trim();
  if (!message) { refs.conflictsStatus.textContent = 'A merge commit message is required.'; return; }
  refs.confirmCompleteMerge.disabled = true; refs.confirmCompleteMerge.textContent = 'Completing…';
  try {
    await invoke('complete_merge', { repositoryPath: state.repository.path, targetPath: target.targetPath, message });
    refs.conflictsDialog.close();
    const msg = `Merge completed${target.isSubmodule ? ` in ${target.label}` : ''}.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshAfterMerge();
  } catch (error) { refs.conflictsStatus.textContent = String(error); }
  finally { refs.confirmCompleteMerge.disabled = false; refs.confirmCompleteMerge.textContent = target.kind === 'stash' ? 'Done' : 'Complete merge'; }
});

refs.abortMergeButton.addEventListener('click', async () => {
  const target = state.mergeTarget;
  if (target.kind === 'stash') {
    if (!await customConfirm('Discard the conflict markers and restore your last commit? The stashed change stays in the stash list — nothing is lost, you can pop it again (or resolve it differently) later.', { title: 'Discard stash conflict', danger: true, okLabel: 'Discard' })) return;
    try {
      await invoke('abort_stash_conflict', { repositoryPath: conflictRepositoryPath(target) });
      refs.conflictsDialog.close();
      const msg = 'Discarded — your working tree is back to normal, and the stash is still there.';
      status(msg); showOperationToast(msg, 'success');
      if (target.stashContext) await refreshAfterStashContext(target.stashContext); else await refreshAfterMerge();
      state.hasStash = true; updateStashUI();
    } catch (error) { handleError(error); }
    return;
  }
  if (!await customConfirm('Abort this merge? All conflict resolutions made so far will be discarded and the repository will return to its pre-merge state.', { title: 'Abort merge', danger: true, okLabel: 'Abort merge' })) return;
  try {
    await invoke('abort_merge', { repositoryPath: state.repository.path, targetPath: target.targetPath });
    refs.conflictsDialog.close();
    const msg = `Merge aborted${target.isSubmodule ? ` in ${target.label}` : ''}.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshAfterMerge();
  } catch (error) { handleError(error); }
});

// Detects a merge left mid-resolution (e.g. the app was closed before finishing)
// and surfaces it via the sidebar banner so it's never silently stuck.
const checkForMergeConflictsGuard = createRequestGuard();
async function checkForMergeConflicts() {
  if (!invoke || !state.repository) { refs.mergeConflictsBanner.hidden = true; return; }
  const stillCurrent = checkForMergeConflictsGuard();
  try {
    const conflicts = await invoke('list_conflicts', { repositoryPath: state.repository.path, targetPath: '' });
    if (!stillCurrent()) return;
    refs.mergeConflictsBanner.hidden = conflicts.length === 0;
    if (conflicts.length) {
      // The same conflicted-index state can come from a real merge or from a
      // stash pop that couldn't apply cleanly — they need different finishing
      // steps (a merge commit vs. nothing at all), so which one this banner
      // means has to be checked, not assumed.
      const inMerge = await invoke('merge_in_progress', { repositoryPath: state.repository.path, targetPath: '' }).catch(() => true);
      if (!stillCurrent()) return;
      state.pendingConflictsKind = inMerge ? 'merge' : 'stash';
      refs.mergeConflictsSubtitle.textContent = `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} to resolve${inMerge ? '' : ' (from a stash)'}`;
    }
    state.pendingMainConflicts = conflicts;
  } catch { if (stillCurrent()) refs.mergeConflictsBanner.hidden = true; }
}

refs.mergeConflictsBanner.addEventListener('click', () => openConflictsDialog({ ...mergeTargetForMain(), kind: state.pendingConflictsKind || 'merge' }, state.pendingMainConflicts || []));
$('#mergeCurrent').addEventListener('click', () => state.repository && openMergeBranchDialog(mergeTargetForMain()));

async function forcePushSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  const firstConfirm = await customConfirm(
    `⚠️ FORCE PUSH will OVERWRITE the remote branch for "${entry.name}" with your local history. Any commits on the remote that aren't in your local history will be PERMANENTLY LOST from the server.\n\nOnly do this if you're certain nobody else is using that remote. Continue?`,
    { title: 'Force push — destructive', danger: true, okLabel: 'I understand, continue' }
  );
  if (!firstConfirm) { status('Force push cancelled'); return; }
  const secondConfirm = await customConfirm(
    `Last check: force push submodule "${entry.name}" now? This cannot be undone from here.`,
    { title: 'Force push — final confirmation', danger: true, okLabel: 'Force push' }
  );
  if (!secondConfirm) { status('Force push cancelled'); return; }
  if (!invoke) return status(`Preview: force pushed ${entry.name}`);
  try {
    status(`Force pushing ${entry.name}…`, 'busy');
    const result = await invoke('force_push_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: state.currentPath });
    const shortSha = (result?.revision || '').slice(0, 8);
    const successMsg = `${entry.name}: force pushed to branch "${result?.branch}" (now at ${shortSha}). The project's link to it was staged, not committed — commit the project when you're ready to share that.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

// Shows the actual commits about to be pushed — before, this was a plain
// "push this submodule?" confirm with no list, which was the whole complaint:
// you had no way to see what you were about to send anywhere.
// Push-submodule-workflow report, point 1: show the actual destination
// before pushing — branch, remote URL, local/remote SHA, ahead/behind — not
// just the commit list. Always reads the submodule's own currently
// checked-out branch (push_submodule_preview_inner's own doc comment), so
// this can never show a stale or wrong destination.
function pushPreviewHtml(preview) {
  const shortSha = preview.local_sha.slice(0, 8);
  const destination = preview.will_create_remote_branch ? `${shortSha} → origin/${esc(preview.branch)} <i>(new branch)</i>` : `${shortSha} → ${esc(preview.upstream || `origin/${preview.branch}`)}`;
  const counts = preview.will_create_remote_branch
    ? 'No origin branch yet — push will create it'
    : preview.ahead === 0 && preview.behind === 0
      ? `Local ${preview.branch} and ${preview.upstream || `origin/${preview.branch}`} are in sync`
      : `Local ${preview.branch}: ${preview.ahead} ahead, ${preview.behind} behind ${preview.upstream || `origin/${preview.branch}`}`;
  return `<div class="push-preview">
    <div class="push-preview-row"><span>Current branch</span><code>${esc(preview.branch)}</code></div>
    <div class="push-preview-row"><span>Destination</span><code>${destination}</code></div>
    <div class="push-preview-row"><span>Remote</span><code>${esc(preview.remote_url)}</code></div>
    <div class="push-preview-row"><span>Ahead / behind</span><span>${esc(counts)}</span></div>
    ${preview.blocked_reason ? `<div class="push-preview-row"><span>Status</span><strong>${esc(preview.blocked_reason)}</strong></div>` : ''}
  </div>`;
}

async function pushSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: pushed ${entry.name}`);
  $('#submodulePublishTitle').textContent = `Push ${entry.name}`;
  $('#submodulePublishCommits').innerHTML = '<div class="loading-row"><i class="spinner"></i>Checking what needs to be pushed…</div>';
  $('#submodulePublishSummary').textContent = 'Checking…'; $('#submodulePublishStatus').textContent = '';
  $('#confirmSubmodulePublish').disabled = true;
  $('#submodulePublishDialog').showModal();
  let commits = []; let preview = null;
  try {
    preview = await invoke('push_submodule_preview', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
  }
  catch (error) {
    $('#submodulePublishSummary').textContent = 'Push unavailable';
    $('#submodulePublishCommits').innerHTML = `<div class="publish-empty">${esc(String(error))}</div>`;
    return;
  }
  const pushState = submodulePushDialogState(preview);
  commits = pushState.commits;
  $('#submodulePublishCommits').innerHTML = pushPreviewHtml(preview) + (commits.map((commit, index) => `<div class="publish-commit"><span>${index + 1}</span><i></i><div><strong>${commitSubjectHtml(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div><b>WILL PUSH</b></div>`).join('') || `<div class="publish-empty">${esc(pushState.emptyMessage)}</div>`);
  $('#submodulePublishSummary').textContent = pushState.summary;
  $('#confirmSubmodulePublish').disabled = !pushState.canPush;

  const confirmed = await new Promise(resolve => {
    const dialog = $('#submodulePublishDialog');
    const onClose = () => { dialog.removeEventListener('close', onClose); resolve(dialog.returnValue === 'default'); };
    dialog.addEventListener('close', onClose);
  });
  if (!confirmed) { status('Push cancelled'); return; }

  try {
    status(`Pushing ${entry.name} to its remote…`, 'busy');
    const menuWasOpenForEntry = !refs.submoduleMenu.hidden && submoduleMenuEntry?.relative_path === entry.relative_path;
    const result = await invoke('push_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: state.currentPath });
    if (menuWasOpenForEntry) {
      submoduleMenuEntry = state.entries.find(item => item.relative_path === entry.relative_path) || submoduleMenuEntry;
      await refreshSubmoduleMenu();
    }
    const shortSha = (result?.revision || '').slice(0, 8);
    const successMsg = `${entry.name}: pushed to branch "${result?.branch}" on its remote (now at ${shortSha}). The project's link to it was staged, not committed — commit the project when you're ready to share that.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function fetchSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: fetched ${entry.name}`);
  try { status(`Fetching ${entry.name}…`, 'busy'); await invoke('fetch_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); status(`${entry.name}: fetched from remote`); showOperationToast(`${entry.name}: fetched from remote`, 'success'); }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function removeEntryFromGit(entry) {
  const label = entry.kind === 'submodule' ? 'submodule, its working folder and its Git link' : entry.kind === 'folder' ? 'folder and all tracked files inside it' : 'file';
  if (!await customConfirm(`REMOVE FROM GIT: delete ${label} locally and stage the deletion of "${entry.relative_path}".\n\nNothing is removed from the server until you commit and push. Continue?`, { title: 'Remove from Git', danger: true, okLabel: 'Remove' })) return;
  if (!invoke) return status(`Preview: remove ${entry.name} from Git`);
  try { status(`Removing ${entry.name} from Git…`, 'busy'); const folder = state.currentPath; await invoke('remove_git_path', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: folder }); state.changesScope = 'global'; refs.changesDrawer.classList.add('open'); const message = `${entry.name} deleted locally. Commit the staged deletion, then Publish/Push it to update the server.`; status(message); showOperationToast(message); }
  catch (error) { handleError(error); }
}

async function deleteEntry(entry, button) {
  if (!button?.dataset.confirmed) {
    button.dataset.confirmed = 'true';
    button.textContent = `Confirm delete “${entry.name}”`;
    button.classList.add('confirm-danger');
    const consequence = entry.tracked ? 'The deletion will appear in Workspace, ready for Commit and Push.' : 'This item exists only locally, so no server change will be created.';
    status(`Press “Confirm delete ${entry.name}” once more. ${consequence}`, 'error');
    setTimeout(() => {
      if (!button.isConnected || button.dataset.confirmed !== 'true') return;
      delete button.dataset.confirmed;
      button.textContent = 'Delete…';
      button.classList.remove('confirm-danger');
    }, 8000);
    return;
  }
  // Without this, the button stayed on "Confirm delete X" for the entire
  // operation — deleting a submodule in particular means removing its own
  // real .git directory (a genuine, possibly large object database, not
  // just a pointer), which can take a real amount of time on a large one —
  // with nothing distinguishing "still waiting for a second click" from
  // "already working on it". A failure used to also leave the button
  // permanently disabled with no way to retry, since disabled was never
  // reset back on that path — restored in `finally` regardless of outcome.
  button.disabled = true; button.textContent = `Deleting “${entry.name}”…`;
  if (!invoke) { button.disabled = false; return status(`Preview: delete ${entry.name}`); }
  try { status(`Deleting ${entry.name} locally…`, 'busy'); const folder = state.currentPath; await invoke(entry.tracked ? 'remove_git_path' : 'delete_local_path', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: folder }); if (entry.tracked) { state.changesScope = 'global'; refs.changesDrawer.classList.add('open'); } const message = entry.tracked ? `${entry.name} deleted locally. Its deletion is in Workspace; commit it, then Push to update the server.` : `${entry.name} deleted locally. It was local-only, so no commit or push is needed.`; status(message); showOperationToast(message); }
  catch (error) { status(String(error), 'error'); showOperationToast(String(error), 'error'); }
  finally {
    // Only touch the button if it's still the one showing — a successful
    // delete already replaced/re-rendered the details panel by this point,
    // so `button` may no longer be in the document at all.
    if (button.isConnected) { button.disabled = false; delete button.dataset.confirmed; button.textContent = 'Delete…'; button.classList.remove('confirm-danger'); }
  }
}

async function compareEntryWithRemote(entry) {
  // Compare & Sync now opens on the Local Drive tab by default. Without
  // forcing Git mode here too, this landed the user on that unrelated
  // two-independent-folders panel while the actual git-compare data it
  // had just fetched sat hidden behind it — "took me to Compare & Sync and
  // didn't compare the file" from the outside, even though the row
  // lookup below did succeed.
  state.commanderFocus = entry.relative_path; state.commanderPath = entry.relative_path.split('/').slice(0, -1).join('/'); state.view = 'commander'; state.compareMode = 'git'; state.commanderRows = []; render();
  await openCommanderDirectory(state.commanderPath);
  const row = state.commanderRows.find(item => item.relative_path === entry.relative_path);
  if (row?.local?.kind === 'file' && row.remote?.kind === 'file') openFileCompare(row); else status('This file is not available on both local and selected remote', 'error');
}
async function runEntryFileAction(entry, action) {
  const explanations = { head: 'RESTORE: this permanently discards ALL uncommitted edits in this file. The file on disk will become identical to the last commit (HEAD). This cannot be undone. Continue?', stage: 'Add this file to the staging area?', unstage: 'UNSTAGE: remove this file from staging. Your edits on disk are kept exactly as they are — only the staging entry is removed. Continue?' };
  const successMessages = { head: `${entry.name}: restored from the last commit (HEAD) — edits on disk were discarded.`, stage: `${entry.name}: staged. It will be included in the next commit.`, unstage: `${entry.name}: unstaged — removed from staging, edits on disk were kept unchanged.` };
  const titles = { head: 'Restore file', stage: 'Stage file', unstage: 'Unstage file' };
  if (!await customConfirm(explanations[action], { title: titles[action], danger: action === 'head', okLabel: action === 'head' ? 'Discard everything' : 'Continue' })) return;
  if (!invoke) { status(`Preview: ${action} ${entry.name}`); showOperationToast(`Preview: ${action} ${entry.name}`); return; }
  try {
    if (action === 'head') await invoke('restore_file', { repositoryPath: state.repository.path, relativePath: entry.relative_path, sourceRef: 'HEAD' }); else await invoke(action === 'stage' ? 'stage_files' : 'unstage_files', { path: state.repository.path, files: [entry.relative_path] });
    // Stage/unstage/restore-from-HEAD only ever change status and this one
    // file's content — never branches, history, stashes, or a submodule
    // sync pass, so a full loadRepository isn't needed to reflect it.
    await refreshStatusAndFolder(state.repository.path, state.currentPath);
    if (state.selectedEntry?.relative_path === entry.relative_path) await selectEntry(entry.relative_path);
    status(successMessages[action]); showOperationToast(successMessages[action], 'success');
  } catch (error) { const message = handleError(error); showOperationToast(`Failed: ${message}`, 'error'); }
}

async function openEditor(entry) {
  state.editingPath = entry.relative_path; state.editorOriginal = ''; refs.editorTitle.textContent = `Edit ${entry.name}`; refs.editorPath.textContent = entry.relative_path; refs.editorContent.value = 'Loading…'; refs.editorDialog.showModal();
  if (!invoke) { refs.editorContent.value = 'Preview editor\n'; state.editorOriginal = refs.editorContent.value; addEditorBlameHints(); return; }
  try { const file = await invoke('read_text_file', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); refs.editorContent.value = file.content; state.editorOriginal = file.content; updateEditorSaveState(); refs.editorContent.focus(); addEditorBlameHints(); }
  catch (error) { refs.editorDialog.close(); handleError(error); }
}

function addEditorBlameHints() {
  if (state.editingConflict) return;
  setTimeout(() => {
    const content = refs.editorContent;
    const lines = content.value.split('\n');
    const hints = lines.map((line, i) => `<span data-line="${i}" data-tooltip="Loading blame info…" style="color:#6b7f96;font-size:10px;">${String(i+1).padStart(4)}</span>`).join('\n');

    if (!invoke) return;
    try {
      invoke('file_blame', { repositoryPath: state.repository.path, relativePath: state.editingPath })
        .then(blame => {
          const blameByLine = {};
          if (blame && blame.lines) {
            blame.lines.forEach((info, i) => {
              blameByLine[i] = info;
            });
          }
          document.querySelectorAll('[data-line]').forEach(lineEl => {
            const lineNum = parseInt(lineEl.dataset.line);
            const info = blameByLine[lineNum];
            if (info) {
              const tooltip = `${info.author || '?'} · ${info.date || '?'} · ${info.message || '?'}`;
              lineEl.setAttribute('data-tooltip', tooltip);
            }
          });
        })
        .catch(() => {});
    } catch (e) {}
  }, 300);
}

async function saveEditor(event) {
  event?.preventDefault();
  if (state.editingConflict) {
    const { target, path } = state.editingConflict;
    const joined = target.targetPath ? `${target.targetPath}/${path}` : path;
    try {
      status(`Saving ${path}…`, 'busy');
      await invoke('write_text_file', { repositoryPath: conflictRepositoryPath(target), relativePath: joined, content: refs.editorContent.value });
      await invoke('resolve_conflict', { repositoryPath: conflictRepositoryPath(target), targetPath: target.targetPath, relativePath: path, resolution: 'manual' });
      refs.editorDialog.close(); state.editingConflict = null;
      status(`${path}: marked resolved.`); showOperationToast(`${path}: marked resolved.`, 'success');
      await refreshConflictsDialog(target);
    } catch (error) { handleError(error); }
    return;
  }
  const folder = state.currentPath;
  try { status(`Saving ${state.editingPath}…`, 'busy'); await saveEditorContent(); refs.editorDialog.close(); directoryCache.clear(); await loadRepository(state.repository.path, { reopenPath: folder }); status(`Saved ${state.editingPath}`); }
  catch (error) { handleError(error); }
}

async function saveEditorContent() { if (!invoke) { state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); return; } await invoke('write_text_file', { repositoryPath: state.repository.path, relativePath: state.editingPath, content: refs.editorContent.value }); state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); }
function updateEditorSaveState() { const dirty = refs.editorContent.value !== state.editorOriginal; $('#editorSaveState').textContent = dirty ? 'Unsaved local edits' : 'Saved locally · UTF-8 · maximum 2 MB'; $('#editorSaveState').classList.toggle('warning', dirty); }

// Deliberately never touches state.repository/branches/commits/changes/
// stashes — those stay the parent repository throughout, so Explorer,
// Commander, Remotes and the breadcrumb are unaffected by having a
// submodule's graph open. The submodule's own data lives only in
// state.submoduleGraph, read exclusively through activeGraphData() (see its
// own comment) by the graph view's own rendering code.
// Bumped by every openSubmoduleGraph call and by closeSubmoduleGraph — a
// response belonging to an older generation (a different submodule opened
// right after this one, or the user having left the graph view entirely
// before this request came back) must never overwrite whatever's on screen
// now. Without this, opening submodule A then quickly submodule B, with A's
// backend call happening to finish after B's, would silently replace B's
// graph with A's data.
let submoduleGraphGeneration = 0;

async function openSubmoduleGraph(entry) {
  clearDetails('Select a submodule commit');
  // Captured immutably, synchronously, before any await — entry (and
  // state.repository) could otherwise change while this is in flight, and
  // this request must keep addressing exactly the submodule it was asked
  // for, in exactly the parent repository it was asked from.
  const parentRepositoryPath = state.repository?.path;
  const relativePath = entry.relative_path;
  const name = entry.name;
  const generation = ++submoduleGraphGeneration;
  jsPerfLog(`openSubmoduleGraph request (parent=${anonymizeForLog(parentRepositoryPath)}, relativePath=${relativePath})`, 0);
  if (!invoke) { state.submoduleGraph = { name, parentRepositoryPath, relativePath, repository: { path: '', name, current_branch: '' }, branches: [], commits: [], changes: [], stashes: [], primaryBranch: null }; state.view = 'graph'; render(); return; }
  try {
    const data = await invoke('submodule_repository', { repositoryPath: parentRepositoryPath, relativePath });
    if (generation !== submoduleGraphGeneration) { jsPerfLog(`openSubmoduleGraph IGNORED (superseded — generation was ${generation}, now ${submoduleGraphGeneration})`, 0); return; }
    // Belt-and-suspenders on top of the backend's own guarantee (submodule_repository
    // now refuses, rather than silently falling back to the parent, when the
    // submodule's own Git metadata is missing — see internal_submodule_repository):
    // never apply a result whose repository.path isn't genuinely the submodule's
    // own, distinct from the parent it was requested from.
    const resultPath = data.repository?.path || '';
    if (!resultPath || resultPath === parentRepositoryPath) {
      jsPerfLog(`openSubmoduleGraph IGNORED (result path did not resolve to a distinct submodule repository)`, 0);
      handleError('Could not open this submodule\'s own repository — its Git metadata may be missing or invalid. Initialize/reset the submodule first.');
      return;
    }
    // Temporary, deliberately verbose diagnostic for the "does the graph
    // ever show the wrong submodule" report — same shape as the backend's
    // own log line, so the two can be compared directly: a real
    // cross-contamination bug would show two different relativePath
    // requests resolving to the same HEAD/commit OIDs here.
    const firstThree = (data.commits || []).slice(0, 3).map(c => `${(c.id || '').slice(0, 8)}:${c.subject}`).join(' | ');
    // Message C, point 9: requested path, resolved workdir+gitdir,
    // repository identity, current ref, and the semantic shape of the
    // history walk that produced this — a genuine cross-contamination bug
    // (this resolving to the parent, or to a sibling submodule) would show
    // up here as a gitdir/head mismatch against what the backend's own
    // matching log line (submodule_repository, repository.rs) reports for
    // the same request.
    const refDescription = data.repository?.head_detached ? `detached at ${(data.repository.head_oid || '').slice(0, 8)}` : (data.repository?.current_branch || '(none)');
    jsPerfLog(`openSubmoduleGraph APPLIED (generation=${generation}, requested_relative_path=${relativePath}, name=${name}, resolved_workdir=${anonymizeForLog(resultPath)}, resolved_gitdir=${anonymizeForLog(data.repository?.gitdir)}, url=${data.repository?.submodule_url || '(none)'}, ref=${refDescription}, head=${(data.repository?.head_oid || '').slice(0, 8)}, history_walk=revwalk(seed=heads+remotes+tags, sort=topological+time, window=${GRAPH_COMMIT_WINDOW}), branches=${(data.branches || []).length}, commits=${(data.commits || []).length}, first_commits=[${firstThree}])`, 0);
    state.submoduleGraph = { name, parentRepositoryPath, relativePath, repository: data.repository, branches: data.branches, commits: data.commits, changes: data.changes, stashes: data.stashes || [], primaryBranch: null, commits_truncated: !!data.commits_truncated };
    state.view = 'graph';
    jsPerfLog(`openSubmoduleGraph before renderGraph (generation=${generation}, submoduleGraph.name=${state.submoduleGraph.name}, submoduleGraph.repository.path=${anonymizeForLog(state.submoduleGraph.repository.path)}, submoduleGraph.commits.length=${state.submoduleGraph.commits.length})`, 0);
    render();
  }
  catch (error) { if (generation === submoduleGraphGeneration) { jsPerfLog(`openSubmoduleGraph ERROR: ${String(error)}`, 0); handleError(error); } }
}

// Closes the submodule context (see closeSubmoduleGraph below for the same
// thing without forcing the Explorer view) and returns to Project Explorer —
// the only difference from just navigating to Explorer directly is that this
// remembers state.currentPath is already correct (it was never touched) so
// no repository reload is needed, just a folder repaint.
function leaveSubmoduleGraph() { if (!state.submoduleGraph) return; state.submoduleGraph = null; state.view = 'explorer'; render(); openDirectory(state.currentPath, { force: true }); }

// Any navigation away from the graph view that ISN'T the explicit "Back to
// parent repository" button above — Project Explorer, Compare & Sync,
// Remotes, or opening a different repository entirely — must still safely
// close the submodule context so it can never be silently combined with
// whatever's navigated to next (a stale "Back to parent" later restoring a
// repository that isn't even open anymore, or a leftover primaryBranch
// picked for a submodule bleeding into the parent's own Branch Map).
function closeSubmoduleGraph() { state.submoduleGraph = null; submoduleGraphGeneration++; }

// The one explicit source of truth for every repository-sensitive UI
// surface outside the graph view itself — the Branches sidebar and every
// branch action (switch/create/rename/delete/menu) — mirroring
// activeGraphData()'s own resolution exactly (same state.submoduleGraph
// check), so the sidebar and the graph header can never disagree about
// which repository is active. Fixes the reported bug directly: the
// sidebar used to always render state.branches (the parent's) and every
// branch action used to always target state.repository.path, regardless
// of whether a submodule's own Branch Map was open.
function activeRepositoryContext() {
  if (state.submoduleGraph) {
    const g = state.submoduleGraph;
    return {
      isSubmodule: true, path: g.repository.path, parentPath: g.parentRepositoryPath, relativePath: g.relativePath,
      name: g.repository.name, branches: g.branches || [], currentBranch: g.repository.current_branch,
      headDetached: !!g.repository.head_detached, headOid: g.repository.head_oid,
    };
  }
  return {
    isSubmodule: false, path: state.repository?.path, parentPath: null, relativePath: null,
    name: state.repository?.name, branches: state.branches || [], currentBranch: state.repository?.current_branch,
    headDetached: !!state.repository?.head_detached, headOid: state.repository?.head_oid,
  };
}

function renderBranches() {
  const context = activeRepositoryContext();
  const { rows, detached, detachedAt } = selectBranchRows(context);
  // Point 2 of the report: a detached submodule (or parent) must show its
  // own explicit "Detached HEAD at <sha>" row/status — never leave the
  // sidebar merely *not* highlighting anything, which reads as "nothing is
  // checked out" rather than the real, different state "on a specific
  // commit, no branch".
  const detachedRow = detached ? `<div class="branch-row detached-head-row active"><span class="branch-bullet detached"></span><span class="branch-name">Detached HEAD at ${esc((detachedAt || '').slice(0, 8))}</span></div>` : '';
  // Message E, point 9: this bullet used to be colored by the row's index
  // into the same `palette` the graph's lanes are colored from
  // (palette[index % palette.length]) — two entirely unrelated indices
  // sharing one finite color set, which visually implied a branch-to-lane
  // correspondence that was never real (a lane is a topology-driven
  // rendering slot the graph model itself reuses across unrelated commits —
  // see buildGraphModel's own doc comment in graph-model.js — not a stable
  // per-branch identity a sidebar row could legitimately mirror). Left
  // neutral (the bullet's plain CSS default) instead of inventing a mapping
  // that would just be a different false correspondence.
  refs.branches.innerHTML = detachedRow + rows.map(branch => `<div class="branch-row ${branch.isHead ? 'active' : ''} ${branch.remote ? 'remote-branch' : 'local-branch'} ${branch.name === 'origin/main' ? 'primary-remote' : ''}" data-branch="${esc(branch.name)}" data-is-remote="${branch.remote ? 'true' : 'false'}">
    <span class="branch-bullet"></span>
    <span class="branch-name">${esc(branch.name)}</span>
    <div style="display:flex;gap:6px;margin-left:auto;">
      ${branch.isHead ? '<small>HEAD</small>' : branch.remote ? `<small>${branch.name === 'origin/main' ? 'PRIMARY REMOTE' : 'REMOTE'}</small>` : `<button class="switch-branch" data-branch="${esc(branch.name)}" title="Switch to ${esc(branch.name)}">↔</button>`}
      ${!branch.remote && !branch.isHead ? `<button class="branch-menu" data-branch="${esc(branch.name)}" title="Branch actions" style="width:20px;height:20px;padding:0;font-size:14px;border-radius:3px;">⋮</button>` : ''}
    </div>
  </div>`).join('') || (detached ? '' : '<div class="empty-change">No branches</div>');
  refs.branches.querySelectorAll('.switch-branch').forEach(button => button.addEventListener('click', event => { event.stopPropagation(); switchBranch(button.dataset.branch, button); }));
  refs.branches.querySelectorAll('.branch-menu').forEach(button => button.addEventListener('click', event => { event.stopPropagation(); showBranchMenu(button.dataset.branch, event); }));
}

// Message D, point 5: the "SUBMODULE PULL REQUEST" section exists at all
// only while a submodule's own Branch Map is actually open — never shown or
// queried the rest of the time, and always plainly labeled with
// *which* submodule so it can never be mistaken for the project's own PR
// section right above it.
function renderSubmodulePrHeading() {
  const active = !!state.submoduleGraph;
  refs.toggleSubmodulePrStatus.hidden = !active;
  refs.submodulePrStatusPanel.hidden = !active;
  if (active) { refs.submodulePrStatusLabel.textContent = `SUBMODULE PULL REQUEST · ${state.submoduleGraph.name}`; }
  else { submodulePrStatusPanel?.setExpanded(false); refs.submodulePrStatusArrow.textContent = '▸'; }
}

function showBranchMenu(branchName, event) {
  const menu = `<div style="position:fixed;top:${event.clientY}px;left:${event.clientX}px;z-index:100;background:#1a2530;border:1px solid #465563;border-radius:6px;box-shadow:0 8px 24px #0008;">
    <button style="display:block;width:100%;padding:8px 14px;text-align:left;border:0;background:transparent;color:#d8e5f0;cursor:pointer;font-size:12px;" data-action="rename">Rename</button>
    <button style="display:block;width:100%;padding:8px 14px;text-align:left;border:0;background:transparent;color:#d8e5f0;cursor:pointer;font-size:12px;border-top:1px solid #465563;" data-action="delete">Delete</button>
  </div>`;
  const menuEl = document.createElement('div');
  menuEl.innerHTML = menu;
  const menuContainer = menuEl.firstElementChild;
  document.body.appendChild(menuContainer);
  menuContainer.querySelectorAll('[data-action]').forEach(btn => {
    btn.addEventListener('click', async () => {
      document.body.removeChild(menuContainer);
      const action = btn.dataset.action;
      if (action === 'rename') { const newName = await customPrompt(`Rename branch "${branchName}" to:`, branchName, { title: 'Rename branch' }); if (newName && newName !== branchName) await renameBranch(branchName, newName); }
      else if (action === 'delete') { if (await customConfirm(`Delete branch "${branchName}"?`, { title: 'Delete branch', danger: true, okLabel: 'Delete' })) await deleteBranch(branchName); }
    });
  });
  document.addEventListener('click', (e) => { if (!menuContainer.contains(e.target)) document.body.removeChild(menuContainer); }, { once: true });
}

// rename_branch/delete_branch are already fully generic on the backend —
// whatever repository_path they're given, parent or submodule's own — so
// the fix here is entirely about which path/refresh path this sends,
// never a backend change. Never state.repository.path unconditionally.
async function renameBranch(oldName, newName) {
  const context = activeRepositoryContext();
  if (!context.path) return;
  if (!invoke) { status(`Preview: renamed ${oldName} to ${newName}`); return; }
  try {
    status('Renaming branch…', 'busy');
    await invoke('rename_branch', { repositoryPath: context.path, oldName, newName });
    if (context.isSubmodule) await openSubmoduleGraph({ relative_path: context.relativePath, name: context.name });
    else await loadRepository(context.path, { keepPath: true });
    status(`Branch renamed to ${newName}`);
  }
  catch (error) { handleError(error); }
}

async function deleteBranch(branchName) {
  const context = activeRepositoryContext();
  if (!context.path) return;
  if (!invoke) { status(`Preview: deleted ${branchName}`); return; }
  try {
    status('Deleting branch…', 'busy');
    await invoke('delete_branch', { repositoryPath: context.path, branchName });
    if (context.isSubmodule) await openSubmoduleGraph({ relative_path: context.relativePath, name: context.name });
    else await loadRepository(context.path, { keepPath: true });
    status(`Branch ${branchName} deleted`);
  }
  catch (error) { handleError(error); }
}

function renderRemotes() {
  refs.remoteCards.innerHTML = state.remotes.map(remote => `<article class="remote-card"><div class="remote-symbol">◎</div><div><h2>${esc(remote.name)}</h2><span>FETCH URL</span><code>${esc(remote.fetch_url)}</code><span>PUSH URL</span><code>${esc(remote.push_url)}</code></div><button data-fetch-remote="${esc(remote.name)}">Fetch now</button></article>`).join('') || '<div class="remote-empty">No remote is configured for this repository.</div>';
  refs.remoteCards.querySelectorAll('[data-fetch-remote]').forEach(button => button.addEventListener('click', () => fetchRemote(button.dataset.fetchRemote, button)));
}

const loadRemotesGuard = createRequestGuard();
async function loadRemotes() {
  state.view = 'remotes'; refs.search.value = ''; clearDetails('Remote configuration');
  const stillCurrent = loadRemotesGuard();
  if (invoke) {
    try { const remotes = await invoke('list_remotes', { repositoryPath: state.repository.path }); if (!stillCurrent()) return; state.remotes = remotes; }
    catch (error) { if (stillCurrent()) handleError(error); }
  } else state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }];
  if (stillCurrent()) render();
}
async function fetchRemote(name, button = null) {
  const finishButton = beginButtonOperation(button, 'Fetching…');
  try { status(`Fetching ${name}…`, 'busy'); await invoke('fetch_remote', { repositoryPath: state.repository.path, remote: name }); await loadRepository(state.repository.path, { keepPath: true }); await loadRemotes(); status(`${name} updated`); }
  catch (error) { handleError(error); }
  finally { finishButton(); }
}
async function fetchAllRemotes(button = null) {
  if (!state.repository) return;
  if (!invoke) return status('Preview: fetched all remotes');
  const finishButton = beginButtonOperation(button, 'Fetching…');
  const startedAt = performance.now();
  jsPerfLog('fetchAllRemotes START', 0);
  try { status('Fetching every remote…', 'busy'); await invoke('fetch_all_remotes', { repositoryPath: state.repository.path }); await loadRepository(state.repository.path, { keepPath: true }); await loadRemotes(); const msg = `${state.remotes.length} remote${state.remotes.length === 1 ? '' : 's'} updated`; status(msg); showOperationToast(msg, 'success'); jsPerfLog('fetchAllRemotes SUCCESS', performance.now() - startedAt); }
  catch (error) { jsPerfLog(`fetchAllRemotes ERROR: ${String(error)}`, performance.now() - startedAt); handleError(error); }
  finally { finishButton(); }
}
function limitedFetchProjectDetails(label, list = []) {
  if (!list.length) return '';
  const visible = list.slice(0, 4).map(item => `- ${item}`).join('\n');
  const extra = list.length > 4 ? `\n- …and ${list.length - 4} more` : '';
  return `\n${label}:\n${visible}${extra}`;
}
async function fetchProjectAndSubmodules(button = null) {
  if (!state.repository) return;
  if (!invoke) return status('Preview: fetched parent repository and submodules');
  const finishButton = beginButtonOperation(button, 'Fetching…');
  const startedAt = performance.now();
  jsPerfLog('fetchProjectAndSubmodules START', 0);
  try {
    status('Fetching parent repository and initialized submodules…', 'busy');
    const result = await invoke('fetch_project', { repositoryPath: state.repository.path });
    await loadRepository(state.repository.path, { keepPath: true });
    await loadRemotes();
    const parentText = result.parent_fetched ? 'Parent updated' : 'Parent not updated';
    const submoduleText = `${result.submodules_fetched}/${result.submodules_total} submodule${result.submodules_total === 1 ? '' : 's'} fetched`;
    const skippedText = result.submodules_skipped ? ` · ${result.submodules_skipped} skipped` : '';
    const msg = `${parentText} · ${submoduleText}${skippedText}`;
    const details = `${limitedFetchProjectDetails('Skipped', result.warnings)}${limitedFetchProjectDetails('Errors', result.errors)}`;
    status(msg, result.errors?.length ? 'error' : '');
    showOperationToast(`${msg}${details}`, result.errors?.length ? 'error' : 'success');
    jsPerfLog(`fetchProjectAndSubmodules SUCCESS (parent=${result.parent_fetched}, submodules=${result.submodules_fetched}/${result.submodules_total}, skipped=${result.submodules_skipped}, errors=${result.errors?.length || 0})`, performance.now() - startedAt);
  }
  catch (error) { jsPerfLog(`fetchProjectAndSubmodules ERROR: ${String(error)}`, performance.now() - startedAt); handleError(error); }
  finally { finishButton(); }
}

let stashDialogContext = null;

function stashBoundaryForCurrentView(path = state.currentPath) {
  if (state.submoduleGraph) return state.submoduleGraph.relativePath;
  return submoduleBoundaryFor(path) || state.activeSubmodule?.path || null;
}

async function resolveStashContext(path = state.currentPath, explicitSubmodulePath = null) {
  if (!state.repository) return null;
  const relativePath = explicitSubmodulePath || stashBoundaryForCurrentView(path);
  if (!invoke) {
    const name = relativePath ? relativePath.split('/').pop() : state.repository.name;
    return { repository_path: state.repository.path, repository_name: name, current_branch: state.repository.current_branch, is_submodule: Boolean(relativePath), relative_path: relativePath, stashes: [] };
  }
  return relativePath
    ? invoke('list_submodule_stashes', { repositoryPath: state.repository.path, relativePath })
    : invoke('list_stashes', { repositoryPath: state.repository.path });
}

async function refreshAfterStashContext(context) {
  if (!context || !state.repository) return;
  directoryCache.clear();
  if (context.is_submodule && state.submoduleGraph?.repository?.path === context.repository_path) {
    await openSubmoduleGraph({ relative_path: context.relative_path, name: context.repository_name });
    return;
  }
  await loadRepository(state.repository.path, { reopenPath: state.currentPath });
}

async function stashWork() {
  if (!state.repository) return;
  try {
    const context = await resolveStashContext(); if (!context) return;
    if (!invoke) { status(`Preview: changes stashed in ${context.repository_name}`); return; }
    status(`Saving changes in ${context.repository_name}…`, 'busy');
    await invoke('stash_changes', { repositoryPath: context.repository_path });
    if (!context.is_submodule) refs.commitMessage.value = '';
    await refreshAfterStashContext(context);
    const message = `Changes in ${context.repository_name} were stashed. Open “Stashes” while browsing this same repository to restore them.`;
    status(message); showOperationToast(message, 'success'); updateStashUI();
  } catch (error) { handleError(error); }
}

async function stashSubmoduleWork(entry) {
  if (!state.repository || entry.kind !== 'submodule') return;
  try {
    const context = await resolveStashContext('', entry.relative_path); if (!context) return;
    if (!invoke) return status(`Preview: changes stashed in ${entry.name}`);
    status(`Saving changes inside ${entry.name}…`, 'busy');
    await invoke('stash_changes', { repositoryPath: context.repository_path });
    await refreshAfterStashContext(context);
    const message = `Uncommitted changes inside ${entry.name} were stashed. The parent project was not changed.`;
    status(message); showOperationToast(message, 'success'); updateStashUI();
  } catch (error) { handleError(error); }
}

function openStashesDialogForContext(context) {
  stashDialogContext = context;
  $('#stashesTitle').textContent = `Stashes · ${context.repository_name}`;
  $('#stashesSubtitle').textContent = context.is_submodule
    ? `Saved changes inside this submodule only. Restore copies one file and keeps this stash as a backup.`
    : `Saved changes in the main project only. Restore copies one file and keeps this stash as a backup.`;
  refs.stashesDialog.showModal();
  renderStashesList();
}

async function showSubmoduleStashes(entry) {
  if (!state.repository || entry.kind !== 'submodule') return;
  try { const context = await resolveStashContext('', entry.relative_path); if (context) openStashesDialogForContext(context); }
  catch (error) { handleError(error); }
}

async function stashOneFile(path) {
  if (!state.repository) return;
  if (!invoke) { status(`Preview: ${path} stashed`); return; }
  try {
    const context = await resolveStashContext(path, submoduleBoundaryFor(path)); if (!context) return;
    status(`Setting ${path} aside…`, 'busy');
    await invoke('stash_file', { repositoryPath: state.repository.path, relativePath: path });
    await refreshAfterStashContext(context); updateStashUI();
    // The stashed file just went back to its committed state, so the details
    // panel (if it's the one showing) needs to drop its now-stale "Modified"
    // banner and action buttons rather than keep them around.
    if (state.selectedEntry?.relative_path === path) await selectEntry(path);
    const message = `${path} was moved to ${context.repository_name}'s stash. Open “Stashes” in that repository to restore it.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) { handleError(error); }
}

// "Stashes" opens the full list rather than blindly restoring whatever
// happens to be most recent — with per-file stash making it common to have
// several at once, silently guessing which one you meant was the actual
// complaint ("I can't find the list of stashes anywhere").
async function popStash() {
  if (!state.repository) return;
  try {
    const context = await resolveStashContext(); if (!context) return;
    openStashesDialogForContext(context);
  } catch (error) { handleError(error); }
}

// Bumped on every render — a file-list fetch launched by an older render
// that resolves after a newer one has already started is discarded instead
// of writing stale data into the dialog after a restore, drop, or refresh.
let stashesRenderGeneration = 0;

async function refreshStashesList() {
  if (!state.repository || !stashDialogContext) return;
  try {
    const data = await invoke('list_stashes', { repositoryPath: stashDialogContext.repository_path });
    stashDialogContext = { ...stashDialogContext, stashes: data.stashes, current_branch: data.current_branch };
    if (!stashDialogContext.is_submodule) { state.stashes = data.stashes; state.hasStash = data.stashes.length > 0; }
    else if (state.submoduleGraph?.repository?.path === stashDialogContext.repository_path) state.submoduleGraph.stashes = data.stashes;
    updateStashUI();
  }
  catch (error) { handleError(error); }
  renderStashesList();
}

function renderStashesList() {
  const generation = ++stashesRenderGeneration;
  const stashes = stashDialogContext?.stashes || [];
  refs.stashesList.innerHTML = stashes.map(stash => `<div class="conflict-row stash-entry-row" data-stash-index="${stash.index}">
    <div class="conflict-head"><span class="conflict-path">stash@{${stash.index}}: ${esc(stash.message.replace(/^WIP on [^:]+:\s*[0-9a-f]+\s*/, 'WIP on ') || 'Saved changes')}</span><button class="stash-drop-icon" data-drop-stash="${stash.index}" title="Drop this entire stash — discards everything left in it, for good">✕</button></div>
    <div class="stash-file-list" data-stash-file-list="${stash.index}"><i class="spinner"></i></div>
  </div>`).join('') || '<div class="empty-change">Nothing set aside right now.</div>';
  stashes.forEach(stash => loadStashFileList(stash.index, generation));
  refs.stashesList.querySelectorAll('[data-drop-stash]').forEach(button => button.addEventListener('click', () => dropStashEntry(Number(button.dataset.dropStash))));
}

// A plain list — one row per file. Restoring copies only that path back to the
// working tree and deliberately keeps the stash as a safety backup; dropping
// the backup remains a separate, explicit destructive action.
function loadStashFileList(stashIndex, generation) {
  const fileList = refs.stashesList.querySelector(`[data-stash-file-list="${stashIndex}"]`);
  if (!fileList) return;
  const render = files => {
    if (generation !== stashesRenderGeneration) return; // a newer render has since started — this response is stale
    fileList.innerHTML = files.length ? files.map(file => `<div class="stash-file-row" data-stash-file-path="${esc(file)}">
      <span class="stash-file">${esc(file)}</span>
      <button class="stash-restore-icon" data-restore-file="${esc(file)}" data-restore-stash="${stashIndex}" title="Restore only this file; keep the stash as a backup">⇈</button>
    </div>`).join('') : '<div class="empty-change">Nothing left in this stash</div>';
    fileList.querySelectorAll('[data-restore-file]').forEach(button => button.addEventListener('click', () => restoreOneStashFile(Number(button.dataset.restoreStash), button.dataset.restoreFile)));
  };
  if (!invoke) { render(['preview.txt']); return; }
  invoke('stash_entry_files', { repositoryPath: stashDialogContext.repository_path, stashIndex })
    .then(render)
    .catch(error => { if (generation === stashesRenderGeneration) fileList.innerHTML = `<div class="stash-file-row">${esc(String(error))}</div>`; });
}

async function restoreOneStashFile(index, path) {
  if (!invoke || !stashDialogContext) { status(`Preview: ${path} restored`); return; }
  const context = { ...stashDialogContext };
  const row = refs.stashesList.querySelector(`[data-stash-index="${index}"] [data-stash-file-path="${CSS.escape(path)}"]`);
  const button = row?.querySelector('[data-restore-file]'); if (button) { button.disabled = true; button.innerHTML = '<i class="spinner"></i>'; }
  try {
    await invoke('restore_stash_paths', { repositoryPath: context.repository_path, stashIndex: index, paths: [path] });
    await refreshAfterStashContext(context);
    // The entry intentionally remains visible: the immutable stash is retained
    // as a backup until the user explicitly drops it.
    await refreshStashesList();
    const msg = `${path} restored; the stash was kept as a backup.`;
    status(msg); showOperationToast(msg, 'success');
  } catch (error) { handleError(error); if (button) { button.disabled = false; button.textContent = '⇈'; } }
}

async function dropStashEntry(index) {
  if (!await customConfirm('Discard this stash? Its changes are gone for good — this cannot be undone.', { title: 'Drop stash', danger: true, okLabel: 'Drop' })) return;
  if (!invoke || !stashDialogContext) { status('Preview: stash dropped'); return; }
  const context = { ...stashDialogContext };
  try {
    await invoke('drop_stash', { repositoryPath: context.repository_path, stashIndex: index });
    await refreshAfterStashContext(context);
    await refreshStashesList();
    status(`Stash dropped from ${context.repository_name}`);
  } catch (error) { handleError(error); }
}

function updateStashUI() {
  const relativePath = stashBoundaryForCurrentView();
  const name = state.submoduleGraph?.repository?.name || relativePath?.split('/').pop() || state.repository?.name || 'project';
  const isSubmodule = Boolean(relativePath);
  // This is intentionally repository-scoped, not folder-scoped: it stashes
  // the main project or the submodule currently being browsed. Keep the
  // short visible label generic ("Stash changes") and explain the exact scope in
  // the tooltip so it cannot be mistaken for a folder-only operation.
  $('#stashWork').title = `Stash changes in the current repository — sets aside uncommitted changes only in ${isSubmodule ? `the ${name} submodule` : 'the main project'} you are currently browsing (not just this one folder). A parent project and each submodule have separate stashes.`;
  // Same rule as every other button in that row (Commit folder, Add
  // submodule, History, Changes in folder) — without this it was the one
  // button left stranded alone in an otherwise-empty toolbar the moment you
  // left Explorer for Graph/Compare & Sync/Remotes. The Ctrl+Shift+S shortcut
  // still works everywhere regardless — that's calling stashWork() directly,
  // never gated on this element's visibility.
  // Stashing is exposed from the selected item/details panel and the
  // sidebar's Stashes section; the header should stay focused on navigation
  // and repository-level actions.
  $('#stashWork').hidden = true;
  $('#stashListSubtitle').textContent = isSubmodule ? `View saved changes in ${name}` : 'View saved project changes';
  $('#stashWork').disabled = !state.repository || Boolean(state.activeSubmodule && !state.activeSubmodule.statusReady);
  $('#popStash').disabled = !state.repository;
}

// ---- Git DAG / topology model -------------------------------------------
// reachableFrom/buildGraphModel used to live here; they moved to their own
// file, graph-model.js (loaded by index.html right before this script), so
// they can be exercised by a real, automated test suite (tests/graph-model.
// test.js, run with `node --test`) instead of relying on hand-tracing
// documented in a comment. Nothing else changed: both are still ordinary
// globals by the time this file runs, called exactly as before.

const LANE_WIDTH = 30;
// Mirrors the backend's own GRAPH_COMMIT_WINDOW — used only as the page size
// for "Load older" requests; the backend is always the actual source of
// truth for whether more history exists (commitsTruncated/has_more).
const GRAPH_COMMIT_WINDOW = 500;
const laneX = lane => LANE_WIDTH / 2 + lane * LANE_WIDTH;

// Point 5 of the rework: a subtle, collapsible legend at the base of the
// graph — <details>/<summary> gives the collapse behavior natively (same
// pattern the Help panel already uses), no JS needed for it. Built once as
// a constant, not per-render: it never depends on anything in `state`.
const GRAPH_LEGEND_HTML = `<details class="graph-legend">
  <summary>How to read this map</summary>
  <div class="graph-legend-grid">
    <span><i class="legend-glyph">●</i>Commit — A saved point in repository history</span>
    <span><i class="legend-glyph">◎</i>HEAD — The commit currently checked out</span>
    <span><i class="legend-glyph">⑂</i>Branch point — A common ancestor or lane transition</span>
    <span><i class="legend-glyph legend-tag">◆</i>TAG — A named release or version</span>
    <span><i class="legend-swatch kind-local_branch"></i>Local branch — Cyan rectangular label and solid tip ring</span>
    <span><i class="legend-swatch kind-remote_branch"></i>Remote branch — Rounded slate label and dashed tip ring</span>
    <span><i class="legend-swatch primary-remote"></i>origin/main — Red primary-remote marker</span>
    <span><i class="legend-line"></i>Line — A real parent relationship between commits</span>
    <span><i class="legend-swatch legend-merge"></i>MERGE — A commit with multiple parents</span>
    <span><i class="legend-glyph">…</i>Older history is available but not loaded</span>
  </div>
  <p class="graph-legend-note">Lane colors are visual guides only; they do not permanently identify a branch.</p>
</details>`;

// Turns selectRefBadges' pure selection (graph-model.js) into markup. Badge
// color is by *kind* now, not by lane — lanes/colors are exclusively about
// the graph's own topology (point 1 of the rework: never inferred as
// meaning anything about a specific branch), while a badge's color is
// exactly the opposite: a fixed, stable signal for what *kind* of ref it
// is, the same on every row it ever appears on. A tag badge also carries
// its own click handler (see renderGraph) separate from the row's —
// clicking a tag shows *that tag's* detail (lightweight/annotated, full
// name), clicking anywhere else on the row selects the commit.
const REF_BADGE_ICON = { tag: '◆ ' };
function refsBadges(refList, isHead, currentBranchName) {
  const { badges, overflowTags, overflowBranches } = selectRefBadges(refList, { isHead, currentBranchName });
  if (!badges.length && !overflowTags.length && !overflowBranches.length) return '';
  const badgeHtml = badges.map(badge => {
    if (badge.kind === 'tag') return `<b class="ref-pill kind-tag" data-tag-name="${esc(badge.name)}" data-tooltip="Click for tag details">${REF_BADGE_ICON.tag}${esc(badge.name)}</b>`;
    if (badge.kind === 'local_branch') return `<b class="ref-pill kind-local_branch branch-ref-pill" data-graph-ref-kind="local_branch" data-graph-ref-name="${esc(badge.name)}" data-tooltip="Local branch tip — right-click for actions"><i>⑂ BRANCH</i>${esc(badge.name)}</b>`;
    if (badge.kind === 'remote_branch') return `<b class="ref-pill kind-remote_branch branch-ref-pill ${badge.name === 'origin/main' ? 'primary-remote' : ''}" data-graph-ref-kind="remote_branch" data-graph-ref-name="${esc(badge.name)}" data-tooltip="${badge.name === 'origin/main' ? 'Primary origin branch tip' : 'Remote-tracking branch tip'} — right-click for actions"><i>${badge.name === 'origin/main' ? 'PRIMARY REMOTE' : 'REMOTE'}</i>${esc(badge.name)}</b>`;
    return `<b class="ref-pill kind-${badge.kind}">${esc(badge.name)}</b>`;
  }).join('');
  const tagOverflow = overflowTags.length ? `<b class="ref-pill kind-tag ref-pill-more" data-tooltip="${esc(overflowTags.map(t => t.name).join(', '))}">+${overflowTags.length} tags</b>` : '';
  const branchOverflow = overflowBranches.length ? `<b class="ref-pill ref-pill-more" data-tooltip="${esc(overflowBranches.map(b => b.name).join(', '))}">+${overflowBranches.length}</b>` : '';
  return `<span class="ref-pills">${badgeHtml}${tagOverflow}${branchOverflow}</span>`;
}

// The graph view renders one of two entirely separate repositories: the
// parent (state.repository/branches/commits/stashes, untouched by entering a
// submodule's graph) or, while state.submoduleGraph is set, that submodule's
// own data — kept in its own namespace specifically so Explorer, Commander,
// Remotes and the breadcrumb never see anything but the parent repository,
// no matter what's on screen in the graph view. Every read the graph needs
// goes through this one accessor instead of state.repository/branches/
// commits/stashes directly, so there's exactly one place that decides which
// repository is "active" for graph purposes.
function activeGraphData() {
  if (state.submoduleGraph) {
    const g = state.submoduleGraph;
    return { path: g.repository.path, name: g.repository.name, gitdir: g.repository.gitdir, submoduleUrl: g.repository.submodule_url, currentBranch: g.repository.current_branch, headOid: g.repository.head_oid, headDetached: !!g.repository.head_detached, branches: g.branches, commits: g.commits, stashes: g.stashes || [], commitsTruncated: !!g.commits_truncated };
  }
  return { path: state.repository?.path, name: state.repository?.name, gitdir: state.repository?.gitdir, submoduleUrl: undefined, currentBranch: state.repository?.current_branch, headOid: state.repository?.head_oid, headDetached: !!state.repository?.head_detached, branches: state.branches, commits: state.commits, stashes: state.stashes, commitsTruncated: !!state.commits_truncated };
}
// state.graphPrimaryBranch (the parent's own "Primary" picker choice) must
// never leak into a submodule's graph, or vice versa — each submodule (and
// the parent) keeps its own choice, isolated in state.submoduleGraph.primaryBranch.
function activeGraphPrimaryBranch() { return state.submoduleGraph ? state.submoduleGraph.primaryBranch : state.graphPrimaryBranch; }
function setActiveGraphPrimaryBranch(name) { if (state.submoduleGraph) state.submoduleGraph.primaryBranch = name; else state.graphPrimaryBranch = name; }

// The subtle All/Branches/Releases filter — same per-context isolation as
// the primary-branch picker above, and the same "dim, never remove" rule
// search already uses: filtering must never make buildGraphModel see a
// different (smaller) commit list, only change which rows *look* faded, so
// a filtered-out commit's edges stay exactly as real and unbroken as before.
function activeGraphRefFilter() { return (state.submoduleGraph ? state.submoduleGraph.refFilter : state.graphRefFilter) || 'all'; }
function setActiveGraphRefFilter(value) { if (state.submoduleGraph) state.submoduleGraph.refFilter = value; else state.graphRefFilter = value; }

// The one place that decodes activeGraphPrimaryBranch()'s stored value
// ("detached" / "branch:<name>" / "remote:<name>", or nothing yet — see the
// picker in renderGraph) into an actual {kind, name} — used by renderGraph
// itself and by ensureBranchDivergence's own staleness check below, so both
// always agree on what "primary" currently resolves to instead of the
// latter recomputing it with a different (and, after the picker started
// accepting detached HEAD and remote-tracking refs, no longer matching)
// scheme of its own.
function resolvePrimarySelection(g) {
  const localBranchNames = (g.branches || []).filter(b => !b.remote).map(b => b.name);
  const remoteBranchNames = (g.branches || []).filter(b => b.remote).map(b => b.name);
  const selection = activeGraphPrimaryBranch();
  if (selection === 'detached' && g.headDetached) return { kind: 'detached', name: null };
  if (selection?.startsWith('branch:') && localBranchNames.includes(selection.slice(7))) return { kind: 'branch', name: selection.slice(7) };
  if (selection?.startsWith('remote:') && remoteBranchNames.includes(selection.slice(7))) return { kind: 'remote', name: selection.slice(7) };
  if (g.headDetached) return { kind: 'detached', name: null };
  return { kind: 'branch', name: g.currentBranch };
}

// The 500-commit window load_repository/open_repository_fast return is never
// presented as "this is all of history" — activeGraphData().commitsTruncated
// (real, from the backend's own one-past-the-limit peek, never guessed from
// "exactly 500 came back") drives a "continues in older history" stub with
// an explicit, read-only "Load older" action; nothing here is ever fetched
// automatically. Read-only and side-effect-free beyond appending commits to
// whichever context (parent or submodule) actually asked for them.
let loadingOlderCommits = false;
async function loadOlderGraphCommits() {
  if (loadingOlderCommits || !invoke) return;
  const g = activeGraphData();
  if (!g.commits.length) return;
  const oldest = g.commits[g.commits.length - 1];
  // Identity, not just a path string — correctly distinguishes "still this
  // exact submodule graph" from "left it and came back" (a fresh object) or
  // "switched to a different submodule" (a different object), so a response
  // landing after either can never silently append onto the wrong context.
  const targetContext = state.submoduleGraph;
  const targetPath = g.path;
  const previousCommitCount = g.commits.length;
  loadingOlderCommits = true;
  if (state.view === 'graph') renderGraph();
  try {
    const page = await invoke('load_older_commits', { repositoryPath: targetPath, afterCommitId: oldest.id, limit: GRAPH_COMMIT_WINDOW });
    if (state.submoduleGraph !== targetContext || activeGraphData().path !== targetPath) return; // superseded — see the comment above
    if (state.submoduleGraph) {
      state.submoduleGraph.commits = state.submoduleGraph.commits.concat(page.commits);
      state.submoduleGraph.commits_truncated = page.has_more;
    } else {
      state.commits = state.commits.concat(page.commits);
      state.allCommits = state.commits;
      state.commits_truncated = page.has_more;
    }
  } catch (error) { status(String(error), 'error'); }
  finally {
    loadingOlderCommits = false;
    // Point 6 of the rework: append only the new page's own rows when it's
    // safe to (appendOlderGraphRows itself is the one place that decides
    // that, and falls back to a full renderGraph whenever it isn't sure).
    if (state.view === 'graph') appendOlderGraphRows(previousCommitCount);
  }
}

// Real, backend-computed ahead/behind + merge-base per local branch relative
// to whichever branch is currently "primary" in the graph view — see
// graph_branch_divergence's own doc comment for why this must never be
// inferred from lane/row layout. Cached per (repository path, primary
// branch) so switching the "Primary" picker or navigating away and back
// doesn't refetch needlessly; a real mutation (commit, merge, branch
// create/delete...) already triggers a full loadRepository reload, which
// resets state.commits and therefore this cache's key relevance naturally —
// nothing here needs its own separate invalidation hook.
let branchDivergenceCache = { key: null, data: null };
let branchDivergenceFetchKey = null; // the key currently in flight, so a second renderGraph() call for the same key doesn't fire a second request
let headMainMergeBaseCache = { key: null, data: null };
let headMainMergeBaseFetchKey = null;

function ensureBranchDivergence(repositoryPath, primaryBranchName) {
  if (!repositoryPath || !primaryBranchName) return null;
  const key = `${repositoryPath}::${primaryBranchName}`;
  if (branchDivergenceCache.key === key) return branchDivergenceCache.data;
  if (branchDivergenceFetchKey === key) return null; // already fetching this exact one
  if (!invoke) return null;
  branchDivergenceFetchKey = key;
  invoke('graph_branch_divergence', { repositoryPath, primaryBranch: primaryBranchName })
    .then(data => {
      branchDivergenceFetchKey = null;
      // Only apply/re-render if nothing changed while this was in flight —
      // a different repository (parent or submodule), a different primary
      // branch, or having left the graph view entirely must not have a late
      // result silently paint over what's on screen now.
      const g = activeGraphData();
      if (g.path !== repositoryPath) return;
      const resolved = resolvePrimarySelection(g);
      if (resolved.kind !== 'branch' || resolved.name !== primaryBranchName) return;
      branchDivergenceCache = { key, data };
      if (state.view === 'graph') renderGraph();
    })
    .catch(() => { branchDivergenceFetchKey = null; }); // graph still renders correctly without annotations
  return null;
}

function ensureHeadMainMergeBase(g) {
  if (!g?.path || !g.headOid) return null;
  const key = `${g.path}::${g.headOid}`;
  if (headMainMergeBaseCache.key === key) return headMainMergeBaseCache.data;
  if (headMainMergeBaseFetchKey === key) return null;
  if (!invoke) return null;
  headMainMergeBaseFetchKey = key;
  invoke('graph_head_main_merge_base', { repositoryPath: g.path })
    .then(data => {
      headMainMergeBaseFetchKey = null;
      const current = activeGraphData();
      if (current.path !== g.path || current.headOid !== g.headOid) return;
      headMainMergeBaseCache = { key, data };
      if (state.view === 'graph') renderGraph();
    })
    .catch(() => { headMainMergeBaseFetchKey = null; });
  return null;
}

// ---- Rendering -------------------------------------------------------------
// Every node/edge is drawn on a single continuous SVG overlaid across the whole
// list, using each row's *actual* rendered position (measured from the DOM
// after layout) rather than an assumed fixed row height — this is what
// guarantees a commit's dot always lines up exactly with its own row, and that
// a parent/child edge is one real line straight from one dot to the other,
// however many rows apart they end up being (instead of independent per-row
// segments that only look connected when every row happens to be the same
// height).

// The one real per-row template — shared by renderGraph's full build and
// appendOlderGraphRows' incremental one, so "Load older" can never drift
// into a second, slightly-different copy of what a commit row looks like.
// `ctx` bundles everything both need to read: model/lanesWidth/
// stashesByBase/aheadAnnotations/branchPointRows/query/matchesQuery/
// currentBranch/refFilter (see renderGraph, which builds the real one).
function mergeCommitPresentation(commit, ctx) {
  if ((commit.parents || []).length <= 1) return null;
  const parentCommits = (commit.parents || []).map(parentId => ctx.commitById?.get(parentId)).filter(Boolean);
  const mainParent = parentCommits.find(parent => (parent.refs || []).some(ref => ref.kind === 'remote_branch' && (ref.name === 'origin/main' || ref.name === 'origin/master')));
  if (mainParent) {
    const mainRef = (mainParent.refs || []).find(ref => ref.kind === 'remote_branch' && (ref.name === 'origin/main' || ref.name === 'origin/master'))?.name || 'origin/main';
    return { label: 'MERGE MAIN', tooltip: `Merge commit — one direct parent is ${mainRef}. This is based on commit parents/refs, not lane color.` };
  }
  return { label: 'MERGE', tooltip: `Merge commit — this node has ${commit.parents.length} parents.` };
}

function buildCommitRowHtml(commit, index, ctx) {
  const node = ctx.model[index];
  const stashPills = (ctx.stashesByBase.get(commit.id) || []).map(stash => `<b class="stash-pill" data-toggle-stash="${stash.index}" data-tooltip="stash@{${stash.index}} — click for details">⇕ stash</b>`).join('');
  const stashDetails = (ctx.stashesByBase.get(commit.id) || []).map(stash => `<div class="stash-internals" data-stash-detail="${stash.index}" hidden>
    <div>stash@{${stash.index}}: ${esc(stash.message.replace(/^WIP on [^:]+:\s*[0-9a-f]+\s*/, 'WIP on ') || 'Saved changes')} — bundles working-tree changes, staged index${stash.message.includes('untracked') ? ', untracked files' : ''}</div>
    <div class="stash-file-list" data-stash-file-list="${stash.index}"></div>
  </div>`).join('');
  const ahead = ctx.aheadAnnotations.get(index);
  const isBranchPoint = ctx.branchPointRows.has(index);
  const isCommonAncestorWithMain = ctx.commonAncestorRows?.has(index);
  const commonAncestorBaseRef = ctx.headMainBase?.base_ref || 'origin/main';
  const isMergeCommit = (commit.parents || []).length > 1;
  // A query highlights matches instead of removing anything from the
  // graph — a non-matching commit stays exactly where it is, still fully
  // connected, as the real context for whichever matches surround it.
  const isMatch = ctx.query && ctx.matchesQuery(commit);
  const isSearchDimmed = ctx.query && !isMatch;
  // The All/Branches/Releases filter dims exactly the same way search
  // does — never removes a commit or its edges, only fades rows that
  // don't carry the kind of ref currently asked for. "Releases" means
  // "has a tag"; a merge or branch point is never dimmed regardless of
  // its own refs, since it's structural context for whatever nearby row
  // *does* match, the same reasoning search already applies.
  const refKinds = new Set((node.refs || []).map(r => r.kind));
  const hasLocalBranchRef = refKinds.has('local_branch');
  const hasRemoteBranchRef = refKinds.has('remote_branch');
  const hasPrimaryRemoteRef = (node.refs || []).some(ref => ref.kind === 'remote_branch' && ref.name === 'origin/main');
  const isCommandBranchStart = state.branchStartMarker?.repositoryPath === activeGraphData().path && state.branchStartMarker?.id === commit.id;
  const matchesFilter = ctx.refFilter === 'all' || isBranchPoint || (ctx.refFilter === 'branches' ? (refKinds.has('local_branch') || refKinds.has('remote_branch')) : refKinds.has('tag'));
  const isFilterDimmed = !matchesFilter;
  const mergePresentation = mergeCommitPresentation(commit, ctx);
  const headLocationPill = node.isHead
    ? `<b class="head-location-pill" data-tooltip="This is the commit currently checked out in ${esc(ctx.repositoryName || 'this repository')}">YOU ARE HERE · HEAD</b>`
    : '';
  const structuralBadges = [
    headLocationPill,
    isCommonAncestorWithMain ? `<b class="branch-point-pill common-main-pill" data-tooltip="Real merge-base between HEAD and ${esc(commonAncestorBaseRef)} — where this checkout diverged from main">Branch start</b>` : '',
    !isCommonAncestorWithMain && isBranchPoint ? '<b class="branch-point-pill" data-tooltip="Common ancestor or lane transition — computed from commit parents, not lane color">⑂</b>' : '',
    isCommandBranchStart ? `<b class="branch-point-pill command-start-pill" data-tooltip="merge-base with ${esc(state.branchStartMarker.baseRef)} — where this branch split from the selected base">Command start</b>` : '',
  ].join('');

  return `<article class="commit-row ${node.isHead ? 'is-head' : ''} ${hasPrimaryRemoteRef ? 'has-primary-remote-tip' : hasLocalBranchRef ? 'has-local-branch-tip' : hasRemoteBranchRef ? 'has-remote-branch-tip' : ''} ${isBranchPoint ? 'is-branch-point' : ''} ${isCommonAncestorWithMain ? 'is-common-ancestor-main' : ''} ${isMergeCommit ? 'is-merge-commit' : ''} ${isCommandBranchStart ? 'is-command-branch-start' : ''} ${isMatch ? 'is-search-match' : ''} ${isSearchDimmed || isFilterDimmed ? 'is-search-dimmed' : ''}" data-id="${esc(commit.id)}" data-lane="${node.lane}">
    <div class="graph-cell"></div>
    <div class="commit-body">
      <div class="commit-card"><div class="commit-main"><div class="commit-title-line">${structuralBadges}<span class="commit-title">${commitSubjectHtml(commit.subject)}</span></div><div class="commit-ref-line">${refsBadges(node.refs, node.isHead, ctx.currentBranch)}${stashPills}</div></div><span class="commit-id-wrap"><button type="button" class="commit-copy-sha" data-copy-commit-sha="${esc(commit.id)}" title="Copy full commit SHA">${esc(commit.id.slice(0, 8))} ⧉</button></span>
      <span class="commit-date">${esc(commit.date || '—')}</span><span class="topology-badges">${mergePresentation ? `<b class="merge-badge" data-tooltip="${esc(mergePresentation.tooltip)}">${esc(mergePresentation.label)}</b>` : ''}</span><span class="commit-author">${esc(commit.author)}</span></div>
      ${ahead ? `<div class="ahead-annotation">${esc(ahead)}</div>` : ''}${stashDetails}
    </div>
  </article>`;
}

function graphTruncationStubHtml(truncated) {
  return truncated ? `<div class="history-truncated-stub"><span>⋯ continues in older history</span><button id="loadOlderCommits" ${loadingOlderCommits ? 'disabled' : ''}>${loadingOlderCommits ? '<i class="spinner"></i> Loading…' : 'Load older'}</button></div>` : '';
}

function graphHeadBannerHtml(g, currentBranch, headVisible) {
  const label = g.headDetached ? `Detached at ${(g.headOid || '').slice(0, 8)}` : currentBranch;
  if (!label) return '';
  const visibility = headVisible
    ? '<button type="button" class="head-jump" data-jump-head>Jump to HEAD</button>'
    : '<em>not in loaded rows</em>';
  return `<span class="head-banner ${headVisible ? 'head-visible' : 'head-missing'}" data-tooltip="HEAD is the commit currently checked out">HEAD <i>→</i> <b>${esc(label)}</b>${visibility}</span>`;
}

function jumpToGraphHead() {
  const row = refs.graph?.querySelector('.commit-row.is-head[data-id]');
  if (!row) {
    status('HEAD is not visible in the currently loaded history window. Load older commits or clear filters/search.', 'error');
    return;
  }
  row.scrollIntoView({ block: 'center', behavior: 'smooth' });
  selectCommit(row.dataset.id);
  if (row.animate) row.animate([{ boxShadow: '0 0 0 0 rgba(126, 211, 255, .85)' }, { boxShadow: '0 0 0 12px rgba(126, 211, 255, 0)' }], { duration: 900, easing: 'ease-out' });
}

function activeGraphMergeTarget() {
  if (state.submoduleGraph) {
    return { targetPath: state.submoduleGraph.relativePath, label: state.submoduleGraph.repository.current_branch || state.submoduleGraph.name, isSubmodule: true };
  }
  return mergeTargetForMain();
}

function showFloatingMenu(event, items) {
  event.preventDefault();
  event.stopPropagation();
  document.querySelectorAll('.floating-action-menu').forEach(menu => menu.remove());
  const menu = document.createElement('div');
  menu.className = 'floating-action-menu';
  menu.style.left = `${Math.min(event.clientX, innerWidth - 280)}px`;
  menu.style.top = `${Math.min(event.clientY, innerHeight - 180)}px`;
  menu.innerHTML = items.map(item => `<button type="button" data-action="${esc(item.id)}" ${item.disabled ? 'disabled' : ''}><strong>${esc(item.label)}</strong>${item.detail ? `<small>${esc(item.detail)}</small>` : ''}</button>`).join('');
  document.body.appendChild(menu);
  menu.querySelectorAll('[data-action]').forEach(button => button.addEventListener('click', () => {
    const item = items.find(candidate => candidate.id === button.dataset.action);
    menu.remove();
    if (item && !item.disabled) item.run();
  }));
  setTimeout(() => document.addEventListener('click', click => { if (!menu.contains(click.target)) menu.remove(); }, { once: true }), 0);
}

function graphRefsForCommit(commitId) {
  const node = (lastGraphModel || []).find(candidate => candidate.commitId === commitId);
  return node?.refs || [];
}

function graphBranchRefsForCommit(commitId) {
  const seen = new Set();
  return graphRefsForCommit(commitId)
    .filter(ref => ref?.name && (ref.kind === 'local_branch' || ref.kind === 'remote_branch'))
    .sort((a, b) => {
      if (a.name === 'origin/main') return -1;
      if (b.name === 'origin/main') return 1;
      if (a.kind !== b.kind) return a.kind === 'local_branch' ? -1 : 1;
      return a.name.localeCompare(b.name);
    })
    .filter(ref => {
      if (seen.has(ref.name)) return false;
      seen.add(ref.name);
      return true;
    });
}

function graphMergeUnavailableReason(branchName) {
  const g = activeGraphData();
  if (!g.currentBranch) return g.headDetached ? 'Checkout or switch to a branch first — HEAD is detached.' : 'No current branch is checked out.';
  if (branchName === g.currentBranch) return 'Already on this branch.';
  return '';
}

function graphMergeMenuItem(branchName, id = 'merge') {
  const g = activeGraphData();
  const current = g.currentBranch || (g.headDetached ? `Detached HEAD ${(g.headOid || '').slice(0, 8)}` : 'current checkout');
  const unavailable = graphMergeUnavailableReason(branchName);
  return {
    id,
    label: `Merge ${branchName} into current branch`,
    detail: unavailable || `${branchName} → ${current}. Current branch is the only branch changed.`,
    disabled: !!unavailable,
    run: () => openMergeBranchDialog(activeGraphMergeTarget(), branchName),
  };
}

function showGraphBranchContextMenu(event, branchName) {
  showFloatingMenu(event, [
    graphMergeMenuItem(branchName),
  ]);
}

async function refreshAfterGraphCommitAction(repositoryPath) {
  directoryCache.clear();
  if (state.submoduleGraph && state.submoduleGraph.repository.path === repositoryPath) {
    await openSubmoduleGraph({ relative_path: state.submoduleGraph.relativePath, name: state.submoduleGraph.name });
  } else {
    await loadRepository(repositoryPath, { keepPath: true });
  }
}

async function createBranchFromGraphCommit(commitId) {
  const context = activeRepositoryContext();
  if (!context.path) return;
  const shortId = commitId.slice(0, 8);
  const name = await customPrompt(`Create a new branch starting at commit ${shortId}:`, `branch-from-${shortId}`, { title: 'Create branch from commit', okLabel: 'Create branch' });
  if (!name?.trim()) return;
  if (!invoke) { status(`Preview: branch ${name.trim()} from ${shortId}`); return; }
  try {
    status(`Creating branch ${name.trim()} from ${shortId}…`, 'busy');
    await invoke('create_branch_at_commit', { repositoryPath: context.path, branch: name.trim(), commitId });
    await refreshAfterGraphCommitAction(context.path);
    const message = `Branch "${name.trim()}" created from ${shortId} and checked out.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) { handleError(error); }
}

async function checkoutGraphCommit(commitId) {
  const context = activeRepositoryContext();
  if (!context.path) return;
  const shortId = commitId.slice(0, 8);
  const ok = await customConfirm(`Checkout commit ${shortId}?\n\nThis will detach HEAD in ${context.name}. Your current branch will not move. Git will refuse if local changes would be overwritten.`, { title: 'Checkout this commit', danger: true, okLabel: 'Checkout commit' });
  if (!ok) return;
  if (!invoke) { status(`Preview: checkout ${shortId}`); return; }
  try {
    status(`Checking out ${shortId}…`, 'busy');
    await invoke('checkout_commit', { repositoryPath: context.path, commitId });
    await refreshAfterGraphCommitAction(context.path);
    const message = `Checked out ${shortId}. HEAD is detached.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) { handleError(error); }
}

async function restoreExactCheckpointFromGraphCommit(commitId) {
  const context = activeRepositoryContext();
  if (!context.path) return;
  const shortId = commitId.slice(0, 8);
  const ok = await customConfirm(
    `Restore exact checkpoint ${shortId} in ${context.name}?\n\nThis is stronger than Checkout:\n• detaches HEAD at this commit\n• discards tracked local edits\n• removes untracked, non-ignored leftovers\n• forces submodules to the versions recorded by this checkpoint\n\nIgnored build artifacts are not removed. This cannot be undone from the app.`,
    { title: 'Restore exact checkpoint', danger: true, okLabel: 'Restore exact checkpoint' }
  );
  if (!ok) return;
  if (!invoke) { status(`Preview: restore exact checkpoint ${shortId}`); return; }
  try {
    status(`Restoring exact checkpoint ${shortId} and cleaning leftovers…`, 'busy');
    await invoke('restore_exact_checkpoint', { repositoryPath: context.path, commitId });
    await refreshAfterGraphCommitAction(context.path);
    const message = `Workspace restored exactly to checkpoint ${shortId}.`;
    status(message); showOperationToast(message, 'success');
  } catch (error) { handleError(error); }
}

function showGraphCommitContextMenu(event, commitId) {
  const branchRefs = graphBranchRefsForCommit(commitId).slice(0, 6);
  const mergeItems = branchRefs.map((ref, index) => graphMergeMenuItem(ref.name, `merge-${index}`));
  const submoduleCompareItems = [];
  if (state.submoduleGraph) {
    const anchor = state.submoduleCompareAnchor;
    const sameSubmoduleAnchor = anchor?.parentPath === state.submoduleGraph.parentRepositoryPath && anchor?.relativePath === state.submoduleGraph.relativePath;
    submoduleCompareItems.push(
      { id: 'compare-head', label: 'Compare with current HEAD', detail: `${commitId.slice(0, 8)} ↔ ${(state.submoduleGraph.repository.head_oid || 'HEAD').slice(0, 8)}`, run: () => openSubmoduleCompareFromGraphCommit(commitId) },
      { id: 'compare-start', label: 'Set as compare start', detail: commitId.slice(0, 8), run: () => { state.submoduleCompareAnchor = { parentPath: state.submoduleGraph.parentRepositoryPath, relativePath: state.submoduleGraph.relativePath, name: state.submoduleGraph.name, revision: commitId }; status(`Compare start set to ${commitId.slice(0, 8)}. Right-click another commit and choose Compare with start.`); } },
    );
    if (sameSubmoduleAnchor && anchor.revision !== commitId) {
      submoduleCompareItems.push({ id: 'compare-anchor', label: 'Compare with start', detail: `${anchor.revision.slice(0, 8)} → ${commitId.slice(0, 8)}`, run: () => openSubmoduleCompareFromGraphCommit(anchor.revision, commitId) });
    }
  }
  showFloatingMenu(event, [
    ...submoduleCompareItems,
    ...mergeItems,
    { id: 'branch', label: 'Create branch from this commit', detail: commitId.slice(0, 8), run: () => createBranchFromGraphCommit(commitId) },
    { id: 'checkout', label: 'Checkout this commit', detail: 'Detached HEAD', run: () => checkoutGraphCommit(commitId) },
    { id: 'restore-exact', label: 'Restore exact checkpoint…', detail: 'Clean workspace to this commit', danger: true, run: () => restoreExactCheckpointFromGraphCommit(commitId) },
  ]);
}

// Row click (select), tag-badge click (tag detail), and stash-pill click
// (expand/collapse) — shared so a fresh batch of rows, whether from a full
// renderGraph or an incremental appendOlderGraphRows, is wired up
// identically either way; attaching this twice to the *same* already-wired
// row (which double-toggling stash-pill state would immediately reveal)
// is exactly what keeping this to one shared function, called once per
// row per its own lifetime, avoids.
function wireGraphRowInteractions(rowElements) {
  rowElements.forEach(row => row.addEventListener('click', () => selectCommit(row.dataset.id)));
  rowElements.forEach(row => row.addEventListener('contextmenu', event => showGraphCommitContextMenu(event, row.dataset.id)));
  rowElements.forEach(row => row.querySelectorAll('[data-copy-commit-sha]').forEach(button => button.addEventListener('click', event => {
    event.stopPropagation();
    copyText(button.dataset.copyCommitSha || '', 'Commit SHA copied.');
  })));
  rowElements.forEach(row => row.querySelectorAll('[data-graph-ref-name]').forEach(pill => pill.addEventListener('contextmenu', event => showGraphBranchContextMenu(event, pill.dataset.graphRefName))));
  rowElements.forEach(row => row.querySelectorAll('[data-tag-name]').forEach(pill => pill.addEventListener('click', event => { event.stopPropagation(); showTagDetails(pill.dataset.tagName); })));
  rowElements.forEach(row => row.querySelectorAll('[data-toggle-stash]').forEach(pill => pill.addEventListener('click', event => {
    event.stopPropagation();
    const index = pill.dataset.toggleStash;
    const detail = pill.closest('.commit-card').parentElement.querySelector(`[data-stash-detail="${index}"]`);
    if (!detail) return;
    detail.toggleAttribute('hidden');
    const fileList = detail.querySelector(`[data-stash-file-list="${index}"]`);
    // Load the file list lazily, only the first time this stash is expanded
    // — "what's actually in there?" answered without needing to pop it first.
    if (!detail.hidden && fileList && !fileList.dataset.loaded) {
      fileList.dataset.loaded = '1';
      fileList.innerHTML = '<i class="spinner"></i>';
      if (!invoke) { fileList.innerHTML = '<div class="stash-file">preview.txt</div>'; return; }
      invoke('stash_entry_files', { repositoryPath: activeGraphData().path, stashIndex: Number(index) })
        .then(files => { fileList.innerHTML = files.length ? files.map(file => `<div class="stash-file">${esc(file)}</div>`).join('') : '<div class="stash-file">(no files — this stash is empty)</div>'; })
        .catch(error => { fileList.innerHTML = `<div class="stash-file">${esc(String(error))}</div>`; });
    }
    // The stash toggle above changes this row's height without a full
    // renderGraph — the SVG lines/dots were positioned from row.offsetTop
    // measurements taken before that change, so they'd drift out of
    // alignment with every row below it otherwise.
    scheduleGraphOverlayRedraw();
  })));
}
function renderGraph() {
  const modelStarted = performance.now();
  const query = refs.search.value.trim().toLowerCase();
  const g = activeGraphData();
  // Search never removes a commit from the graph being built — doing that
  // used to let a matching commit's non-matching parent silently vanish
  // from `commits` while buildGraphModel still tried to route an edge to
  // it, landing on whatever row happened to come next instead (a wrong
  // edge, not just a missing one). Every commit stays in the model
  // unconditionally; a query only decides which rows get highlighted.
  const commits = g.commits || [];
  // .refs is now Vec<{name, kind}> from the backend — search must still
  // find a tag (or branch) by name, exactly as it did with the old flat
  // string list, just reading the structured shape correctly now.
  const matchesQuery = c => !query || `${c.subject} ${c.author} ${c.id} ${(c.refs || []).map(r => r.name).join(' ')}`.toLowerCase().includes(query);
  const matchCount = query ? commits.filter(matchesQuery).length : 0;
  const currentBranch = g.currentBranch;
  // "Primary" drives which lane is lane 0 — defaults to whatever is
  // currently checked out (or, for a detached checkout, HEAD's exact
  // commit — there's no branch to default to). Any commit whose tip isn't
  // reachable from it (a sibling that's diverged, even if newer) gets
  // pushed to its own lane instead of ever sharing the primary one. The
  // picker accepts the detached HEAD itself, any local branch, or any
  // remote-tracking ref — encoded as "detached" / "branch:<name>" /
  // "remote:<name>" so all three can share the one <select>.
  const localBranchNames = (g.branches || []).filter(b => !b.remote).map(b => b.name);
  const remoteBranchNames = (g.branches || []).filter(b => b.remote).map(b => b.name);
  const { kind: primaryKind, name: primaryName } = resolvePrimarySelection(g);
  // primaryName can be either a local branch or a remote-tracking ref (the
  // picker accepts both) — match against whichever structured ref kind it
  // actually is, by name.
  const hasRefNamed = (commit, name) => (commit.refs || []).some(r => (r.kind === 'local_branch' || r.kind === 'remote_branch') && r.name === name);
  const primaryTip = primaryKind === 'detached' ? commits.find(c => c.id === g.headOid) : primaryName ? commits.find(c => hasRefNamed(c, primaryName)) : null;
  const model = buildGraphModel(commits, primaryTip?.id);
  const nodeById = new Map(commits.map((commit, index) => [commit.id, model[index]]));
  const headEntry = g.headDetached ? commits.find(c => c.id === g.headOid) : currentBranch ? commits.find(commit => hasRefNamed(commit, currentBranch)) : null;
  if (headEntry) { nodeById.get(headEntry.id).isHead = true; }
  const headVisible = !!headEntry;
  const maxLanes = Math.max(1, ...model.map(n => Math.max(n.before.length, n.after.length)));
  const lanesWidth = maxLanes * LANE_WIDTH;
  jsPerfLog(`renderGraph model build (${commits.length} commits)`, performance.now() - modelStarted);

  const pickerOptions = [
    ...(g.headDetached ? [{ value: 'detached', label: `Detached HEAD (${(g.headOid || '').slice(0, 8)})` }] : []),
    ...localBranchNames.map(name => ({ value: `branch:${name}`, label: name })),
    ...remoteBranchNames.map(name => ({ value: `remote:${name}`, label: name })),
  ];
  const selectedPickerValue = primaryKind === 'detached' ? 'detached' : `${primaryKind}:${primaryName}`;

  // Path/name stays visible here regardless of which repository is active —
  // the persistent global topbar (#repoPath) always shows the *parent*
  // project's own path, even while a submodule's Branch Map is open, so it
  // alone can't answer "which repository am I actually looking at right
  // now" (point 4 of the rework).
  const refFilter = activeGraphRefFilter();
  const filterOptions = [['all', 'All'], ['branches', 'Branches'], ['releases', 'Releases']];
  refs.graphView.style.setProperty('--lanes-width', `${lanesWidth}px`);
  // Message C, point 3's own literal example format ("Repository: X" /
  // "Branch: Y" or "Detached at Z" / "Parent: repo") — explicit, labeled
  // lines, not just the identity folded into the title/path badge above,
  // so there's no ambiguity even at a glance. Only submodule context ever
  // shows a "Parent" line — the parent repository's own graph has none by
  // definition.
  const identityBadges = `<span class="graph-identity-badge" data-tooltip="Repository name"><i>Repository:</i> ${esc(g.name || '')}</span>${state.submoduleGraph ? `<span class="graph-identity-badge" data-tooltip="Parent repository"><i>Parent:</i> ${esc(state.repository?.name || '')}</span>` : ''}`;
  refs.laneLegend.innerHTML = `<span class="time-direction"><b>NEWEST</b><i>↓</i><b>OLDEST</b></span>
    <span class="graph-path-badge" data-tooltip="${esc(g.path || '')}">${esc(g.path || '')}</span>
    ${identityBadges}
    ${graphHeadBannerHtml(g, currentBranch, headVisible)}
    ${query ? `<span class="search-match-count">${matchCount} match${matchCount === 1 ? '' : 'es'} — rest shown as context</span>` : ''}
    <div class="ref-filter-group" role="group" aria-label="Filter by ref kind">${filterOptions.map(([value, label]) => `<button type="button" class="ref-filter-btn ${refFilter === value ? 'active' : ''}" data-ref-filter="${value}">${label}</button>`).join('')}</div>
    ${pickerOptions.length > 1 ? `<label class="primary-branch-picker"><span>Primary</span><select id="graphPrimaryBranch">${pickerOptions.map(opt => `<option value="${esc(opt.value)}" ${opt.value === selectedPickerValue ? 'selected' : ''}>${esc(opt.label)}</option>`).join('')}</select></label>` : ''}
    <span class="lane-header"><span>GRAPH</span><span>COMMIT</span></span>`;
  $('#graphPrimaryBranch')?.addEventListener('change', event => { setActiveGraphPrimaryBranch(event.target.value); renderGraph(); });
  refs.laneLegend.querySelector('[data-jump-head]')?.addEventListener('click', jumpToGraphHead);
  refs.laneLegend.querySelectorAll('[data-ref-filter]').forEach(button => button.addEventListener('click', () => { setActiveGraphRefFilter(button.dataset.refFilter); renderGraph(); }));

  // Stash entries are informational pointers, not real DAG commits. Rendering
  // them as their own row used to insert a break in the middle of the vertical
  // chain, making a branch that's just one commit ahead look like it "floats"
  // disconnected from where it actually came from. They're attached instead as
  // a small lateral pill on their base commit's own row — secondary
  // information, never interrupting the parent/child line.
  const stashesByBase = new Map();
  (g.stashes || []).forEach(stash => { if (!stashesByBase.has(stash.base_commit)) stashesByBase.set(stash.base_commit, []); stashesByBase.get(stash.base_commit).push(stash); });

  // "N commits ahead" and the branch-point marker are real Git facts — from
  // graph_branch_divergence's merge-base/graph_ahead_behind, i.e. actual OIDs
  // and DAG walks — never guessed from which row/lane a ref happens to land
  // on. Two branches can share a lane (buildGraphModel reuses lanes for
  // unrelated branches on purpose) with no ancestry relationship at all; the
  // old lane-distance heuristic could and did label that as if it were one.
  // ensureBranchDivergence is non-blocking: it renders without annotations
  // immediately, fetches once per (repository, primary branch), and
  // re-renders itself when the real data lands. Only meaningful — and only
  // supported by the backend — when a real local branch is primary; a
  // detached HEAD or a remote-tracking ref has no "ahead of X" to compute
  // against itself.
  const divergence = primaryKind === 'branch' && primaryName ? ensureBranchDivergence(g.path, primaryName) : null;
  const headMainBase = ensureHeadMainMergeBase(g);
  const aheadAnnotations = new Map(); // row index -> short text
  const branchPointRows = new Set(); // row indices that are a shared-ancestor base
  const commonAncestorRows = new Set(); // row indices that are merge-base(HEAD, origin/main/origin/master)
  const headMainBaseRow = headMainBase?.oid ? new Map(commits.map((c, i) => [c.id, i])).get(headMainBase.oid) : null;
  if (headMainBaseRow != null) { commonAncestorRows.add(headMainBaseRow); branchPointRows.add(headMainBaseRow); }
  if (divergence) {
    const rowByCommitId = new Map(commits.map((c, i) => [c.id, i]));
    for (const entry of divergence) {
      if (entry.name === primaryName) continue;
      const tipRow = rowByCommitId.get(entry.tip);
      if (tipRow != null && entry.ahead > 0) {
        aheadAnnotations.set(tipRow, `↳ ${entry.ahead} commit${entry.ahead === 1 ? '' : 's'} ahead of ${primaryName}`);
      }
      // The merge-base may be outside the currently loaded (newest-500)
      // window — only mark it when it's actually a row on screen.
      if (entry.merge_base) {
        const baseRow = rowByCommitId.get(entry.merge_base);
        if (baseRow != null) branchPointRows.add(baseRow);
      }
    }
  }
  // More generally: wherever an edge actually crosses lanes (a real diagonal
  // fork/merge line, not just a straight same-lane continuation), the commit
  // it lands on is a genuine branch/merge point — mark it the same way.
  const commitIdToRow = new Map(commits.map((c, i) => [c.id, i]));
  model.forEach(node => node.parents.forEach(parent => {
    if (parent.targetLane === node.lane) return;
    const targetRow = commitIdToRow.get(parent.commitId);
    if (targetRow != null) branchPointRows.add(targetRow);
  }));

  // The full per-row context, bundled so appendOlderGraphRows (below) can
  // recompute the exact same inputs for just the newly-arrived rows,
  // without a second, drifting copy of the actual row markup — both paths
  // call buildCommitRowHtml for the real template.
  lastGraphRenderContext = { model, lanesWidth, stashesByBase, aheadAnnotations, branchPointRows, commonAncestorRows, headMainBase, commitById: new Map(commits.map(commit => [commit.id, commit])), query, matchesQuery, currentBranch, refFilter, repositoryName: g.name };
  const rowsStarted = performance.now();
  const rows = commits.map((commit, index) => buildCommitRowHtml(commit, index, lastGraphRenderContext)).join('') || '<div class="empty-change">No commits in this history</div>';

  // Real, backend-confirmed truncation (never "exactly 500 came back") —
  // never presented as if this were the whole history. Search filtering the
  // *visible* rows doesn't change this: the underlying loaded set is still
  // truncated at the same point regardless of what's currently matched.
  const truncationStub = graphTruncationStubHtml(g.commitsTruncated);

  const domStarted = performance.now();
  jsPerfLog(`renderGraph rows build (${commits.length} rows)`, domStarted - rowsStarted);
  refs.graph.innerHTML = `<svg class="graph-overlay"></svg>` + rows + truncationStub + GRAPH_LEGEND_HTML;
  jsPerfLog(`renderGraph DOM render (${commits.length} rows)`, performance.now() - domStarted);
  wireGraphRowInteractions(refs.graph.querySelectorAll('.commit-row[data-id]'));
  $('#loadOlderCommits')?.addEventListener('click', loadOlderGraphCommits);
  lastGraphModel = model; lastGraphLanesWidth = lanesWidth;
  // Deferred to requestAnimationFrame: the overlay reads each row's real
  // offsetTop, which only reflects this render's *own* new rows once the
  // browser has actually laid them out — measuring synchronously, right
  // after the innerHTML assignment above, risks reading stale positions
  // from before layout on some engines. rAF also keeps this off the
  // critical path of the render itself, so the rows painting doesn't wait
  // on the overlay's own DOM reads.
  if (commits.length) requestAnimationFrame(() => {
    if (lastGraphModel !== model) return; // superseded by a newer render before this frame ran
    const overlayStarted = performance.now();
    drawGraphOverlay(model, lanesWidth);
    jsPerfLog(`renderGraph SVG overlay (${commits.length} commits)`, performance.now() - overlayStarted);
  });
}

// Redraws the SVG overlay against whatever the DOM's *current* row layout
// actually is, without rebuilding the graph model/rows themselves — for
// anything that changes row heights without a full renderGraph (a stash
// entry expanding/collapsing, or the window/pane being resized). Debounced
// so a burst of resize events collapses into one redraw, and guarded so it
// only ever touches a still-current graph, in a still-current view.
let graphOverlayRedrawTimer = null;
let lastGraphModel = null;
let lastGraphLanesWidth = 0;
let lastGraphRenderContext = null;
function scheduleGraphOverlayRedraw() {
  clearTimeout(graphOverlayRedrawTimer);
  graphOverlayRedrawTimer = setTimeout(() => {
    if (state.view === 'graph' && lastGraphModel && lastGraphModel.length) drawGraphOverlay(lastGraphModel, lastGraphLanesWidth);
  }, 80);
}

// Point 6 of the rework: append only the new page's own rows — the rows
// already on screen are left completely untouched, since buildGraphModel
// never looks ahead (a row's lane/before/after is only ever a function of
// the rows *above* it, so appending more below can never retroactively
// change one that's already rendered). Falls back to a full renderGraph
// whenever that isn't obviously true or safe: no previous render context
// to build on, a search query active (dimming/highlighting could touch
// old rows too), or a ref filter other than "all" active (same reason).
function appendOlderGraphRows(previousCommitCount) {
  const g = activeGraphData();
  const commits = g.commits || [];
  const query = refs.search.value.trim().toLowerCase();
  const refFilter = activeGraphRefFilter();
  if (commits.length <= previousCommitCount || !lastGraphRenderContext || query || refFilter !== 'all') { renderGraph(); return; }

  const modelStarted = performance.now();
  const currentBranch = g.currentBranch;
  const hasRefNamed = (commit, name) => (commit.refs || []).some(r => (r.kind === 'local_branch' || r.kind === 'remote_branch') && r.name === name);
  const { kind: primaryKind, name: primaryName } = resolvePrimarySelection(g);
  const primaryTip = primaryKind === 'detached' ? commits.find(c => c.id === g.headOid) : primaryName ? commits.find(c => hasRefNamed(c, primaryName)) : null;
  const model = buildGraphModel(commits, primaryTip?.id);
  const nodeById = new Map(commits.map((commit, index) => [commit.id, model[index]]));
  const headEntry = g.headDetached ? commits.find(c => c.id === g.headOid) : currentBranch ? commits.find(commit => hasRefNamed(commit, currentBranch)) : null;
  if (headEntry) { nodeById.get(headEntry.id).isHead = true; }
  const maxLanes = Math.max(1, ...model.map(n => Math.max(n.before.length, n.after.length)));
  const lanesWidth = maxLanes * LANE_WIDTH;

  const divergence = primaryKind === 'branch' && primaryName ? ensureBranchDivergence(g.path, primaryName) : null;
  const headMainBase = ensureHeadMainMergeBase(g);
  const aheadAnnotations = new Map();
  const branchPointRows = new Set();
  const commonAncestorRows = new Set();
  const headMainBaseRow = headMainBase?.oid ? new Map(commits.map((c, i) => [c.id, i])).get(headMainBase.oid) : null;
  if (headMainBaseRow != null) { commonAncestorRows.add(headMainBaseRow); branchPointRows.add(headMainBaseRow); }
  if (divergence) {
    const rowByCommitId = new Map(commits.map((c, i) => [c.id, i]));
    for (const entry of divergence) {
      if (entry.name === primaryName) continue;
      const tipRow = rowByCommitId.get(entry.tip);
      if (tipRow != null && entry.ahead > 0) aheadAnnotations.set(tipRow, `↳ ${entry.ahead} commit${entry.ahead === 1 ? '' : 's'} ahead of ${primaryName}`);
      if (entry.merge_base) {
        const baseRow = rowByCommitId.get(entry.merge_base);
        if (baseRow != null) branchPointRows.add(baseRow);
      }
    }
  }
  const commitIdToRow = new Map(commits.map((c, i) => [c.id, i]));
  model.forEach(node => node.parents.forEach(parent => {
    if (parent.targetLane === node.lane) return;
    const targetRow = commitIdToRow.get(parent.commitId);
    if (targetRow != null) branchPointRows.add(targetRow);
  }));

  // Stashes are untouched by loading more history — the previous render's
  // own base-commit map is still exactly correct, including for a stash
  // based on a commit that only just became loaded.
  // Older history can easily introduce more simultaneous lanes than the
  // page already on screen ever needed — the header's own column width
  // (set once per full renderGraph) must be kept in sync here too, or the
  // "GRAPH | COMMIT" labels stop lining up with the wider rows below them.
  refs.graphView.style.setProperty('--lanes-width', `${lanesWidth}px`);
  const ctx = { model, lanesWidth, stashesByBase: lastGraphRenderContext.stashesByBase, aheadAnnotations, branchPointRows, commonAncestorRows, headMainBase, commitById: new Map(commits.map(commit => [commit.id, commit])), query: '', matchesQuery: () => false, currentBranch, refFilter, repositoryName: g.name };
  jsPerfLog(`appendOlderGraphRows model build (${commits.length} commits)`, performance.now() - modelStarted);

  const domStarted = performance.now();
  const rowsBefore = refs.graph.querySelectorAll('.commit-row[data-id]').length;
  const newRowsHtml = commits.slice(previousCommitCount).map((commit, i) => buildCommitRowHtml(commit, previousCommitCount + i, ctx)).join('');
  refs.graph.querySelector('.history-truncated-stub')?.remove();
  // The legend (GRAPH_LEGEND_HTML) is always the very last child — new rows
  // and the fresh stub go right before it, never after, or the legend
  // would end up stranded in the middle of the history instead of at the
  // base of it.
  const legend = refs.graph.querySelector('.graph-legend');
  const newTailHtml = newRowsHtml + graphTruncationStubHtml(g.commitsTruncated);
  if (legend) legend.insertAdjacentHTML('beforebegin', newTailHtml); else refs.graph.insertAdjacentHTML('beforeend', newTailHtml);
  jsPerfLog(`appendOlderGraphRows DOM append (${commits.length - previousCommitCount} new rows)`, performance.now() - domStarted);
  $('#loadOlderCommits')?.addEventListener('click', loadOlderGraphCommits);
  wireGraphRowInteractions(Array.prototype.slice.call(refs.graph.querySelectorAll('.commit-row[data-id]'), rowsBefore));

  lastGraphModel = model; lastGraphLanesWidth = lanesWidth; lastGraphRenderContext = ctx;
  requestAnimationFrame(() => {
    if (lastGraphModel !== model) return; // superseded by a newer render before this frame ran
    const overlayStarted = performance.now();
    drawGraphOverlay(model, lanesWidth);
    jsPerfLog(`appendOlderGraphRows SVG overlay (${commits.length} commits)`, performance.now() - overlayStarted);
  });
}

function drawGraphOverlay(model, lanesWidth) {
  const container = refs.graph;
  const positions = new Map(); // commitId -> {x, y}
  const branchPointIds = new Set();
  container.querySelectorAll('.commit-row[data-id]').forEach((row, index) => {
    const node = model[index];
    positions.set(node.commitId, { x: laneX(node.lane), y: row.offsetTop + row.offsetHeight / 2 });
    if (row.classList.contains('is-branch-point')) branchPointIds.add(node.commitId);
  });

  const parts = [];
  model.forEach(node => {
    const from = positions.get(node.commitId); if (!from) return;
    node.parents.forEach(parent => {
      const to = positions.get(parent.commitId); if (!to) return;
      const color = palette[node.lane % palette.length];
      if (from.x === to.x) {
        const attrs = `x1="${from.x}" y1="${from.y}" x2="${to.x}" y2="${to.y}"`;
        parts.push(`<line class="graph-edge-underlay" ${attrs}/><line class="graph-edge" ${attrs} stroke="${color}" stroke-width="3"/>`);
      } else {
        const d = `M${from.x} ${from.y} C${from.x} ${(from.y + to.y) / 2} ${to.x} ${(from.y + to.y) / 2} ${to.x} ${to.y}`;
        parts.push(`<path class="graph-edge-underlay" d="${d}"/><path class="graph-edge" d="${d}" stroke="${color}" stroke-width="3"/>`);
      }
    });
  });
  model.forEach(node => {
    const pos = positions.get(node.commitId); if (!pos) return;
    const color = palette[node.lane % palette.length];
    const refKinds = new Set((node.refs || []).map(ref => ref.kind));
    const isPrimaryRemote = (node.refs || []).some(ref => ref.kind === 'remote_branch' && ref.name === 'origin/main');
    const branchTipStroke = isPrimaryRemote ? '#ff626d' : refKinds.has('local_branch') ? '#4bd3dc' : refKinds.has('remote_branch') ? '#8298b8' : '';
    if (node.isHead) {
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="14" fill="#7ed3ff" opacity="0.22"/><circle cx="${pos.x}" cy="${pos.y}" r="10" fill="none" stroke="#7ed3ff" stroke-width="2.2"/><circle cx="${pos.x}" cy="${pos.y}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/><circle cx="${pos.x}" cy="${pos.y}" r="3" fill="#eaf7ff"/>`);
    } else if (branchPointIds.has(node.commitId)) {
      // The common ancestor two differently-labeled refs share — a distinct
      // ring marks it as the split point, without bending the (real, single)
      // lane into a decorative fork it doesn't topologically have yet.
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/><circle cx="${pos.x}" cy="${pos.y}" r="10" fill="none" stroke="${color}" stroke-width="1.4" stroke-dasharray="2 2"/>`);
    } else {
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/>`);
    }
    if (branchTipStroke) parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="11" fill="none" stroke="${branchTipStroke}" stroke-width="${isPrimaryRemote ? 2.8 : 2}" ${refKinds.has('remote_branch') && !isPrimaryRemote ? 'stroke-dasharray="3 2"' : ''} opacity="0.95"/>`);
  });

  // Point 8 of the report: a lane still open (unresolved) at the very last
  // loaded row must never just stop with no explanation — that reads as an
  // arbitrary colored line, not "this branch's real history continues past
  // what's loaded". Only drawn when there's real, backend-confirmed history
  // beyond this page (commitsTruncated) — a lane that simply reached its
  // own true root commit (the real end of that branch's history) gets no
  // such mark, since there genuinely is nothing more to continue into.
  if (activeGraphData().commitsTruncated && model.length) {
    const lastNode = model[model.length - 1];
    const lastPos = positions.get(lastNode.commitId);
    if (lastPos) {
      lastNode.after.forEach((commitId, lane) => {
        if (commitId == null) return;
        const color = palette[lane % palette.length];
        const x = laneX(lane);
        const attrs = `x1="${x}" y1="${lastPos.y + 12}" x2="${x}" y2="${lastPos.y + 26}"`;
        parts.push(`<line class="graph-edge-underlay graph-continuation" ${attrs} stroke-dasharray="1 5"/><line class="graph-edge graph-continuation" ${attrs} stroke="${color}" stroke-width="3" stroke-dasharray="1 5"/>`);
      });
    }
  }

  const svg = container.querySelector('.graph-overlay');
  const height = container.scrollHeight;
  svg.setAttribute('width', lanesWidth); svg.setAttribute('height', height);
  svg.setAttribute('viewBox', `0 0 ${lanesWidth} ${height}`);
  svg.innerHTML = parts.join('');
}

function selectCommit(id) {
  // Must search whichever repository's history is actually on screen — while
  // a submodule's graph is open, state.commits is still the *parent's* list
  // (see activeGraphData's own doc comment), so searching it directly here
  // would either find nothing (a submodule-only commit id) and throw right
  // below, or — worse — silently match an unrelated parent commit that
  // happens to share the same id prefix. This was a real, reproducible
  // crash: clicking a row in the Submodule Map's own graph.
  state.selectedCommit = activeGraphData().commits.find(commit => commit.id === id);
  refs.graph.querySelectorAll('.commit-row').forEach(row => row.classList.toggle('selected', row.dataset.id === id));
  const c = state.selectedCommit;
  // .refs is Vec<{name, kind}> now — shown here as "name (kind)" pairs,
  // kind spelled out in plain words rather than the raw wire value.
  const kindLabel = { local_branch: 'local branch', remote_branch: 'remote branch', tag: 'tag' };
  const refsSummary = (c.refs || []).map(r => `${r.name} (${kindLabel[r.kind] || r.kind})`).join(', ') || '—';
  refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${commitSubjectHtml(c.subject)}</h2><div class="hash"><a href="#" class="commit-server-link" data-commit-id="${esc(c.id)}" title="Open this commit on the server">${esc(c.id)} ↗</a></div>
    <div class="detail-grid"><span>Author</span><strong>${esc(c.author)}</strong><span>Date</span><strong>${esc(c.date)}</strong>
    <span>Parents</span><strong>${esc(c.parents?.join(', ') || 'First commit')}</strong><span>Refs</span><strong>${esc(refsSummary)}</strong></div></div>`;
}

// Point 3's last bullet: clicking a tag badge shows *that tag's* own
// detail — full name, the commit it resolves to, and whether it's
// lightweight or annotated — instead of (or as well as) selecting the
// commit row it sits on. A separate, on-demand backend call (tag_details)
// rather than something carried in every commit's own bulk payload; see
// that command's own doc comment in repository.rs.
async function showTagDetails(tagName) {
  const repositoryPath = activeGraphData().path;
  refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${esc(tagName)}</h2><p><i class="spinner"></i> Loading tag details…</p></div>`;
  if (!invoke) { refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${esc(tagName)}</h2><div class="detail-grid"><span>Kind</span><strong>Annotated</strong><span>Commit</span><strong>preview-only</strong></div></div>`; return; }
  try {
    const tag = await invoke('tag_details', { repositoryPath, tagName });
    if (activeGraphData().path !== repositoryPath) return; // left this repository/submodule while the call was in flight
    refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${esc(tag.name)}</h2><div class="hash"><a href="#" class="commit-server-link" data-commit-id="${esc(tag.commit_id)}" title="Open this commit on the server">${esc(tag.commit_id)} ↗</a></div>
      <div class="detail-grid"><span>Kind</span><strong>${tag.annotated ? 'Annotated tag' : 'Lightweight tag'}</strong>${tag.tagger ? `<span>Tagged by</span><strong>${esc(tag.tagger)}</strong>` : ''}${tag.date ? `<span>Date</span><strong>${esc(tag.date)}</strong>` : ''}</div>
      ${tag.message ? `<p class="tag-message">${esc(tag.message)}</p>` : ''}</div>`;
    refs.graph.querySelectorAll('.commit-row').forEach(row => row.classList.toggle('selected', row.dataset.id === tag.commit_id));
  } catch (error) {
    refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${esc(tagName)}</h2><p>${esc(String(error))}</p></div>`;
  }
}

// The small always-visible sidebar badge/subtitle, split out from the full
// drawer rebuild below — these need to stay live even while the Working
// tree drawer is closed (or another view like Graph/Commander is active),
// but rebuilding every change row's HTML and re-attaching its listeners
// doesn't, since none of it is visible until the drawer is actually opened
// (which already calls renderChanges() itself, in full, when it happens).
function updateChangeBadge() {
  refs.changeBadge.textContent = state.statusReady ? state.changes.length : '…';
  refs.workspaceSubtitle.textContent = !state.repository ? 'No repository loaded' : !state.statusReady ? 'Loading status…' : state.changes.length ? `${state.changes.length} changed files` : 'Everything committed';
}

function changeDisplayState(change) {
  if (!change) return 'Modified';
  if (change.status === '??') return 'New file';
  if (change.status === 'A') return change.staged ? 'Staged new file' : 'New file';
  if (change.status === 'D') return change.staged ? 'Staged deletion' : 'Deleted locally';
  if (change.status === 'R') return change.staged ? 'Staged rename' : 'Renamed locally';
  if (change.status === 'U') return 'Conflict';
  return change.staged ? 'Staged' : 'Modified';
}

function renderChanges() {
  updateChangeBadge();
  const scope = state.changesScope === 'folder' ? state.currentPath : ''; const scopedChanges = state.changes.filter(change => !scope || change.path === scope || change.path.startsWith(`${scope}/`));
  refs.drawerScopeTitle.textContent = state.changesScope === 'folder' ? `Changes in folder · /${scope}` : 'Working tree · entire repository';
  refs.changesSummary.textContent = scopedChanges.length ? `${scopedChanges.length} file${scopedChanges.length === 1 ? '' : 's'} available for staging` : `No changes in ${scope ? `/${scope}` : 'the repository'}`;
  $('#stageAllButton').disabled = scopedChanges.length === 0;
  $('#unstageAllButton').disabled = !scopedChanges.some(change => change.staged);
  refs.changes.innerHTML = scopedChanges.map(change => `<div class="change-row-wrap"><label class="change-row"><input type="checkbox" data-change-path="${esc(change.path)}" ${change.staged ? 'checked' : ''}>
    <span class="status-code">${esc(change.status)}</span><span class="change-path">${esc(change.path)}</span><span class="change-state">${esc(changeDisplayState(change))}</span></label>
    <button class="stash-file-btn" data-stash-path="${esc(change.path)}" title="Set aside just this file in its own repository's stash. Parent projects and submodules have separate stash lists.">⇕ Stash</button></div>`).join('') || `<div class="empty-change">No changes inside /${esc(scope)}</div>`;
  refs.changes.querySelectorAll('[data-stash-path]').forEach(button => button.addEventListener('click', () => stashOneFile(button.dataset.stashPath)));
  refs.changes.querySelectorAll('[data-change-path]').forEach(input => {
    // Belt-and-suspenders against a real Chromium/WebView2 quirk: a checkbox
    // just toggled by the user can have its *rendered* `checked` attribute
    // silently overridden by the browser's own form-state-restoration when
    // the surrounding HTML is replaced right after — the row would then show
    // the state from before the click. Setting the property explicitly right
    // after building the HTML always wins over that.
    const change = scopedChanges.find(item => item.path === input.dataset.changePath);
    input.checked = Boolean(change?.staged);
    input.addEventListener('change', () => toggleStage(input.dataset.changePath, input.checked));
  });
  const staged = scopedChanges.filter(change => change.staged).length;
  refs.selectionText.textContent = `${staged} file${staged === 1 ? '' : 's'} in staging area`;
  refs.commitButton.disabled = !staged || !refs.commitMessage.value.trim();
}

async function openPublish() {
  if (!state.repository) return;
  // "Unpublished commits" only ever means the MAIN project — it has no
  // concept of a selected submodule, so clicking it while a submodule is
  // selected silently shows the main project's own history instead, which
  // reads as if the submodule's branch dumped its whole history "to push".
  // Make the scope explicit instead of guessing.
  if (state.selectedEntry?.kind === 'submodule') {
    const entry = state.selectedEntry;
    const showMain = await customConfirm(`"${entry.name}" (a submodule) is currently selected, but "Unpublished commits" always shows the MAIN project — not the submodule. To push the submodule's own commits, cancel this and use "Push submodule" in ${entry.name}'s own details panel instead.`, { title: 'Unpublished commits — main project only', okLabel: 'Show main project anyway' });
    if (!showMain) return pushSubmodule(entry);
  }
  if (!state.remotes.length && invoke) state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path });
  if (!invoke && !state.remotes.length) state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }];
  const locals = state.branches.filter(branch => !branch.remote); refs.publishBranch.innerHTML = locals.map(branch => `<option value="${esc(branch.name)}" ${branch.current ? 'selected' : ''}>${esc(branch.name)}${branch.current ? ' (current)' : ''}</option>`).join('');
  refs.publishRemote.innerHTML = state.remotes.map(remote => `<option value="${esc(remote.name)}">${esc(remote.name)}</option>`).join(''); refs.publishDialog.showModal(); await refreshPublish();
}

// Git can only push a contiguous range — there's no way to publish a newer
// commit while holding back an older one it depends on. So "leave this one
// out" can only mean "stop pushing at the commit before it": clicking a
// commit sets it as the cutoff (included, along with everything older);
// everything newer than it is left unpublished for now.
// Submodule-publish-safety report, point 2: fetched once per refreshPublish
// and re-prepended on every renderPublishCommits call (including the ones
// triggered later by clicking a cutoff checkbox) — the outgoing commits stay
// fully visible either way; this only ever adds a warning above them,
// exactly what "mark them with a warning instead of hiding them" asks for.
let publishSubmoduleRisksHtml = '';
function submodulePublishRisksHtml(risks) {
  if (!risks.length) return '';
  const reasonText = risk => risk.risk === 'unpushed'
    ? 'has a commit not yet pushed to its own remote'
    : risk.risk === 'superseded_unpushed'
      ? `has an older intermediate project commit that points to a different local-only submodule commit; the final project commit points at ${esc((risk.target_submodule_oid || '').slice(0, 8) || 'another commit')}`
      : risk.risk === 'no_remote'
        ? 'has no remote configured at all'
        : risk.risk === 'local_only'
          ? `is only reachable from a local path or file:// URL (${esc(risk.configured_url || '?')})`
          : 'could not be checked locally';
  // The outgoing commit being checked can reference an older submodule
  // commit than what's actually checked out right now (the submodule moved
  // on locally after that parent commit was made) — spelled out here so a
  // fully-pushed, in-sync "Push submodule" preview for the *current*
  // checkout doesn't look like it silently disagrees with this warning.
  const currentNote = risk => risk.current_submodule_oid && risk.current_submodule_oid !== risk.submodule_oid
    ? ` — current checkout is <code>${esc(risk.current_submodule_oid.slice(0, 8))}</code>${risk.target_submodule_oid && risk.target_submodule_oid !== risk.submodule_oid ? ', so this warning is about an older parent commit, not the currently selected submodule version' : ''}`
    : '';
  const items = risks.map(risk => `<li><code>${esc(risk.relative_path)}</code> ${reasonText(risk)} — references <code>${esc(risk.submodule_oid.slice(0, 8))}</code> (${esc(risk.commit_subject)})${currentNote(risk)}</li>`).join('');
  const hardBlocking = risks.some(risk => risk.risk === 'unpushed');
  const headline = hardBlocking ? 'Publishing is blocked until the submodule below is pushed:' : 'Some older or local-only submodule references need your confirmation before publishing:';
  return `<div class="submodule-publish-safety-warning">⚠️ ${headline}<ul>${items}</ul></div>`;
}

function publishRemoteAheadWarningHtml(publish) {
  const behind = Number(publish?.behind) || 0;
  if (!behind) return '';
  const remoteBranch = publish.remote_branch || `${publish.remote}/${publish.branch}`;
  const diverged = (Number(publish?.ahead) || 0) > 0;
  return `<div class="publish-remote-ahead-warning">⚠️ ${esc(remoteBranch)} has ${behind} commit${behind === 1 ? '' : 's'} you do not have locally.${diverged ? ' Your local branch also has commits to publish, so the histories have diverged.' : ''} Fetch/pull or merge the remote changes before publishing if this branch is shared.</div>`;
}

function renderPublishCommits() {
  const commits = state.publish?.commits || [];
  const uptoIndex = state.publishUpto ? commits.findIndex(commit => commit.id === state.publishUpto) : commits.length - 1;
  refs.publishCommits.innerHTML = publishRemoteAheadWarningHtml(state.publish) + publishSubmoduleRisksHtml + (commits.map((commit, index) => {
    const willPush = index <= uptoIndex;
    return `<div class="publish-commit ${willPush ? '' : 'excluded'}" data-commit-id="${esc(commit.id)}">
      <span>${index + 1}</span><input type="checkbox" class="publish-check" data-index="${index}" ${willPush ? 'checked' : ''}>
      <div><strong>${esc(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div>
      ${willPush ? (index === uptoIndex && index < commits.length - 1 ? '<b class="publish-cutoff-badge" data-tooltip="Everything above stays local for now">WILL PUSH · stop here</b>' : '<b>WILL PUSH</b>') : '<b class="publish-held-back">STAYS LOCAL</b>'}
    </div>`;
  }).join('') || '<div class="publish-empty">This branch is already up to date on the server.</div>');
  // Git can only push a contiguous range from the oldest pending commit
  // forward — unchecking one always means "and everything newer than it
  // too" (they were built on top of it), checking one always means "and
  // everything older than it too" (it needs them). So every checkbox here
  // really sets the same single cutoff point; ticking any box just moves it.
  refs.publishCommits.querySelectorAll('.publish-check').forEach(box => box.addEventListener('change', () => {
    const index = Number(box.dataset.index);
    // '__none__' is a deliberately non-matching id — distinct from `null`,
    // which means "no cutoff set yet, default to a full push".
    state.publishUpto = box.checked ? commits[index].id : (index > 0 ? commits[index - 1].id : '__none__');
    renderPublishCommits(); updatePublishSummary();
  }));
}

function updatePublishSummary() {
  const commits = state.publish?.commits || [];
  const uptoIndex = state.publishUpto ? commits.findIndex(commit => commit.id === state.publishUpto) : commits.length - 1;
  const willPushCount = uptoIndex + 1;
  const heldBack = commits.length - willPushCount;
  refs.publishSummary.textContent = `${willPushCount} commit${willPushCount === 1 ? '' : 's'} to publish${heldBack ? ` · ${heldBack} staying local for now` : ''}`;
  $('#confirmPublish').disabled = !willPushCount;
}

const refreshPublishGuard = createRequestGuard();
async function refreshPublish() {
  const branch = refs.publishBranch.value, remote = refs.publishRemote.value;
  state.publishUpto = null;
  if (!branch || !remote) { refs.publishCommits.innerHTML = '<div class="publish-empty">Configure a remote before publishing.</div>'; refs.publishSummary.textContent = 'Nothing to publish'; $('#confirmPublish').disabled = true; return; }
  const stillCurrent = refreshPublishGuard();
  refs.publishDestination.textContent = `${branch} → ${remote}/${branch}`; refs.publishCommits.innerHTML = '<div class="loading-row"><i class="spinner"></i>Checking server state…</div>';
  const [publish, risks] = invoke
    ? await Promise.all([
        invoke('publish_status', { repositoryPath: state.repository.path, branch, remote }),
        invoke('submodule_publish_risks', { repositoryPath: state.repository.path, branch, remote, uptoCommit: '' }).catch(() => []),
      ])
    : [{ branch, remote, commits: previewData.commits.slice(0, 2) }, []];
  if (!stillCurrent() || refs.publishBranch.value !== branch || refs.publishRemote.value !== remote) return; // repository changed, or the dialog's own selection moved on
  state.publish = publish;
  publishSubmoduleRisksHtml = submodulePublishRisksHtml(risks);
  const comparison = publishAheadBehindText(publish);
  refs.publishDestination.textContent = `${branch} → ${remote}/${branch}${comparison ? ` · ${comparison}` : ''}`;
  renderPublishCommits();
  refs.publishBadge.textContent = state.publish.commits.length;
  refs.publishSubtitle.textContent = state.publish.commits.length
    ? `${state.publish.commits.length} local commit${state.publish.commits.length === 1 ? '' : 's'} to publish · ${comparison}`
    : `Everything is on the server · ${comparison}`;
  updatePublishSummary();
}

// Submodule-publish-safety report, point 3: publish_branch now runs a
// safety preflight itself (never bypassable by skipping some separate
// advisory step) that can fail in two different ways — a submodule that DOES
// have a remote but simply hasn't been pushed yet always hard-blocks (no
// override exists: push it, there's no other safe option), while a submodule
// with no remote at all (or one this app couldn't even open to check) is
// override-eligible, marked with a fixed prefix this function looks for and
// strips before showing anything.
const UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX = 'UNPUSHED_SUBMODULE_OVERRIDABLE::';
async function confirmPublish(event, overrideUnpushedSubmodules = false) {
  event.preventDefault(); if (!state.publish?.commits.length) return;
  const operation = $('#publishOperationStatus'); operation.textContent = `Publishing ${state.publish.branch}…`; operation.className = 'submodule-operation-status busy';
  try {
    $('#confirmPublish').disabled = true; status(`Publishing ${state.publish.branch}…`, 'busy');
    await invoke('publish_branch', { repositoryPath: state.repository.path, branch: state.publish.branch, remote: state.publish.remote, username: $('#publishUsername').value.trim(), accessToken: $('#publishToken').value, uptoCommit: state.publishUpto || '', overrideUnpushedSubmodules });
    $('#publishToken').value = ''; refs.publishDialog.close(); await loadRepository(state.repository.path, { keepPath: true });
    const msg = state.publishUpto ? `Published part of ${state.publish.branch} to ${state.publish.remote} (up to your chosen commit).` : `Published ${state.publish.branch} to ${state.publish.remote}`;
    status(msg); showOperationToast(msg);
  } catch (error) {
    const message = String(error);
    if (message.startsWith(UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX)) {
      const detail = message.slice(UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX.length);
      $('#confirmPublish').disabled = false;
      const proceed = await customConfirm(`${detail}\n\nPublish anyway?`, { title: 'Local-only submodule commit', danger: true, okLabel: 'Publish anyway' });
      if (proceed) return confirmPublish(event, true);
      operation.textContent = 'Publish cancelled'; operation.className = 'submodule-operation-status'; status('Publish cancelled');
      return;
    }
    operation.textContent = message; operation.className = 'submodule-operation-status error'; status(message, 'error'); $('#confirmPublish').disabled = false;
  }
}

// Rapid checkbox clicking (e.g. "Stage all" material, ticked one-by-one, or
// just working through a long change list) used to fire one stage_files/
// unstage_files call *and* one full repository reload per click, with
// nothing serializing them — a Windows perf log showed exactly this in the
// wild: 19 stage_files calls fired in quick succession, only 5 of them ever
// completed. The rest failed silently mid-way through (almost certainly
// libgit2 unable to acquire .git/index.lock against another concurrent
// call). The backend now serializes all of these per-repository regardless
// (see repo_write_lock), but that alone still means one real round trip and
// one full reload per click — grouping rapid clicks into a single batched
// call and a single reload is both faster and avoids relying on the lock to
// paper over a burst of many redundant round trips.
let pendingToggles = new Map(); // path -> desired checked state
let pendingToggleRepo = null; // which repository's index the queued paths belong to
let pendingToggleTimeout = null;
let pendingToggleGeneration = 0;
// Promise of the single currently-running flush loop, or null when none is
// active. The ONLY two places that ever assign to this are ensureFlushRunning
// (starts one) and flushLoop's own `finally` (clears it once the loop is
// genuinely drained — nothing left queued, not just "this one batch done").
// No other code path touches it, specifically to close a real race that used
// to exist here: the debounce timer used to assign a fresh
// flushPendingToggles() call directly to this variable every time it fired —
// two toggles a little over 300ms apart, with the first batch's backend call
// still in flight, meant the second timer's assignment silently overwrote
// the first's promise reference here. A caller awaiting this variable at
// that exact moment would only ever wait for the second batch, with no way
// to know whether the first's backend call — and its own reload — had
// actually finished. flushLoop's while-loop below is what replaces that:
// one loop drains everything, including anything queued while it's already
// running, so there is never a second, overlapping flush to race with.
let pendingToggleFlight = null;
// Promise of whichever backend stage/unstage call — a checkbox flush, Stage
// all, or Unstage all — is currently in flight, regardless of which of the
// three it is. Commit awaits this (with visible "Waiting for staging…"
// feedback) instead of racing it, so a commit can never run against an
// index a moment away from still catching up with a click that already
// happened.
let activeStagingOperation = null;

// A last-resort safety net for flushPendingTogglesNow, not a normal code
// path — under ordinary conditions a flush finishes in well under a second.
// If this ever fires, the real backend stage/unstage call is NOT cancelled
// and its eventual outcome is NOT assumed either way; it keeps running
// completely on its own. This only stops the *caller* from waiting forever
// and lets it refuse to proceed (e.g. switching repositories, committing)
// with a visible message instead of hanging silently with no explanation.
const STAGE_FLUSH_TIMEOUT_MS = 20000;

function toggleStage(path, checked) {
  if (!invoke || !state.repository) return;
  // Defensive: the queue should never carry entries from a repository that's
  // no longer open (every path that switches repository/branch is expected
  // to flush first, via flushPendingTogglesNow — this only guards against
  // some other path having missed that).
  if (pendingToggleRepo && pendingToggleRepo !== state.repository.path) pendingToggles = new Map();
  pendingToggleRepo = state.repository.path;
  // Reflect the click immediately in state — the round trip to the backend
  // and back through a full repository reload takes a moment, and the
  // checkbox should never visibly sit in a stale state (still "Staged" right
  // after unchecking it) while that's in flight.
  const change = state.changes.find(item => item.path === path); if (change) change.staged = checked;
  renderChanges();
  pendingToggles.set(path, checked);
  jsPerfLog(`toggleStage queued ${path}=${checked} (pendingToggles.size=${pendingToggles.size})`, 0);
  if (pendingToggleTimeout) clearTimeout(pendingToggleTimeout);
  pendingToggleTimeout = setTimeout(() => { pendingToggleTimeout = null; ensureFlushRunning(); }, 300);
}

// Starts the single flush loop if none is currently running; a no-op if one
// already is — the running loop's own while-loop (see flushLoop) picks up
// anything just added to pendingToggles on its next iteration, so nothing
// queued while a flush is already in progress is ever dropped or needs a
// second, overlapping flush of its own.
function ensureFlushRunning(options = {}) {
  if (!pendingToggleFlight) pendingToggleFlight = flushLoop(options);
  return pendingToggleFlight;
}

async function flushLoop(options) {
  const startedAt = performance.now();
  jsPerfLog(`flushLoop START (pendingToggles.size=${pendingToggles.size})`, 0);
  try {
    // Drains for as long as new toggles keep showing up — e.g. a click
    // arriving while the previous batch's backend call was still in flight.
    // Whoever is awaiting pendingToggleFlight is guaranteed every toggle
    // queued before or during this loop has actually reached the backend by
    // the time it resolves, not just whatever happened to be queued the
    // moment the loop started.
    while (pendingToggles.size) { await flushOneBatch(options); }
  } finally {
    // The one and only place pendingToggleFlight is ever cleared — on every
    // exit path (the loop draining cleanly, or a batch throwing out of it) —
    // so no exit can leave a stale promise reference sitting behind a truthy
    // pendingToggleFlight for a later caller to wait on forever.
    pendingToggleFlight = null;
    jsPerfLog(`flushLoop END (${(performance.now() - startedAt).toFixed(0)}ms)`, 0);
  }
}

async function flushOneBatch(options) {
  const batch = pendingToggles; pendingToggles = new Map();
  const repositoryPath = pendingToggleRepo; pendingToggleRepo = null;
  const toStage = [...batch].filter(([, checked]) => checked).map(([path]) => path);
  const toUnstage = [...batch].filter(([, checked]) => !checked).map(([path]) => path);
  const stillSameRepo = () => state.repository?.path === repositoryPath;
  const folder = state.currentPath;
  const generation = ++pendingToggleGeneration;
  jsPerfLog(`flushOneBatch START (stage=${toStage.length}, unstage=${toUnstage.length}, generation=${generation})`, 0);
  const batchStarted = performance.now();
  const run = (async () => {
    if (toStage.length) await invoke('stage_files', { path: repositoryPath, files: toStage });
    if (toUnstage.length) await invoke('unstage_files', { path: repositoryPath, files: toUnstage });
  })();
  activeStagingOperation = run;
  try {
    await run;
    if (activeStagingOperation === run) activeStagingOperation = null;
    jsPerfLog(`flushOneBatch backend calls done (generation=${generation}, ${(performance.now() - batchStarted).toFixed(0)}ms)`, 0);
    // A newer batch already started (and will do its own reload) while this
    // one's backend calls were in flight — its reload covers this too.
    if (generation !== pendingToggleGeneration) { jsPerfLog(`flushOneBatch generation mismatch, skipping reload (was ${generation}, now ${pendingToggleGeneration})`, 0); return; }
    // Stage/unstage only ever changes status — never branches, history,
    // stashes, or anything a submodule-gitlink reconciliation pass would
    // catch, so a full loadRepository (which redoes all of that) was
    // unnecessary work paid on every single checkbox click. Commit calls
    // flushPendingTogglesNow({ skipReload: true }) right before doing its
    // own commit-then-reload, so a checkbox ticked just before Commit was
    // clicked doesn't trigger two reloads back to back.
    if (stillSameRepo() && !options.skipReload) {
      refreshStatusAndFolderInBackground(repositoryPath, folder, `checkbox generation=${generation}`);
      jsPerfLog(`flushOneBatch scheduled background refresh (generation=${generation})`, 0);
    }
  } catch (error) {
    if (activeStagingOperation === run) activeStagingOperation = null;
    jsPerfLog(`flushOneBatch ERROR (generation=${generation}): ${String(error)}`, 0);
    // A partial failure inside this batch (the stage call succeeded but the
    // following unstage call then failed, say) means we genuinely don't know
    // which half actually landed on disk — blindly flipping every optimistic
    // bit in the batch back could just as easily show the wrong state as
    // leave it right. An authoritative status refresh replaces every guess
    // with whatever git actually has, instead — refreshStatusAndFolder
    // specifically, NOT loadRepository: loadRepository itself calls
    // flushPendingTogglesNow first, and pendingToggleFlight is still set to
    // *this exact flushLoop's own promise* at this point (only cleared once
    // the whole loop drains, in flushLoop's finally) — calling something
    // that awaits it here would deadlock this recovery on itself until the
    // safety-net timeout. Status is all a stage/unstage failure could ever
    // have left uncertain anyway, same as the success path just above.
    if (stillSameRepo()) { try { await refreshStatusAndFolder(repositoryPath, state.currentPath); } catch { /* handleError below still reports the original failure */ } }
    const message = handleError(error);
    showOperationToast(`Stage/Unstage failed: ${message}`, 'error');
  }
}

// Call this before anything that must not race a still-pending stage/unstage
// batch — Commit, Stage all, Unstage all, switching branch, or opening a
// different repository. Cancels the debounce timer and waits for the flush
// loop (starting one first if a toggle is queued but no loop is running yet)
// to fully drain, so e.g. Commit never runs against an index that's missing
// a checkbox click from a moment ago just because 300ms hadn't elapsed yet.
//
// `context` only affects the message on the safety-net timeout below — it
// never changes normal behavior, and normal behavior (a flush that finishes
// in well under a second) is the only thing that runs in practice.
async function flushPendingTogglesNow(options = {}, context = 'the previous Stage/Unstage operation') {
  if (pendingToggleTimeout) { clearTimeout(pendingToggleTimeout); pendingToggleTimeout = null; }
  const flight = pendingToggles.size ? ensureFlushRunning(options) : pendingToggleFlight;
  jsPerfLog(`flushPendingTogglesNow START (pendingToggles.size=${pendingToggles.size}, hasFlight=${!!flight}, activeStagingOperation=${!!activeStagingOperation})`, 0);
  if (!flight) { jsPerfLog('flushPendingTogglesNow END (nothing pending)', 0); return; }
  const startedAt = performance.now();
  let timedOut = false;
  let timeoutHandle;
  const timeoutPromise = new Promise(resolve => { timeoutHandle = setTimeout(() => { timedOut = true; resolve(); }, STAGE_FLUSH_TIMEOUT_MS); });
  await Promise.race([flight, timeoutPromise]);
  clearTimeout(timeoutHandle);
  jsPerfLog(`flushPendingTogglesNow END (${timedOut ? 'TIMEOUT' : 'done'}, ${(performance.now() - startedAt).toFixed(0)}ms)`, 0);
  if (timedOut) {
    // pendingToggleFlight is deliberately left exactly as it is — it still
    // points at the real, still-running flight, and nothing here knows its
    // outcome yet, so nothing here is "demonstrably stale" and safe to
    // clear. Only this caller is told to stop waiting and refuse to proceed.
    throw new Error(`Cannot continue: ${context} has not finished yet. Please wait a moment and try again.`);
  }
}

async function switchBranch(branch, button = null) {
  const context = activeRepositoryContext();
  if (!invoke || !context.path) return;
  const finishButton = beginButtonOperation(button, 'Switching…');
  try {
    // Must land before the checkout itself, not just before the reload after
    // it — a checkout racing a still-pending stage/unstage call is exactly
    // the kind of thing this exists to prevent. Inside this try (not before
    // it, as before): the safety-net timeout must surface as a visible
    // error, not an unhandled rejection that silently leaves the branch
    // switch never attempted.
    await flushPendingTogglesNow({}, 'the Stage operation');
    status(`Switching to ${branch}…`, 'busy');
    if (context.isSubmodule) {
      // Never the plain switch_branch here: a submodule's own checkout is
      // only half the operation — switch_submodule_version is what also
      // records the new commit in the parent's index right after,
      // otherwise the submodule would immediately show as "modified"
      // against a pointer the parent never actually asked for. Reusing
      // openSubmoduleGraph for the refresh keeps this on the exact same,
      // already-correct path that opening the Branch Map itself uses —
      // never a hand-rolled partial state update that could drift from it.
      await invoke('switch_submodule_version', { repositoryPath: context.parentPath, relativePath: context.relativePath, revision: branch, versionKind: 'branch', name: branch });
      await openSubmoduleGraph({ relative_path: context.relativePath, name: context.name });
    } else {
      await invoke('switch_branch', { path: context.path, branch });
      await loadRepository(context.path);
    }
  }
  catch (error) { handleError(error); }
  finally { finishButton(); }
}

$('#openRepo').addEventListener('click', (event) => { event.stopPropagation(); toggleRepoPickerMenu(); });
$('#repoMenuOpen').addEventListener('click', () => { closeRepoPickerMenu(); openRepository(); });
$('#repoMenuClone').addEventListener('click', () => { closeRepoPickerMenu(); openCloneDialog(); });
$('#repoMenuCreate').addEventListener('click', () => { closeRepoPickerMenu(); createRepositoryFromPicker(); });
document.addEventListener('click', (event) => { if (!event.target.closest('#openRepo') && !event.target.closest('#repoPickerMenu')) closeRepoPickerMenu(); });
$('#emptyOpen').addEventListener('click', openRepository);
$('#cloneRepo').addEventListener('click', openCloneDialog); $('#chooseCloneParent').addEventListener('click', () => chooseCloneParent().catch(error => status(String(error), 'error'))); refs.confirmClone.addEventListener('click', confirmClone);
refs.cloneUrl.addEventListener('input', () => { if (!refs.cloneName.dataset.edited) { const inferred = refs.cloneUrl.value.trim().split(/[\\/]/).pop()?.replace(/\.git$/, '') || ''; refs.cloneName.value = inferred; } validateCloneForm(); }); refs.cloneParent.addEventListener('input', validateCloneForm); refs.cloneName.addEventListener('input', () => { refs.cloneName.dataset.edited = refs.cloneName.value ? '1' : ''; validateCloneForm(); });
$('#addSubmodule').addEventListener('click', openAddSubmoduleDialog); refs.confirmAddSubmodule.addEventListener('click', confirmAddSubmodule);
refs.submoduleUrl.addEventListener('input', () => { if (!refs.submoduleName.dataset.edited) refs.submoduleName.value = suggestedRepositoryName(refs.submoduleUrl.value); validateSubmoduleForm(); });
refs.submoduleName.addEventListener('input', () => { refs.submoduleName.dataset.edited = refs.submoduleName.value ? '1' : ''; validateSubmoduleForm(); });
$('#refresh').addEventListener('click', event => refreshRepository(event.currentTarget));
// Opening the drawer with everything already selected (staged) is what most
// people expect from "here's what changed, commit it" — having to manually
// tick every file first before Commit even becomes clickable read as "commit
// doesn't work". This only stages what's genuinely unstaged in scope (real
// `git add`, so the checkbox state stays truthful); anything you uncheck
// afterward gets unstaged again, same as before.
async function stageAllInScope(scope) {
  if (!invoke || !state.repository) return;
  const button = $('#stageAllButton'); const label = button.textContent;
  let run = null;
  try {
    // A pending per-checkbox toggle must land first — otherwise it could
    // overlap with, or race, stage_all's own backend call. Before the button
    // is even disabled, and inside this try: if the flush can't finish (the
    // safety-net timeout), the button must never end up stuck disabled with
    // no explanation — the catch below reports it and the finally restores it.
    await flushPendingTogglesNow({}, 'the previous Stage operation');
    button.disabled = true; button.textContent = 'Staging…';
    // stage_all reads the real, current disk state itself (see its own doc
    // comment) instead of trusting state.changes, which can already be stale
    // by the time this button is pressed — files added or removed from
    // outside the app since the last load wouldn't be in it at all.
    run = (async () => {
      try { return await invoke('stage_all', { repositoryPath: state.repository.path, scope }); }
      finally { button.textContent = label; }
    })();
    activeStagingOperation = run;
    // The backend tells apart what genuinely landed in the index
    // (staged_paths) from a submodule that was only dirty *inside* it, with
    // no real gitlink change to record (skipped_dirty_submodules) — a
    // parent-level Stage All can never stage a submodule's own uncommitted
    // content, only a new commit pointer, so this is never silently folded
    // into "success" the way a plain count used to.
    const result = await run;
    jsPerfLog(`stageAllInScope result (staged=${result.staged_paths.length}, skipped_dirty_submodules=${result.skipped_dirty_submodules.length})`, 0);
    if (!result.staged_paths.length && !result.skipped_dirty_submodules.length) return;
    let appliedMsg = null;
    if (!result.staged_paths.length && result.skipped_dirty_submodules.length) {
      const names = result.skipped_dirty_submodules.map(p => p.split('/').pop()).join(', ');
      appliedMsg = `${result.skipped_dirty_submodules.length} submodule${result.skipped_dirty_submodules.length === 1 ? '' : 's'} (${names}) contain${result.skipped_dirty_submodules.length === 1 ? 's' : ''} internal changes. Nothing was staged in the parent project. Open each submodule and use Stage/Commit submodule.`;
      status(appliedMsg, 'error'); showOperationToast(appliedMsg, 'error');
      jsPerfLog(`stageAllInScope UI message: ${appliedMsg}`, 0);
      return;
    }
    if (result.skipped_dirty_submodules.length) {
      const names = result.skipped_dirty_submodules.map(p => p.split('/').pop()).join(', ');
      appliedMsg = `Staged ${result.staged_paths.length} item${result.staged_paths.length === 1 ? '' : 's'}. ${result.skipped_dirty_submodules.length} submodule${result.skipped_dirty_submodules.length === 1 ? '' : 's'} (${names}) still ${result.skipped_dirty_submodules.length === 1 ? 'has' : 'have'} internal changes only — open ${result.skipped_dirty_submodules.length === 1 ? 'it' : 'them'} and use Stage/Commit submodule.`;
      status(appliedMsg); showOperationToast(appliedMsg);
    }
    if (appliedMsg) jsPerfLog(`stageAllInScope UI message: ${appliedMsg}`, 0);
    // Stage All only ever changes status — never branches, history, stashes,
    // or a submodule-gitlink reconciliation pass, so a full loadRepository
    // was unnecessary work paid on every click.
    const refreshStarted = performance.now();
    await refreshStatusAndFolder(state.repository.path, state.currentPath);
    jsPerfLog('stageAllInScope refreshStatusAndFolder', performance.now() - refreshStarted);
  } catch (error) { handleError(error); }
  finally { if (activeStagingOperation === run) activeStagingOperation = null; button.disabled = false; renderChanges(); }
}

async function unstageAllInScope(scope) {
  if (!invoke || !state.repository) return;
  const button = $('#unstageAllButton'); const label = button.textContent;
  let run = null;
  try {
    // See stageAllInScope's identical comment: inside this try, before the
    // button is disabled, so the safety-net timeout can never leave it stuck.
    await flushPendingTogglesNow({}, 'the previous Stage operation');
    const files = state.changes.filter(change => change.staged && (!scope || change.path === scope || change.path.startsWith(`${scope}/`))).map(change => change.path);
    if (!files.length) return;
    button.disabled = true; button.textContent = `Unstaging ${files.length} file${files.length === 1 ? '' : 's'}…`;
    run = invoke('unstage_files', { path: state.repository.path, files }).finally(() => { button.textContent = label; });
    activeStagingOperation = run;
    await run;
    // Unstage all only ever changes status — same reasoning as
    // flushOneBatch/stageAllInScope, a full loadRepository was unnecessary.
    await refreshStatusAndFolder(state.repository.path, state.currentPath);
  }
  catch (error) { handleError(error); }
  finally { if (activeStagingOperation === run) activeStagingOperation = null; button.disabled = false; renderChanges(); }
}
// Pre-fills the commit message box with whatever is in the "default commit
// message" field up top — e.g. a Polarion ID you're committing several
// pieces of work against — every time the Working tree drawer is opened.
// Only fills it in when the box is empty, so it never clobbers a message
// you're already partway through typing in a still-open drawer.
let lastAppliedDefaultCommitMessage = '';
function applyDefaultCommitMessage() {
  if (!refs.commitMessage.value.trim() && refs.defaultCommitMessage.value.trim()) {
    lastAppliedDefaultCommitMessage = refs.defaultCommitMessage.value.trim();
    refs.commitMessage.value = lastAppliedDefaultCommitMessage;
  }
}
function syncOpenDrawerDefaultCommitMessage() {
  if (!refs.changesDrawer.classList.contains('open')) return;
  const next = refs.defaultCommitMessage.value.trim();
  const current = refs.commitMessage.value.trim();
  // If the drawer is already open, keep it in sync only while it is still
  // using the old default (or is empty). A manually typed drawer message is
  // deliberately left alone.
  if (!current || current === lastAppliedDefaultCommitMessage) {
    lastAppliedDefaultCommitMessage = next;
    refs.commitMessage.value = next;
    renderChanges();
  }
}

// Remembers every default commit message you've typed in, most recent
// first, capped at 10 — persisted in this browser profile so it survives a
// restart. Shown as a native dropdown under the field (no extra UI needed).
const COMMIT_MESSAGE_HISTORY_KEY = 'git-integrity-default-commit-message-history';
function loadCommitMessageHistory() { try { const list = JSON.parse(localStorage.getItem(COMMIT_MESSAGE_HISTORY_KEY) || '[]'); return Array.isArray(list) ? list.slice(0, 10) : []; } catch { return []; } }
state.commitMessageHistory = loadCommitMessageHistory();
function renderCommitMessageHistory() { $('#defaultCommitMessageHistory').innerHTML = state.commitMessageHistory.map(msg => `<option value="${esc(msg)}"></option>`).join(''); }
function recordDefaultCommitMessage() {
  const trimmed = refs.defaultCommitMessage.value.trim(); if (!trimmed) return;
  state.commitMessageHistory = [trimmed, ...state.commitMessageHistory.filter(item => item !== trimmed)].slice(0, 10);
  try { localStorage.setItem(COMMIT_MESSAGE_HISTORY_KEY, JSON.stringify(state.commitMessageHistory)); } catch { /* private-browsing or storage disabled — history just won't persist */ }
  renderCommitMessageHistory();
}
renderCommitMessageHistory();

// Opening the drawer no longer auto-stages everything in it — it used to
// (every open silently staged every unstaged change in scope), which meant
// unstaging a file you didn't want in the commit was pointless the moment
// you closed and reopened the drawer, or ran any action that reopens it: it
// came right back. Staging is now only ever something you ask for, via the
// checkboxes or the explicit "Stage all" button below.
// The app can't watch the filesystem itself, so a file copied in from
// outside only ever showed up after a full manual Refresh or some unrelated
// action's own reload. This re-checks status — nothing else, no branches/
// commits/submodule sync — at the one moment it matters most: opening the
// drawer that's specifically about "what changed". Fires after the drawer
// is already shown with whatever data was already there (never blocks
// opening it), and fails silently — this is a best-effort background
// top-up, not a user-initiated action worth its own error toast.
const refreshChangesLightweightGuard = createRequestGuard();
async function refreshChangesLightweight() {
  if (!invoke || !state.repository) return;
  const repositoryPath = state.repository.path;
  const stillCurrent = refreshChangesLightweightGuard();
  try {
    const changes = await invoke('refresh_status', { repositoryPath });
    // state.repository.path itself (not just stillCurrent()) — a different
    // repository could have been opened at the very same path a stale
    // response would otherwise still match on identity alone.
    if (stillCurrent() && state.repository?.path === repositoryPath) { state.changes = changes; updateChangeBadge(); if (refs.changesDrawer.classList.contains('open')) renderChanges(); }
  } catch { /* best-effort — a manual Refresh remains the explicit fallback */ }
}

let postStageRefreshSeq = 0;

async function refreshStatusAndFolderInBackground(repositoryPath, folder, reason = 'stage') {
  if (!invoke) return;
  const seq = ++postStageRefreshSeq;
  const startedAt = performance.now();
  jsPerfLog(`postStageRefresh START (${reason}, ${folder || '/'})`, 0);
  try {
    const changes = await invoke('refresh_status', { repositoryPath });
    if (seq !== postStageRefreshSeq || state.repository?.path !== repositoryPath) {
      jsPerfLog(`postStageRefresh END (${reason}, stale after status)`, performance.now() - startedAt);
      return;
    }
    state.changes = changes; state.statusReady = true; updateChangeBadge();
    if (refs.changesDrawer.classList.contains('open')) renderChanges();
    if (state.currentPath === folder) await openDirectory(folder, { force: true });
    jsPerfLog(`postStageRefresh END (${reason}, applied)`, performance.now() - startedAt);
  } catch (error) {
    jsPerfLog(`postStageRefresh ERROR (${reason}): ${String(error)}`, performance.now() - startedAt);
  }
}

// What an ordinary Stage/Unstage/single-file-restore actually needs to
// reflect afterward: current status and the folder on screen — never
// branches, commit history, stashes, or a submodule-gitlink reconciliation
// pass, none of which a plain staging change can affect. Replaces a full
// loadRepository() in exactly those flows; anything that genuinely can
// touch history/refs (a commit, a branch switch, a fetch) keeps using
// loadRepository as before. openDirectory's own force (not invalidateGit)
// reuses the status this refresh_status call just computed instead of
// paying for a second backend scan right behind it — same reasoning as
// every other post-mutation repaint in this file.
async function refreshStatusAndFolder(repositoryPath, folder) {
  const changes = await invoke('refresh_status', { repositoryPath });
  if (state.repository?.path !== repositoryPath) return; // switched to a different repository while this was in flight
  state.changes = changes; state.statusReady = true; updateChangeBadge();
  if (refs.changesDrawer.classList.contains('open')) renderChanges();
  await openDirectory(folder, { force: true });
}

$('#showChanges').addEventListener('click', () => { state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); refreshChangesLightweight(); });
$('#showFolderChanges').addEventListener('click', () => { state.changesScope = 'folder'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); refreshChangesLightweight(); });
$('#stageAllButton').addEventListener('click', () => stageAllInScope(state.changesScope === 'folder' ? state.currentPath : ''));
$('#unstageAllButton').addEventListener('click', () => unstageAllInScope(state.changesScope === 'folder' ? state.currentPath : ''));
refs.defaultCommitMessage.addEventListener('input', () => {
  const match = refs.defaultCommitMessage.value.match(/P:([A-Za-z0-9][A-Za-z0-9_]*-\d+)/);
  if (match) { const workitemId = match[1]; const project = workitemId.split('-')[0]; refs.defaultCommitPolarionLink.href = `https://polarion.vitesco.io/polarion/#/project/${project}/workitem?id=${workitemId}`; refs.defaultCommitPolarionLink.hidden = false; }
  else { refs.defaultCommitPolarionLink.hidden = true; }
  syncOpenDrawerDefaultCommitMessage();
});
refs.defaultCommitMessage.addEventListener('change', recordDefaultCommitMessage);
$('#showPublish').addEventListener('click', () => openPublish().catch(error => status(String(error), 'error')));
$('#stashWork').addEventListener('click', stashWork);
$('#popStash').addEventListener('click', popStash);
$('#refreshStashes').addEventListener('click', () => refreshStashesList());
$('#closeChanges').addEventListener('click', () => refs.changesDrawer.classList.remove('open'));
let searchTimeout; refs.search.addEventListener('input', () => { clearTimeout(searchTimeout); searchTimeout = setTimeout(() => { if (state.view === 'explorer') renderExplorer(); else if (state.view === 'commander' && state.compareMode === 'local-drive') localDriveWorkspace?.setFilter(refs.search.value); else if (state.view === 'commander') renderCommander(); else renderGraph(); }, 200); }); refs.commitMessage.addEventListener('input', renderChanges);
async function returnToProjectNavigator() {
  closeSubmoduleGraph();
  const selectedPath = state.selectedEntry?.relative_path;
  state.view = 'explorer'; state.selectedCommit = null; refs.search.value = ''; render();
  if (state.localDriveGitRefreshPending && invoke && state.repository) {
    state.localDriveGitRefreshPending = false;
    try {
      status('Refreshing Git after Local Drive changes…', 'busy');
      state.changes = await invoke('refresh_status', { repositoryPath: state.repository.path });
      state.statusReady = true;
      directoryCache.clear();
      await openDirectory(state.currentPath, { force: true });
      status('Git status refreshed');
    } catch (error) { handleError(error); }
  }
  if (selectedPath) selectEntry(selectedPath); else clearDetails('Select a file or folder');
}
$('#navExplorer').addEventListener('click', returnToProjectNavigator);
function syncLocalDriveToCommanderPath() {
  if (state.repository && state.view === 'commander' && state.compareMode === 'local-drive') {
    localDriveWorkspace?.openRepositoryLocation(state.repository.path, state.commanderPath || '');
  }
}
$('#navCommander').addEventListener('click', () => {
  const submoduleContext = currentSubmoduleCompareContext();
  closeSubmoduleGraph();
  const selected = state.selectedEntry;
  const nextCommanderPath = selected?.kind === 'file' ? parentPathOf(selected.relative_path) : selected?.kind === 'folder' ? selected.relative_path : state.currentPath;
  state.commanderFocus = selected?.kind === 'file' ? selected.relative_path : '';
  state.commanderRows = [];
  refs.search.value = '';
  if (submoduleContext) {
    state.compareMode = 'submodule';
    return openSubmoduleCompareFromEntry(submoduleContext.entry, { innerPath: submoduleContext.innerPath });
  }
  if (state.compareMode !== 'submodule') state.commanderPath = nextCommanderPath;
  state.view = 'commander';
  render();
  if (state.compareMode === 'git') openCommanderDirectory(state.commanderPath);
  else if (state.compareMode === 'submodule') openSubmoduleCompareDirectory(state.commanderPath);
  else syncLocalDriveToCommanderPath();
});
refs.compareModeGit.addEventListener('click', () => { if (state.compareMode === 'git') return; state.compareMode = 'git'; refs.search.value = ''; render(); openCommanderDirectory(state.commanderPath); });
refs.compareModeDrive.addEventListener('click', () => { if (state.compareMode === 'local-drive') return; state.compareMode = 'local-drive'; refs.search.value = ''; render(); syncLocalDriveToCommanderPath(); });
refs.compareModeSubmodule.addEventListener('click', () => {
  if (state.compareMode === 'submodule') return;
  state.compareMode = 'submodule';
  refs.search.value = '';
  const submoduleContext = currentSubmoduleCompareContext();
  if (submoduleContext) return openSubmoduleCompareFromEntry(submoduleContext.entry, { innerPath: submoduleContext.innerPath });
  state.commanderPath = state.submoduleCompare?.submodulePath ? state.commanderPath : '';
  render();
  if (state.submoduleCompare?.submodulePath) openSubmoduleCompareDirectory(state.commanderPath);
});
$('#navGraph').addEventListener('click', () => { closeSubmoduleGraph(); state.commits = state.allCommits.length ? state.allCommits : state.commits; state.historyScope = ''; state.historyKind = ''; state.selectedEntry = null; state.selectedCommit = null; state.view = 'graph'; refs.search.value = ''; clearDetails('Select a commit'); render(); });
$('#navRemotes').addEventListener('click', () => { closeSubmoduleGraph(); loadRemotes(); });
refs.leaveSubmoduleGraph.addEventListener('click', leaveSubmoduleGraph);
// Covers window/pane resizes (a narrower details panel, dragging the app
// window, DevTools opening) the same way the stash-toggle case above covers
// content-driven height changes — anything that could leave the SVG
// overlay's lines/dots pointing at stale row positions gets the same
// debounced redraw, never a full graph rebuild just to fix alignment.
if (typeof ResizeObserver !== 'undefined') { new ResizeObserver(() => scheduleGraphOverlayRedraw()).observe(refs.graph); }
(() => {
  const mainLayout = document.querySelector('.main-layout'); const toggle = $('#toggleDetailsPanel');
  const collapsed = localStorage.getItem('detailsPanelCollapsed') === '1';
  if (collapsed) { mainLayout.classList.add('details-collapsed'); toggle.title = 'Expand panel'; }
  toggle.addEventListener('click', () => {
    const isCollapsed = mainLayout.classList.toggle('details-collapsed');
    localStorage.setItem('detailsPanelCollapsed', isCollapsed ? '1' : '0');
    toggle.title = isCollapsed ? 'Expand panel' : 'Collapse panel';
    toggle.setAttribute('data-tooltip', isCollapsed ? 'Expand details panel' : 'Collapse details panel');
  });
})();
(() => {
  const heading = $('#toggleBranchList'); const arrow = heading.querySelector('.branch-heading-arrow');
  const expanded = localStorage.getItem('branchListExpanded') === '1';
  if (expanded) { refs.branches.classList.remove('collapsed'); arrow.textContent = '▾'; }
  heading.addEventListener('click', event => {
    if (event.target.closest('#newBranch')) return;
    const isExpanded = refs.branches.classList.toggle('collapsed') === false;
    localStorage.setItem('branchListExpanded', isExpanded ? '1' : '0');
    arrow.textContent = isExpanded ? '▾' : '▸';
  });
})();
$('#saveFile').addEventListener('click', saveEditor); $('#closeEditor').addEventListener('click', () => refs.editorDialog.close()); $('#cancelEditor').addEventListener('click', () => refs.editorDialog.close());
refs.editorContent.addEventListener('input', updateEditorSaveState);
$('#fetchCurrent').addEventListener('click', event => { const first = state.remotes[0]?.name || state.branches.find(branch => branch.remote)?.name.split('/')[0]; if (first) fetchRemote(first, event.currentTarget); else status('No remote is configured', 'error'); });
$('#fetchAll').addEventListener('click', event => fetchAllRemotes(event.currentTarget));
$('#fetchProject').addEventListener('click', event => fetchProjectAndSubmodules(event.currentTarget));
async function syncCurrent(action, button = null) {
  if (!invoke || !state.repository) return;
  const finishButton = beginButtonOperation(button, action === 'pull' ? 'Pulling…' : 'Pushing…');
  try { status(`${action === 'pull' ? 'Pulling' : 'Pushing'} ${state.repository.current_branch}…`, 'busy'); await invoke('sync_repository', { repositoryPath: state.repository.path, action }); await loadRepository(state.repository.path, { keepPath: true }); status(`${action === 'pull' ? 'Pull' : 'Push'} complete`); }
  catch (error) {
    const message = handleError(error);
    if (action === 'pull' && String(error).toLowerCase().includes('requires a merge')) {
      showOperationToast(`${message}\nUse "Merge branch…" (⑂) instead — it can resolve conflicts here.`, 'error');
    } else { showOperationToast(message, 'error'); }
  }
  finally { finishButton(); }
}
$('#pullCurrent').addEventListener('click', event => syncCurrent('pull', event.currentTarget)); $('#pushCurrent').addEventListener('click', event => syncCurrent('push', event.currentTarget));
refs.publishBranch.addEventListener('change', refreshPublish); refs.publishRemote.addEventListener('change', refreshPublish); $('#confirmPublish').addEventListener('click', confirmPublish);
[['#compareRestoreRemote','remote'],['#compareRestoreHead','head'],['#compareStage','stage'],['#compareUnstage','unstage']].forEach(([selector, action]) => $(selector)?.addEventListener('click', event => { event.preventDefault(); updateRecoveryHelp(action); applyFileRecovery(action).catch(error => handleError(error)); }));
document.addEventListener('click', event => { const link = event.target.closest('.polarion-link'); if (!link) return; event.preventDefault(); if (invoke) invoke('open_external_url', { url: link.href }).catch(error => status(String(error), 'error')); else window.open(link.href, '_blank', 'noopener'); });
document.addEventListener('click', event => {
  const link = event.target.closest('.submodule-repository-link'); if (!link) return; event.preventDefault();
  const path = link.dataset.submodulePath;
  const entry = state.selectedEntry?.relative_path === path ? state.selectedEntry : state.entries.find(item => item.relative_path === path);
  if (entry) return openEntryOnServer(entry, true);
  if (!invoke) window.open(link.href, '_blank', 'noopener');
});
document.addEventListener('click', event => {
  const link = event.target.closest('.commit-server-link'); if (!link || !state.repository) return; event.preventDefault();
  const commitId = link.dataset.commitId;
  const subPath = link.dataset.submodulePath;
  // The backend always resolves against the *parent* repository plus an
  // optional submodule path — so a submodule commit link goes to the
  // submodule's own remote (a sibling of the parent for a relative
  // .gitmodules URL), not <parent-url>/commit/<sha>. subPath comes from the
  // Explorer submodule detail panel; state.submoduleGraph covers the graph
  // view while a submodule's own history is on screen; otherwise it's the
  // parent's own commit.
  let repositoryPath, submodulePath = null;
  if (subPath) { repositoryPath = state.repository.path; submodulePath = subPath; }
  else if (state.submoduleGraph) { repositoryPath = state.submoduleGraph.parentRepositoryPath || state.repository.path; submodulePath = state.submoduleGraph.relativePath || null; }
  else { repositoryPath = state.repository.path; }
  if (!invoke) return status(`Preview: open commit ${commitId.slice(0, 8)} on server`);
  invoke('open_commit_on_server', { repositoryPath, commitId, submodulePath }).catch(error => handleError(error));
});
refs.goUp.addEventListener('click', () => { const commander = state.view === 'commander'; const parts = (commander ? state.commanderPath : state.currentPath).split('/').filter(Boolean); parts.pop(); if (!commander) return openDirectory(parts.join('/')); return state.compareMode === 'submodule' ? openSubmoduleCompareDirectory(parts.join('/')) : openCommanderDirectory(parts.join('/')); });
refs.reloadFolder.addEventListener('click', () => {
  if (state.view === 'commander' && state.compareMode === 'local-drive') return localDriveWorkspace?.reload();
  if (state.view === 'commander' && state.compareMode === 'submodule') return openSubmoduleCompareDirectory(state.commanderPath);
  if (state.view === 'commander') return openCommanderDirectory(state.commanderPath);
  return openDirectory(state.currentPath, { force: true, invalidateGit: true });
});
refs.runCurrentUtrud.addEventListener('click', () => {
  if (state.view !== 'explorer' || !state.repository) return;
  const name = state.currentPath ? state.currentPath.split('/').filter(Boolean).at(-1) : state.repository.name;
  runUtrud({ kind: 'folder', name, relative_path: state.currentPath });
});
document.addEventListener('keydown', event => { if (event.key !== 'Escape' || state.view !== 'commander' || document.querySelector('dialog[open]')) return; event.preventDefault(); returnToProjectNavigator(); });
refs.remoteRef.addEventListener('change', () => { state.remoteRef = refs.remoteRef.value; if (state.compareMode === 'git') openCommanderDirectory(state.commanderPath); });
function applySubmoduleCompareRevisionInputs() {
  if (!state.submoduleCompare) return false;
  state.submoduleCompare.leftRef = refs.subCompareLeftRef.value.trim();
  state.submoduleCompare.rightRef = refs.subCompareRightRef.value.trim();
  return !!state.submoduleCompare.leftRef && !!state.submoduleCompare.rightRef;
}
function resolveSubmoduleCompareInput(value) {
  const query = normalizeRepositoryRelativePath(value);
  const candidates = submoduleCompareCandidates();
  if (!query) return { matches: candidates };
  const queryLower = query.toLowerCase();
  const exact = candidates.find(candidate => candidate.path.toLowerCase() === queryLower || candidate.name.toLowerCase() === queryLower);
  if (exact) return { candidate: exact };
  const matches = candidates.filter(candidate => candidate.path.toLowerCase().includes(queryLower) || candidate.name.toLowerCase().includes(queryLower));
  return matches.length === 1 ? { candidate: matches[0] } : { matches };
}
async function openSubmoduleCompareFromInput() {
  const result = resolveSubmoduleCompareInput(refs.subCompareSubmodule.value);
  if (result.candidate) return openSubmoduleCompareFromEntry(submoduleEntryForPath(result.candidate.path));
  const query = refs.subCompareSubmodule.value.trim();
  if (!query) return status('Type or choose a submodule first.', 'error');
  if (!result.matches.length) return status(`No submodule matched "${query}".`, 'error');
  return status(`${result.matches.length} submodules match "${query}". Keep typing or choose one from the dropdown.`, 'busy');
}
function submoduleRevisionOptionMatches(option, query) {
  if (!query) return true;
  const haystack = [option.value, option.label, option.kind, option.name, option.revision, option.subject, option.author, option.date]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
  return haystack.includes(query);
}
function renderSubmoduleRevisionPickerResults(results = null, note = '') {
  const picker = state.submoduleRevisionPicker;
  if (!picker) return;
  const query = refs.subRevisionSearch.value.trim().toLowerCase();
  const source = results || state.submoduleCompare?.revisionOptions || [];
  const visible = (results ? source : source.filter(option => submoduleRevisionOptionMatches(option, query))).slice(0, 160);
  refs.subRevisionHelp.textContent = note || (results
    ? `${visible.length} result${visible.length === 1 ? '' : 's'} from explicit history search.`
    : 'Loaded suggestions are instant. Use “Search all history” only when you need older commits.');
  refs.subRevisionResults.innerHTML = visible.map((option, index) => {
    const kind = option.kind === 'parent-current' ? 'parent/current' : option.kind === 'remote' ? 'remote' : option.kind || 'revision';
    const title = option.name || option.value;
    const detail = [revisionDisplay(option.revision), option.subject, option.author, option.date].filter(Boolean).join(' · ');
    return `<button type="button" class="submodule-revision-result" data-revision-index="${index}">
      <span class="revision-kind">${esc(kind)}</span>
      <strong>${esc(title)}</strong>
      <small>${esc(detail || option.label || '')}</small>
      <code>${esc(option.value)}</code>
    </button>`;
  }).join('') || '<div class="version-loading">No matches. Try “Search all history” for older commits.</div>';
  refs.subRevisionResults.querySelectorAll('[data-revision-index]').forEach(button => button.addEventListener('click', () => {
    const option = visible[Number(button.dataset.revisionIndex)];
    if (option) selectSubmoduleRevisionOption(option);
  }));
}
function openSubmoduleRevisionPicker(side) {
  const compare = state.submoduleCompare;
  if (!compare?.submodulePath) return status('Select a submodule first.', 'error');
  state.submoduleRevisionPicker = { side };
  refs.subRevisionDialogSide.textContent = side === 'left' ? 'LEFT REVISION' : 'RIGHT REVISION';
  refs.subRevisionDialogTitle.textContent = `${compare.name} · ${compare.submodulePath}`;
  refs.subRevisionSearch.value = side === 'left' ? refs.subCompareLeftRef.value.trim() : refs.subCompareRightRef.value.trim();
  renderSubmoduleRevisionPickerResults();
  refs.subRevisionDialog.showModal();
  refs.subRevisionSearch.focus();
  refs.subRevisionSearch.select();
}
function selectSubmoduleRevisionOption(option) {
  const picker = state.submoduleRevisionPicker;
  if (!picker || !state.submoduleCompare) return;
  const input = picker.side === 'left' ? refs.subCompareLeftRef : refs.subCompareRightRef;
  input.value = option.value;
  if (picker.side === 'left') state.submoduleCompare.leftRef = option.value;
  else state.submoduleCompare.rightRef = option.value;
  refs.subRevisionDialog.close();
  state.submoduleRevisionPicker = null;
  if (applySubmoduleCompareRevisionInputs()) openSubmoduleCompareDirectory(state.commanderPath || '');
}
async function searchAllSubmoduleRevisions() {
  const compare = state.submoduleCompare;
  if (!compare?.submodulePath || !invoke) return;
  const query = refs.subRevisionSearch.value.trim();
  if (query.length < 2) {
    refs.subRevisionHelp.textContent = 'Type at least 2 characters before searching all history.';
    return;
  }
  const original = refs.subRevisionSearchAll.textContent;
  refs.subRevisionSearchAll.disabled = true;
  refs.subRevisionSearchAll.textContent = 'Searching…';
  refs.subRevisionResults.innerHTML = '<div class="version-loading"><i class="spinner"></i>Searching all reachable submodule history…</div>';
  try {
    const versions = await invoke('search_submodule_revisions', { repositoryPath: state.repository.path, relativePath: compare.submodulePath, query, limit: 160 });
    const options = submoduleRevisionOptionsFromVersions(versions || []);
    renderSubmoduleRevisionPickerResults(options, `Searched all reachable history for “${query}” · ${options.length} result${options.length === 1 ? '' : 's'}.`);
  } catch (error) {
    refs.subRevisionResults.innerHTML = `<div class="version-loading">${esc(String(error))}</div>`;
    handleError(error);
  } finally {
    refs.subRevisionSearchAll.disabled = false;
    refs.subRevisionSearchAll.textContent = original;
  }
}
refs.subCompareSubmodule.addEventListener('change', () => { openSubmoduleCompareFromInput().catch(error => handleError(error)); });
refs.subCompareSubmodule.addEventListener('keydown', event => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  openSubmoduleCompareFromInput().catch(error => handleError(error));
});
refs.subComparePickLeft.addEventListener('click', () => openSubmoduleRevisionPicker('left'));
refs.subComparePickRight.addEventListener('click', () => openSubmoduleRevisionPicker('right'));
$('#closeSubRevisionDialog').addEventListener('click', () => { refs.subRevisionDialog.close(); state.submoduleRevisionPicker = null; });
refs.subRevisionDialog.addEventListener('close', () => { state.submoduleRevisionPicker = null; });
refs.subRevisionSearch.addEventListener('input', () => renderSubmoduleRevisionPickerResults());
refs.subRevisionSearch.addEventListener('keydown', event => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  searchAllSubmoduleRevisions().catch(error => handleError(error));
});
refs.subRevisionSearchAll.addEventListener('click', () => searchAllSubmoduleRevisions().catch(error => handleError(error)));
refs.subCompareRefresh.addEventListener('click', () => {
  if (!applySubmoduleCompareRevisionInputs()) return status('Choose both submodule revisions first.', 'error');
  openSubmoduleCompareDirectory(state.commanderPath || '');
});
refs.subCompareSwap.addEventListener('click', () => {
  if (!state.submoduleCompare) return;
  const left = refs.subCompareLeftRef.value.trim();
  const right = refs.subCompareRightRef.value.trim();
  state.submoduleCompare.leftRef = right;
  state.submoduleCompare.rightRef = left;
  refs.subCompareLeftRef.value = right;
  refs.subCompareRightRef.value = left;
  openSubmoduleCompareDirectory(state.commanderPath || '');
});
[refs.subCompareLeftRef, refs.subCompareRightRef].forEach(input => {
  input.addEventListener('change', () => { if (applySubmoduleCompareRevisionInputs()) openSubmoduleCompareDirectory(state.commanderPath || ''); });
  input.addEventListener('keydown', event => {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    if (applySubmoduleCompareRevisionInputs()) openSubmoduleCompareDirectory(state.commanderPath || '');
  });
});
$('#closeSubmoduleMenu').addEventListener('click', closeSubmoduleMenu);
$('#refreshSubmoduleMenu').addEventListener('click', event => refreshSubmoduleMenu(event.currentTarget));
// Uses submoduleMenuEntry (the entry the popup was actually opened for), not
// state.selectedEntry — this panel isn't modal, so the selection elsewhere in
// the app can move on while it's still open; reusing that here would be
// exactly the "leaks into a previously selected submodule" mistake the
// submodule-branch-selector report warns against (submoduleOpenGraph, right
// below, already gets this right).
$('#submoduleMenuNewBranch').addEventListener('click', () => {
  if (!submoduleMenuEntry || submoduleMenuEntry.kind !== 'submodule') return;
  const entry = submoduleMenuEntry;
  // Keep the version selector behind the modal. Cancel must return to the
  // exact branch/tag/history context the user was inspecting, rather than
  // closing both the child action and its parent selector.
  if (versionFilter === 'tag') openCreateSubmoduleTagDialog(entry); else createSubmoduleBranch(entry);
});
document.querySelectorAll('[data-version-filter]').forEach(button => button.addEventListener('click', () => {
  versionFilter = button.dataset.versionFilter; document.querySelectorAll('[data-version-filter]').forEach(item => item.classList.toggle('active', item === button)); renderSubmoduleVersions();
}));
refs.submoduleVersionSearch.addEventListener('input', renderSubmoduleVersions);
refs.submoduleOpenGraph.addEventListener('click', () => { if (submoduleMenuEntry) { closeSubmoduleMenu(); openSubmoduleGraph(submoduleMenuEntry); } });
document.addEventListener('click', event => {
  // A New branch/New tag/Push dialog is a child of the version selector in
  // the user's workflow even though <dialog> lives elsewhere in the DOM. Do
  // not mistake clicks in that modal (especially Cancel/X) for an outside
  // click on the parent selector — without this, clicking anything inside
  // the Push preview opened via this menu's own new Push button (including
  // just Cancel) would hide the versions menu underneath before the push
  // dialog itself even closes.
  const childActionOpen = refs.newBranchDialog.open || $('#newTagDialog').open || $('#submodulePublishDialog').open;
  if (!childActionOpen && !refs.submoduleMenu.hidden && !refs.submoduleMenu.contains(event.target) && !event.target.closest('[data-entry]') && !event.target.closest('[data-detail-action="versions"]')) closeSubmoduleMenu();
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || refs.submoduleMenu.hidden) return;
  const childActionOpen = refs.newBranchDialog.open || $('#newTagDialog').open || $('#submodulePublishDialog').open;
  if (childActionOpen) return;
  event.preventDefault();
  closeSubmoduleMenu();
});
refs.commitScope.addEventListener('click', openScopeCommit);
refs.showPathHistory.addEventListener('click', showSelectedHistory);
refs.scopeCommitMessage.addEventListener('input', () => { refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); });
refs.confirmScopeCommit.addEventListener('click', commitSelectedScope);
refs.folderRestoreModeHead.addEventListener('change', () => { refs.folderRestoreCommitPicker.hidden = true; if (state.folderRestore) state.folderRestore.preview = null; renderFolderRestorePreview(null); });
refs.folderRestoreModeCommit.addEventListener('change', () => { refs.folderRestoreCommitPicker.hidden = false; if (state.folderRestore) state.folderRestore.preview = null; renderFolderRestorePreview(null); loadFolderRestoreCommits(); });
refs.folderRestoreClean.addEventListener('change', () => { if (state.folderRestore) { state.folderRestore.preview = null; renderFolderRestorePreview(null); } });
refs.refreshFolderRestoreCommits.addEventListener('click', loadFolderRestoreCommits);
refs.folderRestoreCommitList.addEventListener('click', event => {
  const button = event.target.closest('[data-folder-restore-commit]');
  if (!button || !state.folderRestore) return;
  state.folderRestore.selectedCommit = button.dataset.folderRestoreCommit;
  state.folderRestore.preview = null;
  renderFolderRestoreCommits();
  renderFolderRestorePreview(null);
});
refs.previewFolderRestore.addEventListener('click', () => previewFolderRestore().catch(error => handleError(error)));
refs.confirmFolderRestore.addEventListener('click', () => confirmFolderRestore().catch(error => handleError(error)));
refs.folderRestoreDialog.addEventListener('cancel', () => { state.folderRestore = null; });
refs.folderRestoreDialog.addEventListener('close', () => { if (!refs.folderRestoreDialog.open) state.folderRestore = null; });
$('#initRepo').addEventListener('click', createRepositoryFromPicker);
$('#newBranch').addEventListener('click', async () => {
  if (!state.repository) return;
  // While a submodule's own Branch Map is actually open, this button is
  // unambiguous — there's no "which repository did you mean" the way
  // there is for a merely-*selected* submodule row in Explorer (handled
  // just below): the user is looking straight at the submodule's own
  // sidebar/graph, so the new branch goes there, not the parent.
  if (state.submoduleGraph) { return createSubmoduleBranch({ kind: 'submodule', relative_path: state.submoduleGraph.relativePath, name: state.submoduleGraph.name }); }
  // This button is easy to confuse with a per-submodule action when a
  // submodule happens to be selected — it always creates the branch (and
  // switches) on the MAIN project, never the submodule, so make that
  // unambiguous instead of silently doing the wrong-scope thing.
  if (state.selectedEntry?.kind === 'submodule') {
    const entry = state.selectedEntry;
    const onMain = await customConfirm(`"${entry.name}" (a submodule) is currently selected. This button creates the branch on the MAIN project — the whole project will switch to it. To create a branch inside the submodule instead, cancel this and use "＋ New branch…" in ${entry.name}'s own details panel.`, { title: 'Create branch — choose scope', okLabel: `Create on main project` });
    if (!onMain) return createSubmoduleBranch(entry);
  }
  openNewBranchDialog();
});

// Works for both the main project (submoduleEntry omitted) and a submodule
// (passed in) — same dialog either way, so branching inside a submodule
// gets exactly the same "where am I relative to origin/main" clarity the
// main project's "New branch" already has, instead of a bare text prompt.
async function openNewBranchDialog(submoduleEntry) {
  state.newBranchTarget = submoduleEntry || null;
  refs.newBranchName.value = ''; refs.newBranchStatus.textContent = ''; refs.confirmNewBranch.disabled = true;
  const targetLabel = submoduleEntry ? `${submoduleEntry.name} (submodule)` : (state.repository.current_branch || 'HEAD');
  refs.newBranchFrom.textContent = targetLabel;
  refs.newBranchOriginStatus.textContent = 'Checking origin/main…';
  refs.newBranchDialog.showModal();
  refs.newBranchName.focus();
  if (!invoke) { refs.newBranchOriginStatus.textContent = 'In sync with origin/main.'; return; }
  try {
    const context = await invoke('branch_creation_context', { repositoryPath: state.repository.path, targetPath: submoduleEntry?.relative_path || '' });
    refs.newBranchFrom.textContent = `${submoduleEntry ? `${submoduleEntry.name} — ` : ''}${context.current_branch} @ ${context.current_commit}`;
    if (!context.main_remote_branch) { refs.newBranchOriginStatus.textContent = 'No remote-tracking branch found — fetch first to compare.'; return; }
    if (context.ahead === 0 && context.behind === 0) { refs.newBranchOriginStatus.textContent = `✓ In sync with ${context.main_remote_branch} — the new branch will start from the latest.`; return; }
    const parts = [];
    if (context.ahead) parts.push(`${context.ahead} commit${context.ahead === 1 ? '' : 's'} ahead`);
    if (context.behind) parts.push(`${context.behind} commit${context.behind === 1 ? '' : 's'} behind`);
    refs.newBranchOriginStatus.textContent = `⚠ ${parts.join(', ')} of ${context.main_remote_branch} — the new branch starts from here, not from the latest ${context.main_remote_branch}.`;
  } catch (error) { refs.newBranchOriginStatus.textContent = String(error); }
}

refs.newBranchName.addEventListener('input', () => { refs.confirmNewBranch.disabled = !refs.newBranchName.value.trim(); });
refs.confirmNewBranch.addEventListener('click', async () => {
  const target = state.newBranchTarget;
  if (target) {
    const name = refs.newBranchName.value.trim(); if (!name) return;
    refs.confirmNewBranch.disabled = true; refs.confirmNewBranch.textContent = 'Creating…';
    try {
      await invoke('create_submodule_branch', { repositoryPath: state.repository.path, relativePath: target.relative_path, branch: name });
      refs.newBranchDialog.close();
      directoryCache.clear(); await loadRepository(state.repository.path, { keepPath: true }); await openDirectory(state.currentPath, { force: true });
      if (state.selectedEntry?.relative_path === target.relative_path) await selectEntry(target.relative_path);
      const msg = `${target.name}: branch "${name}" created and checked out.`;
      status(msg); showOperationToast(msg, 'success');
    } catch (error) { refs.newBranchStatus.textContent = String(error); refs.confirmNewBranch.disabled = false; }
    finally { refs.confirmNewBranch.textContent = 'Create branch'; }
    return;
  }
  const name = refs.newBranchName.value.trim(); if (!name) return;
  refs.confirmNewBranch.disabled = true; refs.confirmNewBranch.textContent = 'Creating…';
  try {
    await invoke('create_branch', { path: state.repository.path, branch: name });
    refs.newBranchDialog.close();
    await loadRepository(state.repository.path, { keepPath: true });
    status(`Branch "${name}" created`); showOperationToast(`Branch "${name}" created and checked out.`, 'success');
  } catch (error) { refs.newBranchStatus.textContent = String(error); refs.confirmNewBranch.disabled = false; }
  finally { refs.confirmNewBranch.textContent = 'Create branch'; }
});
refs.commitButton.addEventListener('click', async () => {
  // Without this, the button gave no sign anything was happening — for a
  // commit touching many files (real work happens on the backend: staging,
  // writing the tree, then a full status/history reload) that looked
  // indistinguishable from the app being frozen, and nothing stopped a
  // second click from firing a second commit while the first was still busy.
  // Disabled *before* the flush below too — a second click landing during
  // that flush (waiting on a checkbox toggle from a moment ago) must not be
  // able to fire a second, overlapping commit.
  refs.commitButton.disabled = true;
  refs.commitButton.textContent = (activeStagingOperation || pendingToggles.size) ? 'Waiting for staging…' : 'Committing…';
  status(refs.commitButton.textContent, 'busy');
  // Everything from here on is inside one try/finally — flushPendingTogglesNow
  // used to be called outside any try, so its safety-net timeout (or any
  // other rejection) would leave the button permanently disabled, showing
  // "Waiting for staging…" forever with no error and no way to click Commit
  // again, since the finally that resets it was never reached.
  try {
    // A checkbox ticked in the last 300ms may not have reached the backend
    // yet — committing now would silently leave it out, since the index on
    // disk wouldn't have caught up. state.changes is read *after* this so the
    // files list below reflects what will actually be in the index.
    // { skipReload: true } — this commit does its own reload right after
    // anyway, so a checkbox flush landing right here doesn't need to do one
    // of its own too; without this, ticking a box and immediately hitting
    // Commit did two full reloads back to back instead of one.
    await flushPendingTogglesNow({ skipReload: true }, 'the Stage operation');
    // A Stage all / Unstage all in flight (not the checkbox queue above, a
    // separate mechanism) still needs waiting for — its own reload isn't
    // skippable the same way (unlike a toggle flush, its caller doesn't know
    // in advance that a commit will follow), so this does mean two reloads in
    // that specific interleaving; correctness matters more than avoiding it.
    if (activeStagingOperation) { try { await activeStagingOperation; } catch { /* already reported by whichever button started it */ } }
    const folder = state.changesScope === 'folder' ? state.currentPath : '';
    const files = state.changes.filter(change => change.staged && (!folder || change.path === folder || change.path.startsWith(`${folder}/`))).map(change => change.path);
    refs.commitButton.textContent = `Committing ${files.length} file${files.length === 1 ? '' : 's'}…`;
    status(refs.commitButton.textContent, 'busy');
    // Global scope (no folder filter) means `files` is already exactly
    // everything staged — commit_staged skips rebuilding a scratch index for
    // that case (real work on a large repo/index) since the real on-disk
    // index already *is* the tree this commit needs. A folder-scoped commit
    // is a genuine subset, so it still goes through commit_files.
    if (folder) await invoke('commit_files', { repositoryPath: state.repository.path, files, message: refs.commitMessage.value });
    else await invoke('commit_staged', { repositoryPath: state.repository.path, message: refs.commitMessage.value });
    refs.commitMessage.value = ''; refs.commitButton.textContent = 'Updating status…'; status(refs.commitButton.textContent, 'busy'); await loadRepository(state.repository.path, { reopenPath: folder });
    const successMsg = `Committed ${files.length} file${files.length === 1 ? '' : 's'}${folder ? ` in "${folder}"` : ''}. Push when you're ready to send it to the server.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(`Commit failed: ${message}`, 'error'); }
  finally { refs.commitButton.textContent = 'Commit changes'; renderChanges(); }
});

// Actions and Terminal — two tabs sharing one input:
//  - "App actions" is a context-aware palette over the app's own already-tested
//    actions (no shell access) — suggestions depend on where you are (a
//    submodule selected, a folder open…), each with a short explanation.
//  - "Terminal" is a persistent shell transcript: every command you run (and
//    its actual stdout/stderr) stays visible as a scrolling
//    log, with ↑↓ command history, and an explicit working-directory selector
//    so "runs on the current location" is never a guess.
function currentConsoleContext() {
  const entry = state.selectedEntry;
  if (!state.repository) return { label: 'No repository open', tags: ['no-repo'] };
  if (entry?.kind === 'submodule') return { label: `Submodule: ${entry.relative_path}`, tags: ['submodule', 'has-selection'] };
  if (entry) return { label: `Selected: ${entry.relative_path}`, tags: ['file', 'has-selection'] };
  // Must read through activeGraphData(), never state.repository directly —
  // state.repository always stays the *parent* while a submodule's own
  // graph is open (see activeGraphData's own doc comment), so reading it
  // here directly would show the parent's branch/name under a
  // "Submodule Branch Map" console context, exactly the kind of mix-up this
  // whole area is about not letting happen.
  if (state.view === 'graph') { const g = activeGraphData(); return state.submoduleGraph ? { label: `Submodule Branch Map · ${state.submoduleGraph.name} · ${g.headDetached ? 'detached' : (g.currentBranch || 'detached')}`, tags: ['graph', 'submodule'] } : { label: `Branch Map · ${g.headDetached ? 'detached' : (g.currentBranch || 'detached')}`, tags: ['graph'] }; }
  if (state.view === 'commander') return { label: state.compareMode === 'local-drive' ? 'Compare & Sync · Local Drive' : state.compareMode === 'submodule' ? 'Compare & Sync · Submodule' : 'Compare & Sync · Git', tags: ['commander'] };
  return { label: `${state.currentPath || state.repository.name} · branch ${state.repository.current_branch || 'detached'}`, tags: ['explorer'] };
}

function defaultBranchStartBaseRef() {
  const graph = activeGraphData();
  const branchNames = (graph?.branches || []).map(branch => branch.name);
  if (branchNames.includes('origin/main')) return 'origin/main';
  if (branchNames.includes('origin/master')) return 'origin/master';
  if (branchNames.includes('main')) return 'main';
  if (branchNames.includes('master')) return 'master';
  return 'origin/main';
}

async function findBranchStartCommit() {
  if (!state.repository) return status('Open a repository first.', 'error');
  setConsoleMode('console');
  const target = consoleGitTarget();
  const baseRef = defaultBranchStartBaseRef();
  status(`Finding branch start against ${baseRef}…`, 'busy');
  const mergeBase = await runTerminalFromConsole(`git merge-base HEAD ${baseRef}`);
  const sha = mergeBase?.stdout?.trim().split(/\s+/)[0];
  if (!mergeBase?.success || !/^[0-9a-f]{7,40}$/i.test(sha || '')) {
    status(`Could not find merge-base against ${baseRef}. Check that the branch exists/fetch is up to date.`, 'error');
    return;
  }
  await runTerminalFromConsole(`git show --no-patch --decorate --date=short --stat ${sha}`);
  state.branchStartMarker = { repositoryPath: target.path, id: sha, baseRef };
  if (state.submoduleGraph || activeGraphData().path !== target.path) closeSubmoduleGraph();
  state.view = 'graph';
  refs.search.value = sha.slice(0, 8);
  render();
  const row = refs.graph.querySelector(`.commit-row[data-id="${CSS.escape(sha)}"]`);
  if (row) { row.scrollIntoView({ block: 'center' }); selectCommit(sha); }
  status(`Branch start found: ${sha.slice(0, 8)} against ${baseRef}. Commands are shown in Terminal.`);
}

async function initAndUpdateSubmodulesFromActions() {
  if (!state.repository) return status('Open a repository first.', 'error');
  status('Initializing and updating submodules recursively…', 'busy');
  const result = await runTerminalFromConsole('git submodule update --init --recursive');
  if (result?.success) status('Submodules initialized/updated. Repository was refreshed.');
  else status('Submodule init/update failed. See Terminal output.', 'error');
  return result;
}

function buildCommands() {
  const entry = state.selectedEntry;
  const list = [
    { id: 'open-repo', name: 'Open Repository', description: 'Choose a local Git folder to open', keys: 'Ctrl+O', tags: ['no-repo'], fn: openRepository },
    { id: 'new-branch', name: 'New Branch', description: 'Create a new local branch from the current one', keys: 'Ctrl+Shift+B', tags: ['explorer', 'graph'], fn: () => $('#newBranch').click() },
    { id: 'commit', name: 'Commit Changes', description: 'Open the Working tree drawer and write a commit message for staged files', keys: 'Ctrl+Shift+C', keywords: 'save record', tags: ['explorer'], fn: () => { state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); refreshChangesLightweight(); refs.commitMessage.focus(); } },
    { id: 'push', name: 'Push Current Branch', description: 'Send your local commits on this branch to the server', keys: 'Ctrl+Shift+P', tags: ['explorer', 'graph'], fn: () => $('#pushCurrent').click() },
    { id: 'pull', name: 'Pull Current Branch', description: 'Fetch and fast-forward the current branch from the server', keys: '', tags: ['explorer', 'graph'], fn: () => $('#pullCurrent').click() },
    { id: 'merge', name: 'Merge Branch…', description: 'Bring another branch\'s commits into your current one — stays local, resolves conflicts here if any', keys: '', keywords: 'combine join', tags: ['explorer', 'graph'], fn: () => state.repository && openMergeBranchDialog(mergeTargetForMain()) },
    { id: 'fetch', name: 'Fetch Remote', description: 'Download new commits/refs from the server without changing your branch', keys: 'Ctrl+Shift+F', tags: ['explorer', 'graph'], fn: () => $('#fetchCurrent').click() },
    { id: 'fetchall', name: 'Fetch All Remotes', description: 'Download new commits/refs from every configured remote, not just the first one', keywords: 'multiple upstream mirror', tags: ['explorer', 'graph'], fn: () => fetchAllRemotes() },
    { id: 'fetch-project', name: 'Fetch Project + Submodules', description: 'Safe update: fetch parent remotes and initialized submodule origins without pull, checkout or branch changes', keywords: 'submodule update all refresh server safe', tags: ['explorer', 'graph'], fn: () => fetchProjectAndSubmodules() },
    { id: 'init-update-submodules', name: 'Init / Update Submodules', description: 'Run git submodule update --init --recursive, then refresh the project. Use when submodule folders are empty or Git metadata is missing.', keywords: 'submodule init initialize recursive update empty missing metadata', tags: ['explorer', 'graph', 'relevant'], keepOpen: true, fn: initAndUpdateSubmodulesFromActions },
    { id: 'branch-start', name: 'Find Branch Start Commit', description: 'Run merge-base against origin/main, show the commit details, and mark that split point on the Branch Map', keys: '', keywords: 'merge-base parent start base fork origin/main', tags: ['explorer', 'graph', 'relevant'], keepOpen: true, fn: findBranchStartCommit },
    { id: 'stash', name: 'Stash Changes in Current Repository', description: 'Set aside changes only in the project or submodule currently being browsed', keys: 'Ctrl+Shift+S', tags: ['explorer'], fn: stashWork },
    { id: 'pop', name: 'View Stashes in Current Repository', description: 'View or restore saved changes for this project or submodule', keys: '', tags: ['explorer'], fn: popStash },
    { id: 'conflicts', name: 'Resolve Merge Conflicts', description: 'Open the conflict resolution dialog for a merge in progress', keys: '', keywords: 'merge conflict resolve', tags: state.pendingMainConflicts?.length ? ['explorer', 'graph', 'relevant'] : [], fn: () => openConflictsDialog(mergeTargetForMain(), state.pendingMainConflicts || []) },
    { id: 'search', name: 'Search Repository', description: 'Filter the current view by name, author or commit id', keys: 'Ctrl+F', tags: ['explorer', 'graph', 'commander'], fn: () => refs.search.focus() },
    { id: 'explorer', name: 'Go to Project Explorer', description: 'Browse files, folders and submodules', keys: '', keywords: 'files browse', tags: [], fn: () => $('#navExplorer').click() },
    { id: 'commander', name: 'Go to Compare & Sync', description: 'Compare Git snapshots, submodule revisions, or sync/copy between two local folders', keys: 'Ctrl+Shift+L', keywords: 'diff compare folder sync local drive remote submodule', tags: [], fn: () => $('#navCommander').click() },
    { id: 'graph', name: 'Go to Branch Map', description: 'See commit history and branches as a graph', keys: 'Ctrl+Shift+G', keywords: 'log history commits', tags: [], fn: () => $('#navGraph').click() },
    { id: 'remotes', name: 'Go to Remotes', description: 'View and fetch configured server locations', keys: '', tags: [], fn: () => $('#navRemotes').click() },
    { id: 'refresh', name: 'Refresh Repository', description: 'Re-read branches, commits and status from disk (e.g. after external Git commands)', keys: '', keywords: 'reload', tags: [], fn: () => $('#refresh').click() },
  ];
  if (entry?.kind === 'submodule') {
    if (entry.submodule_initialized === false) {
      list.push({ id: 'sub-init', name: `Initialize Submodule (${entry.name})`, description: 'Clone/check out this submodule at the commit recorded by the parent project', tags: ['submodule', 'relevant'], keywords: 'empty missing init update checkout', fn: () => initializeSubmodule(entry) });
    }
    list.push(
      { id: 'sub-pull', name: `Pull Submodule (${entry.name})`, description: 'Fast-forward this submodule from its own remote', tags: ['submodule', 'relevant'], keywords: 'submodule update', fn: () => pullSubmodule(entry) },
      { id: 'sub-push', name: `Push Submodule (${entry.name})`, description: 'Send this submodule\'s local commits to its own remote', tags: ['submodule', 'relevant'], fn: () => pushSubmodule(entry) },
      { id: 'sub-merge', name: `Merge Branch into Submodule (${entry.name})`, description: 'Bring another branch into this submodule\'s current branch', tags: ['submodule', 'relevant'], keywords: 'merge combine', fn: () => openMergeBranchDialog(mergeTargetForSubmodule(entry)) },
      { id: 'sub-commit', name: `Commit Submodule (${entry.name})`, description: 'Commit uncommitted changes inside this submodule', tags: ['submodule', 'relevant'], fn: () => commitSubmoduleChanges(entry) },
      { id: 'sub-version', name: `Change Submodule Version (${entry.name})`, description: 'Switch this submodule to a different branch, tag or commit', tags: ['submodule', 'relevant'], keywords: 'checkout switch branch', fn: () => openSubmoduleMenu(entry, innerWidth - 480, 110) },
      { id: 'sub-compare', name: `Compare Submodule Revisions (${entry.name})`, description: 'Compare two exact branches, tags or commits of this submodule side by side', tags: ['submodule', 'relevant'], keywords: 'diff compare revision tag branch', fn: () => openSubmoduleCompareFromEntry(entry) },
      { id: 'sub-fetch', name: `Fetch Submodule (${entry.name})`, description: 'Download new commits for this submodule without changing its checkout', tags: ['submodule'], fn: () => fetchSubmodule(entry) },
      { id: 'sub-new-branch', name: `New Branch in Submodule (${entry.name})`, description: 'Create and switch to a new branch in this submodule, from its current commit', tags: ['submodule', 'relevant'], keywords: 'checkout create', fn: () => createSubmoduleBranch(entry) },
    );
  } else if (entry?.kind === 'file') {
    list.push(
      { id: 'file-edit', name: `Edit ${entry.name}`, description: 'Open this file in the built-in editor', tags: ['file', 'relevant'], fn: () => openEditor(entry) },
      { id: 'file-compare', name: `Compare ${entry.name} with Remote`, description: 'Side-by-side diff against the server version, with restore options', tags: ['file', 'relevant'], keywords: 'diff', fn: () => compareEntryWithRemote(entry) },
    );
  }
  state.savedActions.forEach((action, index) => {
    const commands = savedActionCommands(action);
    list.push({
      id: `saved-action-${index}`,
      name: `Saved action: ${action.name}`,
      description: commands.length === 1 ? commands[0] : `${commands.length} commands · ${commands.join(' → ')}`,
      keywords: `custom saved preset macro ${commands.join(' ')}`,
      tags: ['explorer', 'graph', 'commander'],
      keepOpen: true,
      fn: () => runSavedAction(action),
    });
  });
  return list;
}

let commandPaletteOpen = false;
let activeCommands = [];

function scoreCommand(cmd, query, tags) {
  const relevant = cmd.tags?.some(tag => tags.includes(tag)) ? 1 : 0;
  if (!query) return relevant;
  const haystack = `${cmd.name} ${cmd.description || ''} ${cmd.keywords || ''}`.toLowerCase();
  if (!haystack.includes(query)) return -1;
  const nameMatch = cmd.name.toLowerCase().startsWith(query) ? 2 : cmd.name.toLowerCase().includes(query) ? 1 : 0;
  return relevant * 10 + nameMatch;
}

// A word someone very plausibly typed expecting real git output (habit from a
// terminal) rather than an app-action search — offered as a one-click "run
// this as raw git instead" suggestion so forgetting the "$" prefix doesn't
// silently fall back to fuzzy-matching app commands by unrelated description
// text (e.g. "status" matching "Refresh Repository" because its description
// happens to mention "status").
// A small, plain-language git reference — not exhaustive, but covers what
// someone unfamiliar with git actually reaches for. Powers both the "run as
// git command" suggestion and the live "what comes next" flag hints while
// typing a Git command in the Terminal.
const GIT_COMMAND_HELP = {
  status: { description: 'Shows what changed in your working folder — modified, staged, untracked files.', flags: [
    { flag: '-s', desc: 'Short format — one compact line per file' },
    { flag: '-b', desc: 'Also show the current branch and how far ahead/behind it is' },
    { flag: '--ignored', desc: 'Also list files excluded by .gitignore' },
  ] },
  log: { description: 'Shows commit history, newest first.', flags: [
    { flag: '--oneline', desc: 'One short line per commit instead of the full message' },
    { flag: '-10', desc: 'Only the last 10 commits' },
    { flag: '--graph', desc: 'Draw the branch/merge lines in text form' },
    { flag: '--author=', desc: 'Only commits by a specific author' },
    { flag: '-- <path>', desc: 'Only commits that touched this file/folder' },
  ] },
  diff: { description: 'Shows the exact line-by-line changes that aren\'t committed yet.', flags: [
    { flag: '--staged', desc: 'Show only what\'s already staged (about to be committed)' },
    { flag: 'HEAD~1', desc: 'Compare against the previous commit instead of the working copy' },
    { flag: '-- <path>', desc: 'Limit the diff to one file/folder' },
  ] },
  add: { description: 'Stages a file — marks it to be included in the next commit.', flags: [
    { flag: '.', desc: 'Stage everything changed in the current folder and below' },
    { flag: '-A', desc: 'Stage everything in the whole repository, including deletions' },
    { flag: '-p', desc: 'Choose which parts (hunks) of a file to stage, interactively' },
  ] },
  commit: { description: 'Records the currently staged changes as a new commit.', flags: [
    { flag: '-m ""', desc: 'Provide the commit message inline instead of opening an editor' },
    { flag: '-am ""', desc: 'Stage every already-tracked modified file AND commit, in one step' },
    { flag: '--amend', desc: 'Edit the message/contents of the last commit instead of making a new one' },
  ] },
  branch: { description: 'Lists, creates, or deletes branches.', flags: [
    { flag: '-a', desc: 'List local AND remote-tracking branches' },
    { flag: '-d <name>', desc: 'Delete a branch that\'s already merged (safe)' },
    { flag: '-D <name>', desc: '⚠ Force-delete a branch even if unmerged (can lose commits)' },
    { flag: '-m <new>', desc: 'Rename the current branch' },
  ] },
  checkout: { description: 'Switches branches, or restores files to a previous state.', flags: [
    { flag: '-b <name>', desc: 'Create a new branch and switch to it in one step' },
    { flag: '-- <path>', desc: '⚠ Discard local edits to this file, restoring it from the last commit' },
  ] },
  switch: { description: 'Switches to a different branch (the modern, safer alternative to checkout).', flags: [
    { flag: '-c <name>', desc: 'Create a new branch and switch to it' },
  ] },
  merge: { description: 'Brings another branch\'s commits into the one you\'re on.', flags: [
    { flag: '--no-ff', desc: 'Always create a merge commit, even if a fast-forward is possible' },
    { flag: '--abort', desc: 'Cancel a merge that has conflicts, back to the pre-merge state' },
  ] },
  rebase: { description: '⚠ Replays your commits on top of another branch, rewriting history. Avoid on commits you\'ve already pushed/shared.', flags: [
    { flag: '--abort', desc: 'Cancel an in-progress rebase, back to how it was before' },
    { flag: '--continue', desc: 'Continue after resolving a conflict' },
  ] },
  reset: { description: 'Moves the current branch pointer — how destructive depends on the flag.', flags: [
    { flag: 'HEAD -- <path>', desc: 'Unstage a file, keep its edits on disk (safe)' },
    { flag: '--soft HEAD~1', desc: 'Undo the last commit, keep everything staged' },
    { flag: '--hard HEAD', desc: '⚠ Discard ALL local edits and staged changes — cannot be undone' },
  ] },
  clean: { description: '⚠ Deletes untracked files from disk — not recoverable from git afterward.', flags: [
    { flag: '-n', desc: 'Dry run — show what WOULD be deleted, without deleting anything' },
    { flag: '-fd', desc: '⚠ Actually delete untracked files AND untracked folders' },
  ] },
  remote: { description: 'Lists or manages configured remotes (like "origin").', flags: [
    { flag: '-v', desc: 'Show the fetch/push URLs for each remote' },
    { flag: 'add <name> <url>', desc: 'Add a new remote' },
  ] },
  fetch: { description: 'Downloads new commits/branches from a remote, without changing your files.', flags: [
    { flag: '--all', desc: 'Fetch from every configured remote' },
    { flag: '--prune', desc: 'Also remove local references to branches deleted on the remote' },
  ] },
  pull: { description: 'Fetches from a remote AND merges/fast-forwards into your current branch.', flags: [
    { flag: '--ff-only', desc: 'Only proceed if it can fast-forward — refuse if it would need a real merge' },
    { flag: '--rebase', desc: 'Replay your local commits on top instead of merging' },
  ] },
  push: { description: 'Uploads your local commits to a remote branch.', flags: [
    { flag: '-u origin <branch>', desc: 'Push and remember this as the branch\'s default upstream' },
    { flag: '--force-with-lease', desc: '⚠ Overwrite the remote branch, but refuse if someone else pushed since your last fetch' },
    { flag: '--force', desc: '⚠⚠ Overwrite the remote branch unconditionally — can destroy others\' work' },
  ] },
  show: { description: 'Shows the full details (message + diff) of one commit.', flags: [
    { flag: 'HEAD', desc: 'Show the most recent commit' },
    { flag: '--stat', desc: 'Show only which files changed and by how much, not the full diff' },
  ] },
  blame: { description: 'Shows who last changed each line of a file, and in which commit.', flags: [
    { flag: '-- <path>', desc: 'The file to blame (required)' },
  ] },
  tag: { description: 'Lists or creates tags (named pointers to a specific commit).', flags: [
    { flag: '-a <name> -m ""', desc: 'Create an annotated tag with a message' },
    { flag: '-d <name>', desc: 'Delete a local tag' },
  ] },
  stash: { description: 'Temporarily sets aside uncommitted changes so your working folder is clean.', flags: [
    { flag: 'list', desc: 'Show all saved stashes' },
    { flag: 'pop', desc: 'Restore the most recent stash and remove it from the list' },
    { flag: 'drop', desc: 'Delete the most recent stash without restoring it' },
  ] },
  reflog: { description: 'Shows a log of everywhere HEAD has pointed — a safety net to recover "lost" commits.', flags: [] },
  submodule: { description: 'Manages submodules (nested repositories inside this one).', flags: [
    { flag: 'status', desc: 'Show the commit each submodule is on, and whether it\'s changed' },
    { flag: 'update --init --recursive', desc: 'Fetch and check out every submodule at the commit the parent expects' },
    { flag: 'foreach "<cmd>"', desc: 'Run a shell command inside every submodule' },
  ] },
  'ls-files': { description: 'Lists files git is tracking.', flags: [] },
};
const GIT_SUBCOMMAND_HINTS = new Set(Object.keys(GIT_COMMAND_HELP));

function renderCommandList(query) {
  const ctx = currentConsoleContext();
  const q = query.trim().toLowerCase();
  const scored = activeCommands.map(cmd => ({ cmd, score: scoreCommand(cmd, q, ctx.tags) })).filter(entry => entry.score >= 0);
  scored.sort((a, b) => b.score - a.score);
  const list = $('#commandList');
  // Recognize both "add ." and "git add ." — either way of typing it should
  // surface the "run as raw git" suggestion.
  const words = q.split(/\s+/).filter(Boolean);
  const gitWords = words[0] === 'git' ? words.slice(1) : words;
  const gitArgsText = words[0] === 'git' ? query.trim().replace(/^git\s+/i, '') : query.trim();
  const gitHint = gitWords[0] && GIT_SUBCOMMAND_HINTS.has(gitWords[0]) ? `<div class="command-item selected raw-git-item" data-raw-git-suggest="${esc(gitArgsText)}">
    <div class="command-item-head"><span class="command-name">▸ Run as git command: git ${esc(gitArgsText)}</span><span class="command-keys">Enter</span></div>
    <span class="command-desc">Looks like a git subcommand — run it directly and see real output</span>
  </div>` : '';
  list.innerHTML = gitHint + (scored.map(({ cmd, score }, i) => `<div class="command-item ${i === 0 && !gitHint ? 'selected' : ''} ${score >= 10 ? 'relevant' : ''}" data-cmd-id="${esc(cmd.id)}">
    <div class="command-item-head"><span class="command-name">${esc(cmd.name)}</span>${cmd.keys ? `<span class="command-keys">${esc(cmd.keys)}</span>` : ''}</div>
    <span class="command-desc">${esc(cmd.description || '')}</span>
  </div>`).join('') || (gitHint ? '' : '<div class="command-item" style="text-align:center;color:#6b7f96;">No matching commands</div>'));
}

function savedActionCommands(action) {
  return normalizeSavedAction(action)?.commands || [];
}
function splitSavedActionCommandText(text) {
  return text.split(/\n|&&/).map(command => command.trim()).filter(Boolean);
}
async function runSavedAction(action) {
  const normalized = normalizeSavedAction(action);
  if (!normalized) return status('Saved action has no commands to run.', 'error');
  setConsoleMode('console');
  status(`Running saved action: ${normalized.name}`, 'busy');
  for (const command of normalized.commands) {
    const result = await runTerminalFromConsole(command);
    if (!result?.success) {
      status(`Saved action stopped at failed command: ${command}`, 'error');
      return result;
    }
  }
  status(`Saved action completed: ${normalized.name}`);
  return { success: true };
}

function renderSavedActions(query = '') {
  const list = $('#commandList'); list.classList.remove('console-transcript');
  const q = query.trim().toLowerCase();
  const rows = state.savedActions
    .map((item, index) => ({ item, index }))
    .filter(({ item }) => !q || `${item.name} ${savedActionCommands(item).join(' ')}`.toLowerCase().includes(q));
  list.innerHTML = rows.map(({ item, index }, rowIndex) => `<div class="command-item ${rowIndex === 0 ? 'selected' : ''}" data-saved-index="${index}">
    <div class="command-item-head"><span class="command-name">${esc(item.name)}</span><span class="saved-command-actions"><button type="button" data-saved-run="${index}">Run</button><button type="button" data-saved-edit="${index}">Edit</button><button type="button" class="danger" data-saved-delete="${index}">Delete</button></span></div>
    <span class="command-desc">${esc(savedActionCommands(item).length === 1 ? savedActionCommands(item)[0] : `${savedActionCommands(item).length} commands · ${savedActionCommands(item).join('  →  ')}`)}</span>
  </div>`).join('') || '<div class="command-item" style="text-align:center;color:#6b7f96;">No saved actions yet. Press ＋ Save to add a named command group.</div>';
}

async function addOrEditSavedAction(index = -1) {
  const existing = index >= 0 ? state.savedActions[index] : null;
  const existingCommands = savedActionCommands(existing);
  const name = await customPrompt('Name for this saved action:', existing?.name || '', { title: existing ? 'Edit saved action' : 'Save action', okLabel: 'Next' });
  if (name == null) return;
  const trimmedName = name.trim();
  if (!trimmedName) return status('Saved action needs a name.', 'error');
  const defaultCommands = existingCommands.length ? existingCommands.join('\n') : $('#commandInput').value.trim();
  const commandText = await customPrompt('Commands to run, one per line. You can also separate simple commands with &&. They run in order and stop on first failure:', defaultCommands, { title: existing ? 'Edit saved action commands' : 'Save action commands', okLabel: existing ? 'Save' : 'Add', multiline: true });
  if (commandText == null) return;
  const commands = splitSavedActionCommandText(commandText);
  if (!commands.length) return status('Saved action needs at least one command.', 'error');
  const item = { name: trimmedName, commands };
  if (existing) state.savedActions.splice(index, 1, item); else state.savedActions.push(item);
  saveSavedActions();
  setConsoleMode('saved');
  activeCommands = buildCommands();
  renderSavedActions($('#commandInput').value);
  status(existing ? 'Saved action updated.' : 'Saved action added.');
}

async function deleteSavedAction(index) {
  const item = state.savedActions[index];
  if (!item) return;
  const ok = await customConfirm(`Delete saved action "${item.name}"?`, { title: 'Delete saved action', danger: true, okLabel: 'Delete' });
  if (!ok) return;
  state.savedActions.splice(index, 1);
  saveSavedActions();
  activeCommands = buildCommands();
  renderSavedActions($('#commandInput').value);
  status('Saved action deleted.');
}

// ---- Terminal — persistent shell transcript -------------------------------
// Deliberately separate from the app's own tested actions above: this is the
// explicit escape hatch for real shell commands. A known bare Git subcommand
// ("status") is normalized to "git status" for compatibility with the older
// Git Console, while an already-complete or non-Git command is left untouched.
const RAW_GIT_PREFIX = '$';
function isRawGitQuery(query) { return query.trimStart().startsWith(RAW_GIT_PREFIX); }
function rawGitArgs(query) { return query.trimStart().slice(1).trim(); }
const DESTRUCTIVE_TERMINAL_PATTERN = /(^|[\s;&|])(?:git\s+)?(?:reset\s+--hard|clean\s+-[a-z]*f|push\s+.*(?:--force|-f\b)|branch\s+-D|checkout\s+.*-f\b|rebase|filter-branch|gc\s+--prune|update-ref\s+-d)|(^|[\s;&|])(?:rm\s+-[a-z]*r[a-z]*f|rm\s+-[a-z]*f[a-z]*r|del(?:ete)?\s|rmdir\s|remove-item\s|format\s|diskpart\b)/i;
function normalizeTerminalCommand(input) {
  const text = input.trim();
  const first = text.split(/\s+/, 1)[0]?.toLowerCase();
  return first && first !== 'git' && KNOWN_GIT_SUBCOMMANDS.includes(first) ? `git ${text}` : text;
}
function looksDestructiveTerminalCommand(command) {
  // Redirection writes to disk even when the program itself sounds read-only.
  return DESTRUCTIVE_TERMINAL_PATTERN.test(command) || />/.test(command);
}

// Build a context-bound model of every valid working directory. The pure model
// stores concrete paths (not reusable "folder"/"submodule" tokens), so an
// explicit choice cannot leak after navigation or into another repository.
function consoleAvailableScopes() {
  return ConsoleContextModel.buildConsoleScopeModel({
    repository: state.repository,
    view: state.view,
    currentPath: state.currentPath,
    commanderPath: state.commanderPath,
    selectedSubmodule: state.selectedEntry?.kind === 'submodule'
      ? { relativePath: state.selectedEntry.relative_path, name: state.selectedEntry.name }
      : null,
    submoduleGraph: state.submoduleGraph
      ? { path: state.submoduleGraph.repository?.path, relativePath: state.submoduleGraph.relativePath, name: state.submoduleGraph.name }
      : null,
  });
}
function consoleGitTarget() {
  return ConsoleContextModel.resolveConsoleScope(consoleAvailableScopes(), state.consoleScopeOverride);
}
function selectConsoleScope(targetKey) {
  const model = consoleAvailableScopes();
  state.consoleScopeOverride = ConsoleContextModel.consoleScopeOverrideFor(model, targetKey);
  updateConsoleScopeLabel();
}
function updateConsoleScopeLabel() {
  const model = consoleAvailableScopes();
  const scope = consoleGitTarget();
  $('#commandScope').innerHTML = state.repository ? `<strong>${esc(scope.label)}</strong><small>${esc(scope.displayPath || scope.path)}</small>` : '<strong>No repository open</strong>';
  $('#commandScope').title = state.repository ? scope.path : '';
  const selector = $('#commandScopeSelect');
  selector.innerHTML = model.scopes.map(item => `<option value="${esc(item.key)}">${esc(item.label)} — ${esc(item.displayPath)}</option>`).join('') || '<option value="">No repository open</option>';
  selector.value = scope.key;
  selector.disabled = model.scopes.length < 2;
  selector.title = model.scopes.length > 1 ? 'Choose the exact working directory' : 'No other working directory is available in this context';
}

// Lightly colorizes familiar Git output while leaving arbitrary command
// output untouched apart from HTML escaping.
function colorizeGitOutput(text) {
  return text.split('\n').map(line => {
    const escaped = esc(line);
    if (/^\+\+\+ /.test(line) || /^--- /.test(line)) return `<span class="git-out-meta">${escaped}</span>`;
    if (/^\+/.test(line)) return `<span class="git-out-add">${escaped}</span>`;
    if (/^-/.test(line)) return `<span class="git-out-del">${escaped}</span>`;
    if (/^@@.*@@/.test(line)) return `<span class="git-out-hunk">${escaped}</span>`;
    if (/^(diff --git|index [0-9a-f])/.test(line)) return `<span class="git-out-meta">${escaped}</span>`;
    if (/^\s*(modified|new file|deleted|renamed|copied):/.test(line)) return `<span class="git-out-changed">${escaped}</span>`;
    if (/^\s*\(use "git/.test(line)) return `<span class="git-out-hint">${escaped}</span>`;
    if (/^(On branch |Your branch)/.test(line)) return `<span class="git-out-branch">${escaped}</span>`;
    if (/^(Untracked files:|Changes (to be committed|not staged for commit):)/.test(line)) return `<span class="git-out-section">${escaped}</span>`;
    if (/^[0-9a-f]{7,40}\b/.test(line)) return `<span class="git-out-hash">${escaped}</span>`;
    return escaped;
  }).join('\n');
}

function renderConsoleTranscript() {
  const list = $('#commandList'); list.classList.add('console-transcript');
  list.innerHTML = state.consoleTranscript.map((entry, index) => {
    const elapsed = entry.status === 'RUNNING' ? ((performance.now() - entry.startedAt) / 1000).toFixed(1) : (entry.elapsedMs / 1000).toFixed(1);
    const badge = { RUNNING: '<i class="spinner"></i> RUNNING', SUCCESS: 'SUCCESS', FAILED: 'FAILED', TIMED_OUT: 'TIMED OUT' }[entry.status];
    const badgeClass = { RUNNING: 'running', SUCCESS: 'ok', FAILED: 'fail', TIMED_OUT: 'fail' }[entry.status];
    const result = entry.result;
    const exitCodeLabel = result && result.exit_code != null ? ` · exit ${result.exit_code}` : '';
    return `<div class="raw-git-output">
    <div class="raw-git-cmd"><span class="raw-git-command-text">$ ${esc(entry.command)} <span class="raw-git-cwd" title="${esc(entry.targetPath || '')}">(in ${esc(entry.targetLabel)}${entry.targetDisplayPath ? ` · ${esc(entry.targetDisplayPath)}` : ''})</span></span> <b class="raw-git-status ${badgeClass}">${badge}</b><span class="raw-git-elapsed">${elapsed}s${exitCodeLabel}</span><span class="raw-git-actions"><button type="button" class="raw-git-action" data-console-reuse="${index}" title="Put this command back in the input so it can be edited">Use again</button><button type="button" class="raw-git-action" data-console-copy="${index}" title="Copy this command and its output">Copy</button></span></div>
    ${result?.stdout ? `<pre class="raw-git-stdout">${colorizeGitOutput(result.stdout)}</pre>` : ''}
    ${result?.stderr ? `<pre class="raw-git-stderr">${colorizeGitOutput(result.stderr)}</pre>` : ''}
    ${entry.status === 'SUCCESS' && !result?.stdout && !result?.stderr ? '<div class="raw-git-empty">Completed successfully — no output</div>' : ''}
  </div>`;
  }).join('') || '<div class="console-empty">Run a command in the working directory shown above — e.g. "git status", "git remote -v", "pwd", "ls" (macOS) or "dir" (Windows).</div>';
  $('#commandClearTranscript').hidden = state.consoleTranscript.length === 0;
  $('#commandCopyTranscript').hidden = state.consoleTranscript.length === 0;
  list.scrollTop = list.scrollHeight;
}

function consoleEntryAsText(entry) {
  const result = entry.result;
  const lines = [`$ ${entry.command}`, `# working directory: ${entry.targetPath || entry.targetLabel}`, `# ${entry.status}${result?.exit_code != null ? ` (exit ${result.exit_code})` : ''}`];
  if (result?.stdout) lines.push(result.stdout.trimEnd());
  if (result?.stderr) lines.push(result.stderr.trimEnd());
  return lines.join('\n');
}

async function copyText(text, successMessage) {
  try {
    if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
    else {
      const textarea = document.createElement('textarea'); textarea.value = text; textarea.style.position = 'fixed'; textarea.style.opacity = '0';
      document.body.appendChild(textarea); textarea.select(); document.execCommand('copy'); textarea.remove();
    }
    status(successMessage);
  } catch (error) { status(`Could not copy: ${String(error)}`, 'error'); }
}

function setCommandInputValue(value) {
  const input = $('#commandInput'); input.value = value;
  state.consoleDrafts[state.consoleMode] = value;
  input.setSelectionRange(value.length, value.length);
}

let consoleHistoryPointer = -1;
let consoleHistoryDraft = '';
function recallConsoleHistory(direction) {
  const history = state.consoleCmdHistory; if (!history.length) return;
  const input = $('#commandInput');
  if (consoleHistoryPointer === -1) {
    if (direction > 0) return;
    consoleHistoryDraft = input.value;
    consoleHistoryPointer = history.length;
  }
  const next = Math.max(0, Math.min(history.length, consoleHistoryPointer + direction));
  if (next === history.length) { consoleHistoryPointer = -1; setCommandInputValue(consoleHistoryDraft); }
  else { consoleHistoryPointer = next; setCommandInputValue(history[next]); }
}

let consoleRunningTicker = null;

async function runTerminalFromConsole(input) {
  if (!input) return;
  setConsoleMode('console');
  const command = normalizeTerminalCommand(input);
  setCommandInputValue('');
  if (/^(clear|cls)$/i.test(command)) { state.consoleTranscript = []; renderConsoleTranscript(); return { success: true, stdout: '', stderr: '', exit_code: 0, read_only: true }; }
  if (!state.repository) { const result = { success: false, stdout: '', stderr: 'Open a repository first.' }; state.consoleTranscript.push({ command, targetLabel: '—', targetPath: '', targetDisplayPath: '', status: 'FAILED', elapsedMs: 0, result }); renderConsoleTranscript(); return result; }
  // Repeated Enter while one is already running is a no-op, not a queued-up
  // second command — only one Git command is ever active at a time, and the
  // centralized invoke wrapper enforces this the same way for every other
  // mutation too, not just another console command.
  if (state.consoleCommandRunning) { status('A Terminal command is already running — wait for it to finish.', 'error'); return { success: false, stdout: '', stderr: 'A Terminal command is already running.' }; }
  if (looksDestructiveTerminalCommand(command)) {
    const target = consoleGitTarget();
    const ok = await customConfirm(`This command may overwrite or delete data: "${command}" in ${target.label}. Continue?`, { title: 'Potentially destructive command', danger: true, okLabel: 'Run it anyway' });
    if (!ok) return { success: false, stdout: '', stderr: 'Cancelled.' };
  }
  if (state.consoleCmdHistory[state.consoleCmdHistory.length - 1] !== command) state.consoleCmdHistory.push(command);
  if (state.consoleCmdHistory.length > 100) state.consoleCmdHistory.splice(0, state.consoleCmdHistory.length - 100);
  consoleHistoryPointer = -1; consoleHistoryDraft = '';
  // Captured now, before anything async — a delayed result must act on the
  // repository/scope this command actually ran against, never on whatever
  // happens to be open by the time it resolves (though the invoke wrapper
  // already refuses to switch repositories while consoleCommandRunning is
  // true, so this is belt-and-suspenders, not the only thing preventing it).
  const target = consoleGitTarget();
  const capturedRepositoryPath = state.repository.path;
  if (!invoke) { const result = { success: true, stdout: '(preview mode — not actually run)', stderr: '' }; state.consoleTranscript.push({ command, targetLabel: target.label, targetPath: target.path, targetDisplayPath: target.displayPath, status: 'SUCCESS', elapsedMs: 0, result }); renderConsoleTranscript(); return result; }

  const entry = { command, targetLabel: target.label, targetPath: target.path, targetDisplayPath: target.displayPath, status: 'RUNNING', startedAt: performance.now(), elapsedMs: 0, result: null };
  state.consoleTranscript.push(entry);
  renderConsoleTranscript();
  state.consoleCommandRunning = true;
  jsPerfLog(`frontend: runTerminal START (${target.label})`, 0);
  consoleRunningTicker = setInterval(renderConsoleTranscript, 200);
  try {
    const result = await invoke('run_terminal_command', { repositoryPath: target.path, commandText: command });
    entry.status = result.success ? 'SUCCESS' : 'FAILED';
    entry.result = result;
    entry.elapsedMs = performance.now() - entry.startedAt;
    jsPerfLog(`frontend: runTerminal ${entry.status} (read_only=${!!result.read_only})`, entry.elapsedMs);
    clearInterval(consoleRunningTicker); consoleRunningTicker = null;
    state.consoleCommandRunning = false;
    renderConsoleTranscript();
    refreshCommandHint();
    // The strict read-only allowlist (status/log/diff/show/blame/ls-files,
    // matched on the subcommand alone) is the only case that skips a full
    // reload — anything else, including a command this app has never heard
    // of, is treated conservatively as possibly mutating.
    if (!result.read_only && state.repository?.path === capturedRepositoryPath) {
      // Force bypasses status caches because an arbitrary shell command can
      // change files or Git metadata without going through any app command's
      // normal invalidation path (especially when the selected scope is a
      // submodule but the parent repository must also notice its new state).
      directoryCache.clear(); await loadRepository(capturedRepositoryPath, { keepPath: true, force: true });
    }
    return result;
  } catch (error) {
    const message = String(error);
    entry.status = message.toLowerCase().includes('timed out') ? 'TIMED_OUT' : 'FAILED';
    entry.result = { success: false, stdout: '', stderr: message, exit_code: null };
    entry.elapsedMs = performance.now() - entry.startedAt;
    jsPerfLog(`frontend: runTerminal ${entry.status}`, entry.elapsedMs);
    clearInterval(consoleRunningTicker); consoleRunningTicker = null;
    state.consoleCommandRunning = false;
    renderConsoleTranscript();
    refreshCommandHint();
    return entry.result;
  }
}

// Live "what comes next" helper while typing a Git command in the Terminal — shows the
// recognized subcommand's plain-language description plus its common flags
// (click to append), or, while still typing the subcommand itself, matching
// subcommand names to autocomplete. Aimed squarely at someone who doesn't
// already have git's flags memorized.
// Extra subcommand names worth typo-correcting even though they don't have
// their own flag reference above.
const KNOWN_GIT_SUBCOMMANDS = [...Object.keys(GIT_COMMAND_HELP), 'init', 'clone', 'config', 'describe', 'worktree', 'revert', 'bisect', 'cherry-pick', 'archive', 'rm', 'mv', 'gc', 'apply'];

function levenshtein(a, b) {
  const dp = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
  for (let j = 0; j <= b.length; j++) dp[0][j] = j;
  for (let i = 1; i <= a.length; i++) for (let j = 1; j <= b.length; j++) {
    dp[i][j] = a[i - 1] === b[j - 1] ? dp[i - 1][j - 1] : 1 + Math.min(dp[i - 1][j], dp[i][j - 1], dp[i - 1][j - 1]);
  }
  return dp[a.length][b.length];
}
function closestGitSubcommand(word) {
  if (word.length < 2) return null;
  let best = null; let bestDist = Infinity;
  for (const name of KNOWN_GIT_SUBCOMMANDS) {
    const dist = levenshtein(word, name);
    if (dist < bestDist) { bestDist = dist; best = name; }
  }
  // Only offer it as a correction when it's plausibly a typo, not just any
  // vaguely-similar word — scales with length so short typos still match.
  return bestDist <= Math.max(1, Math.ceil(word.length / 3)) ? best : null;
}

function renderGitHints() {
  const box = $('#commandGitHints');
  if (state.consoleMode !== 'console') { box.hidden = true; return; }
  const raw = $('#commandInput').value;
  const words = raw.split(/\s+/).filter(Boolean);
  const hasGitPrefix = words[0]?.toLowerCase() === 'git';
  const sub = words[hasGitPrefix ? 1 : 0]?.toLowerCase();
  if (!sub) { box.hidden = true; return; }
  const entry = GIT_COMMAND_HELP[sub];
  if (!entry) {
    // Arbitrary shell commands should not receive irrelevant Git typo hints.
    // Only a literal `git ...` or a prefix of a known bare Git subcommand
    // participates in the Git helper.
    const matches = words.length === (hasGitPrefix ? 2 : 1) ? Object.keys(GIT_COMMAND_HELP).filter(name => name.startsWith(sub)) : [];
    if (matches.length) {
      box.hidden = false;
      box.innerHTML = `<div class="hint-subcommands">${matches.map(name => `<button type="button" class="hint-sub" data-fill-sub="${esc(`git ${name}`)}">git ${esc(name)}</button>`).join('')}</div>`;
      return;
    }
    // Not a known subcommand, and not a prefix of one either — check for a typo.
    const suggestion = hasGitPrefix && !KNOWN_GIT_SUBCOMMANDS.includes(sub) ? closestGitSubcommand(sub) : null;
    if (suggestion) {
      const corrected = ['git', suggestion, ...words.slice(2)].join(' ');
      box.hidden = false;
      box.innerHTML = `<div class="hint-typo">Did you mean <button type="button" class="hint-sub" data-fill-sub="${esc(corrected)}">${esc(corrected)}</button>?</div>`;
      return;
    }
    box.hidden = true; return;
  }
  const already = new Set(words.slice(hasGitPrefix ? 2 : 1).map(w => w.split('=')[0]));
  const flags = entry.flags.filter(f => !already.has(f.flag.split(/[\s=]/)[0]));
  box.hidden = false;
  box.innerHTML = `<p class="hint-desc"><b>git ${esc(sub)}</b> — ${esc(entry.description)}</p>${flags.length ? `<div class="hint-flags">${flags.map(f => `<button type="button" class="hint-flag" data-append-flag="${esc(f.flag)}"><b>${esc(f.flag)}</b><span>${esc(f.desc)}</span></button>`).join('')}</div>` : ''}`;
}
$('#commandGitHints').addEventListener('click', (e) => {
  const fillSub = e.target.closest('[data-fill-sub]');
  if (fillSub) { $('#commandInput').value = `${fillSub.dataset.fillSub} `; $('#commandInput').focus(); renderGitHints(); return; }
  const appendFlag = e.target.closest('[data-append-flag]');
  if (appendFlag) {
    const input = $('#commandInput');
    const sep = input.value === '' || input.value.endsWith(' ') ? '' : ' ';
    input.value = `${input.value}${sep}${appendFlag.dataset.appendFlag} `;
    input.focus(); renderGitHints();
  }
});

function setConsoleMode(mode) {
  const input = $('#commandInput');
  const previousMode = state.consoleMode;
  if (previousMode !== mode) {
    state.consoleDrafts[previousMode] = input.value;
    state.consoleMode = mode;
    input.value = state.consoleDrafts[mode] || '';
  }
  document.querySelectorAll('.command-tab').forEach(tab => tab.classList.toggle('active', tab.dataset.mode === mode));
  $('#terminalQuickCommands').hidden = mode !== 'console';
  $('#terminalScopeBar').hidden = !['console', 'saved'].includes(mode);
  $('#commandAddSaved').hidden = mode !== 'saved';
  if (mode === 'console') {
    $('#commandModeHint').textContent = 'Terminal keeps its own transcript and command draft. Use quick Git buttons or type any command.';
    input.placeholder = 'Type a command… e.g. git status, pwd, gh pr status';
    $('#commandHelp').textContent = '↑↓ history · Enter to run · “Run in” is the exact working directory · use “… here” shortcuts to path-filter Git';
    updateConsoleScopeLabel(); renderConsoleTranscript(); renderGitHints();
  } else if (mode === 'saved') {
    $('#commandModeHint').textContent = 'Saved actions are your named command groups. They run in order and stop on the first failed command.';
    input.placeholder = 'Search saved actions…';
    $('#commandHelp').textContent = 'Enter/click Run to execute in “Run in” · ＋ Save adds or edits reusable commands';
    updateConsoleScopeLabel(); $('#commandGitHints').hidden = true; $('#commandClearTranscript').hidden = true; $('#commandCopyTranscript').hidden = true; renderSavedActions(input.value);
  } else {
    activeCommands = buildCommands();
    $('#commandModeHint').textContent = 'App actions include built-in actions plus your Saved actions. Search by name or command.';
    input.placeholder = 'Type a command… (Cmd/Ctrl+K)';
    $('#commandHelp').textContent = '↑↓ to navigate · Enter to run · Esc to close';
    $('#commandScope').textContent = ''; $('#commandScope').title = ''; $('#commandList').classList.remove('console-transcript'); $('#commandGitHints').hidden = true; $('#commandClearTranscript').hidden = true; $('#commandCopyTranscript').hidden = true; renderCommandList(input.value);
  }
}

function openCommandPalette() {
  const input = $('#commandInput');
  commandPaletteOpen = true;
  activeCommands = buildCommands();
  setConsoleMode(state.consoleMode || 'console');
  $('#commandPalette').showModal();
  input.focus();
}
function filterCommands(query) {
  if (isRawGitQuery(query)) { const terminalText = rawGitArgs(query); setConsoleMode('console'); setCommandInputValue(terminalText); renderGitHints(); return; }
  renderCommandList(query);
}
$('#commandInput').addEventListener('input', (e) => {
  state.consoleDrafts[state.consoleMode] = e.target.value;
  if (state.consoleMode === 'console') { consoleHistoryPointer = -1; consoleHistoryDraft = e.target.value; renderGitHints(); return; }
  if (state.consoleMode === 'saved') { renderSavedActions(e.target.value); return; }
  filterCommands(e.target.value);
});
$('#commandInput').addEventListener('keydown', (e) => {
  if (state.consoleMode === 'console') {
    if (e.key === 'Enter') { e.preventDefault(); const command = $('#commandInput').value.trim(); if (command) { $('#commandInput').value = ''; renderGitHints(); runTerminalFromConsole(command); } }
    else if (e.key === 'ArrowUp') { e.preventDefault(); recallConsoleHistory(-1); renderGitHints(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); recallConsoleHistory(1); renderGitHints(); }
    return;
  }
  if (state.consoleMode === 'saved') {
    const items = Array.from($('#commandList').querySelectorAll('.command-item[data-saved-index]'));
    const selected = items.find(i => i.classList.contains('selected'));
    if (e.key === 'ArrowDown') { e.preventDefault(); const next = selected?.nextElementSibling || items[0]; items.forEach(i => i.classList.remove('selected')); next?.classList.add('selected'); next?.scrollIntoView({ block: 'nearest' }); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); const prev = selected?.previousElementSibling || items[items.length - 1]; items.forEach(i => i.classList.remove('selected')); prev?.classList.add('selected'); prev?.scrollIntoView({ block: 'nearest' }); }
    else if (e.key === 'Enter') { e.preventDefault(); const item = state.savedActions[Number(selected?.dataset.savedIndex)]; if (item) runSavedAction(item); }
    return;
  }
  const items = Array.from($('#commandList').querySelectorAll('.command-item'));
  const selected = items.find(i => i.classList.contains('selected'));
  if (e.key === 'ArrowDown') { e.preventDefault(); const next = selected?.nextElementSibling || items[0]; items.forEach(i => i.classList.remove('selected')); next?.classList.add('selected'); next?.scrollIntoView({ block: 'nearest' }); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); const prev = selected?.previousElementSibling || items[items.length - 1]; items.forEach(i => i.classList.remove('selected')); prev?.classList.add('selected'); prev?.scrollIntoView({ block: 'nearest' }); }
  else if (e.key === 'Enter') {
    e.preventDefault();
    if (selected?.dataset.rawGitSuggest !== undefined) { runTerminalFromConsole(selected.dataset.rawGitSuggest); return; }
    const cmd = activeCommands.find(c => c.id === selected?.dataset.cmdId); if (cmd) { if (cmd.keepOpen) cmd.fn(); else { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; } }
  }
});
$('#commandList').addEventListener('click', (e) => {
  const reuse = e.target.closest('[data-console-reuse]');
  if (reuse) { const entry = state.consoleTranscript[Number(reuse.dataset.consoleReuse)]; if (entry) { setCommandInputValue(entry.command); $('#commandInput').focus(); renderGitHints(); } return; }
  const copy = e.target.closest('[data-console-copy]');
  if (copy) { const entry = state.consoleTranscript[Number(copy.dataset.consoleCopy)]; if (entry) copyText(consoleEntryAsText(entry), 'Command output copied.'); return; }
  const suggest = e.target.closest('[data-raw-git-suggest]');
  if (suggest) { runTerminalFromConsole(suggest.dataset.rawGitSuggest); return; }
  const savedRun = e.target.closest('[data-saved-run]');
  if (savedRun) { const item = state.savedActions[Number(savedRun.dataset.savedRun)]; if (item) runSavedAction(item); return; }
  const savedEdit = e.target.closest('[data-saved-edit]');
  if (savedEdit) { addOrEditSavedAction(Number(savedEdit.dataset.savedEdit)); return; }
  const savedDelete = e.target.closest('[data-saved-delete]');
  if (savedDelete) { deleteSavedAction(Number(savedDelete.dataset.savedDelete)); return; }
  const item = e.target.closest('.command-item');
  if (item?.dataset.savedIndex !== undefined) { const saved = state.savedActions[Number(item.dataset.savedIndex)]; if (saved) runSavedAction(saved); return; }
  if (item && item.dataset.cmdId) {
    const cmd = activeCommands.find(c => c.id === item.dataset.cmdId);
    if (cmd) { if (cmd.keepOpen) cmd.fn(); else { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; } }
  }
});
document.querySelectorAll('.command-tab').forEach(tab => tab.addEventListener('click', () => { setConsoleMode(tab.dataset.mode); $('#commandInput').focus(); }));
$('#terminalQuickCommands').addEventListener('click', (e) => { const quick = e.target.closest('[data-terminal-fill]'); if (quick) { setCommandInputValue(quick.dataset.terminalFill); $('#commandInput').focus(); renderGitHints(); } });
$('#commandAddSaved').addEventListener('click', () => addOrEditSavedAction());
$('#commandScopeSelect').addEventListener('change', (event) => { selectConsoleScope(event.target.value); if (state.consoleMode === 'console') renderConsoleTranscript(); });
$('#commandClearTranscript').addEventListener('click', () => { state.consoleTranscript = []; renderConsoleTranscript(); });
$('#commandCopyTranscript').addEventListener('click', () => copyText(state.consoleTranscript.map(consoleEntryAsText).join('\n\n'), 'Terminal transcript copied.'));
$('#closeCommandPalette').addEventListener('click', () => $('#commandPalette').close());
$('#commandPalette').addEventListener('close', () => { commandPaletteOpen = false; });
$('#openConsole').addEventListener('click', () => openCommandPalette());
$('#openHelp').addEventListener('click', () => $('#helpDialog').showModal());
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'k') { e.preventDefault(); commandPaletteOpen ? $('#commandPalette').close() : openCommandPalette(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'o') { e.preventDefault(); openRepository(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'C')) { e.preventDefault(); state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); refreshChangesLightweight(); refs.commitMessage.focus(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'P')) { e.preventDefault(); $('#pushCurrent').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'F')) { e.preventDefault(); $('#fetchCurrent').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'S')) { e.preventDefault(); stashWork(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'L')) { e.preventDefault(); $('#navCommander').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'G')) { e.preventDefault(); $('#navGraph').click(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'f' && !['input','textarea'].includes(document.activeElement.tagName.toLowerCase())) { e.preventDefault(); refs.search.focus(); }
});

// ---- Pull request status panel (read-only, first incremental step) ----
//
// A self-contained factory, not a singleton tied to the sidebar: it owns its
// own DOM subtree, its own manual-connect lifecycle, and takes its repository
// path and branch through callbacks instead of reading global `state`
// directly — so a later "Project Status" view can create one instance per
// repository (the parent, and one per submodule) just by pointing each at a
// different root element and a different {path, branch} pair, with no
// changes needed here. `pr_status` never receives or returns a token. The
// backend prefers the optional `gh` CLI, and on systems without it (notably
// managed Windows machines) uses GitHub's API with the HTTPS credential held
// by Git Credential Manager. Credentials remain entirely outside this DOM.
//
// Loading is independent of the rest of the repository UI: this never awaits
// anything the explorer/graph/commander views depend on, and nothing here
// blocks them — a slow or failed PR check only ever affects this one panel.
// Network access is deliberately explicit: opening the section only reveals
// a Connect button. A request is made only when the user presses Connect,
// Refresh, or Retry. There is no timer/polling, including while expanded.
const PR_STATE_LABELS = {
  loading: 'Checking pull request status…',
  superseded: 'Checking pull request status…', // a newer check already superseded this one; about to be replaced
  no_remote: 'No remote configured for this repository.',
  detached_head: 'HEAD is detached — not on a branch, so there is no branch to check for a pull request.',
  no_branch: 'No branch is currently checked out.',
  unsupported_provider: null, // uses the backend's own detail message verbatim
  auth_missing: null,
  api_error: null,
  no_open_pr: null, // built from result.branch + result.queried_repo
  no_upstream: null,
  partial_result: null, // built the same way, with an "incomplete" note
};
const { prCardHtml } = window.GitDrillDownPr;

function githubPullsBrowserUrl(queriedRepo) {
  const parts = String(queriedRepo || '').split('/');
  if (parts.length !== 3 || !parts.every(Boolean)) return '';
  const [host, owner, repo] = parts;
  if (!(host === 'github.com' || host === 'github' || host.startsWith('github.'))) return '';
  return `https://${host}/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls`;
}

function createPrStatusPanel(root, options) {
  let expanded = false;
  let generation = 0;
  let lastKey = null;

  // A short "what is this panel showing right now" line — the submodule's
  // name when the Submodule Branch Map is open, so it's unmistakable the PR
  // context follows the view and isn't the parent's.
  function contextHeaderHtml() {
    const title = options.getContextTitle?.();
    return title ? `<div class="pr-status-context">${esc(title)}</div>` : '';
  }

  // Point 2 of the report: outgoing ("this branch is the PR's head/source")
  // and incoming ("this branch is the PR's base/target") are two genuinely
  // different relationships — shown as two clearly, separately labeled
  // sections rather than one merged list, which is exactly how a real
  // incoming PR (this branch as someone else's *target*) went unnoticed
  // before: there was nowhere for it to be shown as a different kind of
  // result at all.
  function prDirectionSectionHtml(label, prs) {
    if (!prs.length) return '';
    const heading = `<div class="pr-status-direction-heading">${esc(label)}${prs.length > 1 ? ` (${prs.length})` : ''}</div>`;
    return heading + prs.map(prCardHtml).join('');
  }

  function manualRefreshHtml() {
    return '<div class="pr-status-manual"><span>Connected on demand · no background checks</span><button class="pr-status-retry" data-pr-status-action="refresh">Refresh</button></div>';
  }

  function renderConnectPrompt() {
    root.innerHTML = contextHeaderHtml() + '<div class="pr-status-empty pr-status-disconnected"><span>Connect only when you want to check GitHub. No request runs in the background.</span><button class="pr-status-retry" data-pr-status-action="connect">Connect to GitHub</button></div>';
  }

  function renderState(result) {
    const ctx = contextHeaderHtml();
    if (result.state === 'loading' || result.state === 'superseded') { root.innerHTML = ctx + '<div class="pr-status-loading"><i class="spinner"></i>Checking pull request status…</div>'; return; }
    const outgoing = result.outgoing_pull_requests || [];
    const incoming = result.incoming_pull_requests || [];
    if (result.state === 'ok' && (outgoing.length || incoming.length)) {
      const partialNote = result.partial ? '<div class="pr-status-partial">Not every related repository could be checked — there may be more.</div>' : '';
      root.innerHTML = ctx + manualRefreshHtml() + partialNote + prDirectionSectionHtml('Pull requests from this branch', outgoing) + prDirectionSectionHtml('Pull requests into this branch', incoming);
      return;
    }
    const message = PR_STATE_LABELS[result.state] || result.detail || 'Pull request status unavailable.';
    const where = result.queried_repo ? ` in <code>${esc(result.queried_repo)}</code>` : '';
    const branchCode = `<code>${esc(result.branch || 'this branch')}</code>`;
    let detail;
    if (result.state === 'no_open_pr') detail = `No open pull request for ${branchCode}${where}.`;
    else if (result.state === 'no_upstream') detail = `No open pull request for ${branchCode}${where}. This branch has no upstream set — push it and set an upstream so it can be matched to a PR.`;
    else if (result.state === 'partial_result') detail = `No open pull request found for ${branchCode}${where} — but not every related repository could be checked, so this may be incomplete.`;
    else detail = esc(message);
    const retryable = ['api_error', 'auth_missing', 'partial_result'].includes(result.state);
    const browserUrl = githubPullsBrowserUrl(result.queried_repo);
    const browserFallback = browserUrl && retryable ? `<button class="pr-open-link" data-open-url="${esc(browserUrl)}">Open pull requests in browser ↗</button>` : '';
    const actionLabel = retryable ? 'Retry' : 'Refresh';
    root.innerHTML = ctx + `<div class="pr-status-empty pr-status-${esc(result.state)}">${detail}<button class="pr-status-retry" data-pr-status-action="refresh">${actionLabel}</button>${browserFallback}</div>`;
  }

  function contextKey() {
    return `${options.getRepositoryPath() || ''}::${options.getBranch() || ''}::${options.getContextLabel?.() || ''}`;
  }

  async function load() {
    const repositoryPath = options.getRepositoryPath();
    const branch = options.getBranch();
    const context = options.getContextLabel?.() || null;
    lastKey = contextKey();
    if (!repositoryPath) { renderState({ state: 'no_repository', detail: 'No repository open.', outgoing_pull_requests: [], incoming_pull_requests: [] }); return; }
    const myGeneration = ++generation;
    renderState({ state: 'loading', outgoing_pull_requests: [], incoming_pull_requests: [] });
    if (!invoke) { renderState({ state: 'no_open_pr', branch, outgoing_pull_requests: [], incoming_pull_requests: [] }); return; }
    try {
      const result = await invoke('pr_status', { repositoryPath, branch: branch || null, context });
      if (myGeneration !== generation) return; // a newer load (or context change) superseded this one
      renderState(result);
    } catch (error) {
      if (myGeneration !== generation) return;
      renderState({ state: 'api_error', detail: String(error), pull_requests: [] });
    }
  }

  function setExpanded(next) {
    expanded = next;
    root.classList.toggle('collapsed', !expanded);
    if (expanded && lastKey !== contextKey()) renderConnectPrompt();
  }

  // A context change invalidates the displayed server result, but never starts
  // a replacement request. The user explicitly connects for the new branch.
  function refreshIfContextChanged() {
    const key = contextKey();
    if (key === lastKey) return;
    generation += 1; // ignore a response still in flight for the old context
    lastKey = null;
    if (expanded) renderConnectPrompt();
  }

  root.addEventListener('click', event => {
    if (event.target.closest('[data-pr-status-action]')) load();
  });
  root.addEventListener('click', event => {
    const details = event.target.closest('[data-open-check-url]');
    if (!details) return;
    const url = details.dataset.openCheckUrl;
    if (invoke) invoke('open_status_check_url', { url }).catch(error => status(String(error), 'error'));
    else window.open(url, '_blank', 'noopener');
  });
  root.addEventListener('submit', async event => {
    const form = event.target.closest('[data-pr-comment-form]');
    if (!form) return;
    event.preventDefault();
    const input = form.querySelector('textarea');
    const feedback = form.querySelector('[role="status"]');
    const button = form.querySelector('button[type="submit"]');
    const body = input.value.trim();
    if (!body) { feedback.textContent = 'Write a comment first.'; input.focus(); return; }
    if (!invoke) { feedback.textContent = 'Desktop app required.'; return; }
    button.disabled = true; input.disabled = true; feedback.textContent = 'Sending…';
    try {
      await invoke('post_pull_request_comment', { repositoryPath: options.getRepositoryPath(), pullRequestUrl: form.dataset.prUrl, body });
      input.value = ''; feedback.textContent = 'Comment sent.'; showOperationToast('Pull request comment sent.', 'success');
    } catch (error) {
      feedback.textContent = String(error); showOperationToast(`Comment was not sent: ${String(error)}`, 'error');
    } finally { button.disabled = false; input.disabled = false; }
  });

  return { setExpanded, refreshIfContextChanged, isExpanded: () => expanded };
}

// Message D, point 5: "Project pull request" always means the PARENT
// project and its own checked-out branch — never silently replaced by a
// submodule's just because its Branch Map happens to be open. (It used to
// follow state.submoduleGraph the same way the sidebar's own branch list
// incorrectly did — same root cause, same fix: never let a repository-
// sensitive panel default to "whatever's currently on screen" instead of
// an explicit, named repository.) The separate submodulePrStatusPanel
// below is where a submodule's own PR status actually lives now.
const mainPrStatusPanel = createPrStatusPanel(refs.prStatusPanel, {
  getRepositoryPath: () => state.repository?.path || null,
  getBranch: () => (state.repository && !state.repository.head_detached) ? (state.repository.current_branch || null) : null,
  getContextLabel: () => 'parent',
  getContextTitle: () => null,
});
refs.togglePrStatus.addEventListener('click', () => {
  const next = !mainPrStatusPanel.isExpanded();
  refs.prStatusArrow.textContent = next ? '▾' : '▸';
  mainPrStatusPanel.setExpanded(next);
});
refs.prStatusPanel.addEventListener('click', event => {
  const link = event.target.closest('[data-open-url]');
  if (!link || link.disabled) return;
  const url = link.dataset.openUrl;
  if (invoke) invoke('open_external_url', { url }).catch(error => status(String(error), 'error'));
  else window.open(url, '_blank', 'noopener');
});

// A separate, clearly-labeled section for the currently-open submodule's
// own PR status — only ever visible while state.submoduleGraph is set (see
// renderSubmodulePrHeading, called from render()). A detached submodule
// reports no branch here (getBranch returns null), which the existing
// panel/backend already render as an explicit "detached HEAD, no PR"
// state rather than silently showing nothing.
const submodulePrStatusPanel = createPrStatusPanel(refs.submodulePrStatusPanel, {
  getRepositoryPath: () => state.submoduleGraph?.repository?.path || null,
  getBranch: () => (state.submoduleGraph && !state.submoduleGraph.repository.head_detached) ? (state.submoduleGraph.repository.current_branch || null) : null,
  getContextLabel: () => (state.submoduleGraph ? `submodule:${state.submoduleGraph.name}` : 'none'),
  getContextTitle: () => null,
});
refs.toggleSubmodulePrStatus.addEventListener('click', () => {
  const next = !submodulePrStatusPanel.isExpanded();
  refs.submodulePrStatusArrow.textContent = next ? '▾' : '▸';
  submodulePrStatusPanel.setExpanded(next);
});
refs.submodulePrStatusPanel.addEventListener('click', event => {
  const link = event.target.closest('[data-open-url]');
  if (!link || link.disabled) return;
  const url = link.dataset.openUrl;
  if (invoke) invoke('open_external_url', { url }).catch(error => status(String(error), 'error'));
  else window.open(url, '_blank', 'noopener');
});

if (!invoke) {
  refs.browserNotice.hidden = false;
  if (new URLSearchParams(location.search).has('demo')) Object.assign(state, previewData);
}
renderRecentRepos();
render();
if (invoke) invoke('build_info').then(sha => { $('#buildInfo').textContent = `build ${sha}`; }).catch(() => {});

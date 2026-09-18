const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'frontend/index.html'), 'utf8');
const app = fs.readFileSync(path.join(root, 'frontend/app.js'), 'utf8');

test('Terminal is the first default command panel and has an explicit close button', () => {
  const terminalTab = html.indexOf('data-mode="console"');
  const actionsTab = html.indexOf('data-mode="commands"');
  assert.ok(terminalTab >= 0 && terminalTab < actionsTab);
  assert.match(html, /id="closeCommandPalette"[^>]*>×<\/button>/);
  assert.match(app, /function openCommandPalette\(\)[\s\S]*?setConsoleMode\('console'\)/);
});

test('Add submodule starts at the Vitesco engineering namespace', () => {
  assert.match(app, /const defaultSubmoduleUrl = '\.\.\/\.\.\/eng\/'/);
  assert.match(app, /setSelectionRange\(defaultSubmoduleUrl\.length, defaultSubmoduleUrl\.length\)/);
  assert.match(html, /PORTABLE REPOSITORY URL/);
  assert.match(html, /Use <code>\.\.\/\.\.\/ORG\/REPO<\/code>/);
});

test('Vitesco browser URLs are suggested as portable gitmodules URLs', () => {
  assert.match(app, /function portableSubmoduleUrl\(url\)/);
  assert.match(app, /return `\.\.\/\.\.\/\$\{match\[1\]\}\/\$\{match\[2\]\}`/);
  assert.match(app, /Portable \.gitmodules URL for submodule/);
});

test('cancelling a child branch or tag dialog preserves the submodule version selector', () => {
  assert.doesNotMatch(app, /const entry = submoduleMenuEntry;\s*refs\.submoduleMenu\.hidden = true;\s*if \(versionFilter === 'tag'\)/);
  assert.match(app, /const childActionOpen = refs\.newBranchDialog\.open \|\| \$\('#newTagDialog'\)\.open/);
  assert.match(app, /if \(!childActionOpen && !refs\.submoduleMenu\.hidden/);
});

test('slow Git actions expose progress on the exact button that was pressed', () => {
  assert.match(app, /function beginButtonOperation\(button, label\)[\s\S]*?button\.disabled = true;[\s\S]*?aria-busy[\s\S]*?spinner/);
  assert.match(app, /switchSubmoduleVersion\(row\.dataset\.revision, row\.dataset\.versionKind, row\.dataset\.name, button\)/);
  assert.match(app, /const finishButton = beginButtonOperation\(button, 'Switching…'\)/);
  assert.match(app, /finally \{ finishButton\(\); \}/);
});

test('deleted tracked paths render without trying to stat a path that no longer exists', () => {
  assert.match(app, /\['deleted', 'deleted-folder', 'deleted-submodule'\]\.includes\(state\.selectedEntry\?\.kind\)/);
  assert.match(app, /Deleted tracked file/);
  assert.match(app, /Deleted tracked folder/);
});

test('a submodule row shows its attached branch or detached state, only when actually checked', () => {
  // Must never guess: a clean, fully-synced submodule skips the check
  // entirely (submodule_checked stays false) to avoid opening every
  // submodule's repository on every folder listing — this function must
  // fall back to the generic hint rather than claim "detached".
  assert.match(app, /function submoduleHeadHint\(entry\) \{\s*if \(!entry\.submodule_checked\) return 'Independent Git repository';/);
  assert.match(app, /entry\.submodule_current_branch \? `Independent Git repository · \$\{entry\.submodule_current_branch\}` : 'Independent Git repository · detached'/);
  assert.match(app, /entry\.kind === 'submodule' \? esc\(submoduleHeadHint\(entry\)\)/);
});

test('Reset to upstream is reachable for a submodule that is only dirty, not just ahead/behind', () => {
  // submodule_is_dirty is already computed by load_directory for this exact
  // row whenever there was anything to explain — reused here instead of a
  // second, new check, and only meaningful for whichever branch is actually
  // the active checkout right now.
  assert.match(app, /dirty: item\.current && !!submoduleMenuEntry\?\.submodule_is_dirty/);
});

test('a submodule row does not show a generic action next to its identically-behaving dedicated one', () => {
  // "View history"/"Open on server" and "Submodule Branch Map"/"Open
  // submodule repository ↗" call the exact same function with the exact
  // same arguments for a submodule entry — report: two buttons doing one
  // thing is confusing, keep only the more specifically labeled one.
  assert.match(app, /entry\.kind === 'submodule' \? '' : '<button data-detail-action="server">Open on server ↗<\/button>'/);
  assert.match(app, /entry\.kind === 'submodule' \? '' : '<button data-detail-action="history">View history<\/button>'/);
});

test('the status bar quietly shows the last real git command, and the footer opens its full history on double-click', () => {
  // 'busy' means the action is still in flight — the command that will
  // explain it hasn't been recorded on the backend yet, so refreshing then
  // would show last time's stale command instead of this one's.
  assert.match(app, /function status\(message, kind = ''\) \{[\s\S]*?if \(kind !== 'busy'\) refreshCommandHint\(\);/);
  assert.match(app, /async function refreshCommandHint\(\)[\s\S]*?invoke\('recent_git_commands'\)/);
  assert.match(app, /refs\.statusFooter\.addEventListener\('dblclick', openCommandHistoryDialog\)/);
  assert.match(html, /id="commandHistoryDialog"/);
  assert.match(html, /id="statusCommandHint"/);
  // Its own row, not a reuse of .publish-commit: that class assumes a
  // 4-column grid (checkbox/index, dot, 1fr content, badge) built for a
  // clickable, togglable push-commit list — neither applies to this plain,
  // unclickable 2-column read history, and .excluded means "de-prioritized"
  // there, the wrong signal for "this command failed".
  assert.match(app, /function commandHistoryRowHtml\(entry\) \{[\s\S]*?class="command-history-row \$\{entry\.success \? '' : 'failed'\}"/);
});

test('Stash folder disappears with its Explorer row-mates outside Explorer, never left stranded alone', () => {
  // Every other button in that toolbar row (Commit folder, Add submodule,
  // History, Changes in folder) already hides whenever state.view isn't
  // 'explorer' — before this, Stash folder was the one left behind, alone,
  // in an otherwise-empty header the moment you switched to Graph/Folder
  // Sync/Remotes. The Ctrl+Shift+S shortcut calls stashWork() directly, so
  // it stays reachable everywhere regardless of this element's visibility.
  assert.match(app, /\$\('#stashWork'\)\.hidden = state\.view !== 'explorer' \|\| !state\.repository;/);
});

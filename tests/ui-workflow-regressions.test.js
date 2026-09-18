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

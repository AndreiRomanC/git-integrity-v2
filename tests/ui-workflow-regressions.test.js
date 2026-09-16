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
  assert.match(app, /const defaultSubmoduleUrl = 'https:\/\/github\.vitesco\.io\/eng\/'/);
  assert.match(app, /setSelectionRange\(defaultSubmoduleUrl\.length, defaultSubmoduleUrl\.length\)/);
});

test('cancelling a child branch or tag dialog preserves the submodule version selector', () => {
  assert.doesNotMatch(app, /const entry = submoduleMenuEntry;\s*refs\.submoduleMenu\.hidden = true;\s*if \(versionFilter === 'tag'\)/);
  assert.match(app, /const childActionOpen = refs\.newBranchDialog\.open \|\| \$\('#newTagDialog'\)\.open/);
  assert.match(app, /if \(!childActionOpen && !refs\.submoduleMenu\.hidden/);
});

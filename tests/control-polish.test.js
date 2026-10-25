'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { submoduleVersionRowHtml } = require('../frontend/submodule-versions.js');
const read = name => fs.readFileSync(path.join(__dirname, '../frontend', name), 'utf8');
const html = read('index.html');
const css = read('styles.css');
const button = id => {
  const matches = [...html.matchAll(new RegExp(`<button\\b[^>]*id="${id}"[^>]*>[^]*?<\\/button>`, 'g'))];
  assert.equal(matches.length, 1, `${id} remains a single existing action`);
  return matches[0][0];
};

test('comparison search labels remain accessible without consuming visual space', () => {
  assert.equal((html.match(/class="sr-only">Find in comparison<\/span>/g) || []).length, 2);
  const rule = css.match(/\.sr-only \{([^}]*)\}/)[1];
  for (const expected of ['position: absolute', 'width: 1px', 'height: 1px', 'clip-path: inset(50%)']) assert.ok(rule.includes(expected));
  assert.doesNotMatch(rule, /display:\s*none|visibility:\s*hidden/);
});

test('both file comparison toolbars retain search, difference and export actions in named groups', () => {
  for (const name of ['Find text', 'Changed blocks', 'Export comparison']) {
    assert.equal(html.split(`role="group" aria-label="${name}"`).length - 1, 2);
  }
  for (const id of ['previousCompareSearch', 'nextCompareSearch', 'previousLocalDriveSearch', 'nextLocalDriveSearch']) {
    const markup = button(id);
    assert.match(markup, /type="button"/);
    assert.match(markup, /aria-label="(?:Previous|Next) text match"/);
    assert.match(markup, /disabled/);
    assert.match(markup, /<svg class="ui-icon"[^>]*aria-hidden="true"[^>]*focusable="false"/);
  }
  for (const id of ['previousCompareDifference', 'nextCompareDifference', 'previousLocalDriveDifference', 'nextLocalDriveDifference']) {
    assert.match(button(id), /<\/svg>(?:Previous|Next) block<\/button>/);
  }
  assert.match(css, /\.comparison-tools button \{[^}]*height: 30px;[^}]*font-weight: 500/);
  assert.match(css, /\.comparison-tools button:focus-visible/);
});

test('snapshot buttons use the same decorative tray icon and retain export labels and disabled state', () => {
  const icon = /<svg class="ui-icon"[^>]*aria-hidden="true"[^>]*focusable="false"><path d="M10 2v10m-4-4 4 4 4-4M3 13v4h14v-4"\/><\/svg>/;
  for (const id of ['compareDownload', 'downloadLocalDriveComparison', 'gitCompareDownload', 'subCompareDownload', 'downloadLocalFolderComparison']) {
    const markup = button(id);
    assert.match(markup, icon);
    assert.match(markup, /aria-label="(?:Download|Export)[^"]+"/);
    assert.match(markup, /disabled/);
  }
  for (const revision of ['', '0123456789abcdef']) {
    const row = submoduleVersionRowHtml({ kind: 'commit', revision, subject: 'Example' });
    const markup = row.match(/<button[^>]*data-download-version[^]*?<\/button>/)[0];
    assert.match(markup, icon);
    assert.equal(/disabled/.test(markup), !revision);
    assert.match(row, /data-switch-version/); // Checkout is preserved, not replaced by export.
  }
});

test('restore action hierarchy is scoped to Restore and preserves cancel and preview semantics', () => {
  const restore = html.match(/<dialog id="folderRestoreDialog"[^]*?<\/dialog>/)[0];
  assert.match(restore, /<button value="cancel" class="restore-cancel">Cancel<\/button>/);
  assert.match(button('previewFolderRestore'), /type="button"[^>]*class="restore-preview"/);
  assert.match(button('confirmFolderRestore'), /type="button"[^>]*class="danger-action-soft" disabled/);
  assert.match(css, /\.folder-restore-dialog footer \.restore-cancel \{[^}]*background: transparent/);
  assert.match(css, /\.folder-restore-dialog footer \.restore-preview \{[^}]*border: 1px solid/);
  assert.match(css, /\.folder-restore-dialog footer \.danger-action-soft \{[^}]*background: #3a2a12/);
  assert.match(css, /\.folder-restore-dialog button:focus-visible/);
});

test('Branch Story navigation has an explicit quiet style and keyboard focus, not native default buttons', () => {
  assert.match(css, /\.story-merge-source button \{[^}]*border: 1px solid[^}]*background: transparent/);
  assert.match(css, /\.story-merge-source button:focus-visible/);
});

test('Folder Compare rows stay flat while header actions keep their button styling', () => {
  assert.doesNotMatch(css, /\.local-drive-pane button\b/);
  assert.match(css, /\.local-drive-pane > header button \{[^}]*border: 1px solid/);
  const row = css.match(/\.local-drive-entry \{([^}]*)\}/)[1];
  for (const expected of ['width: 100%', 'min-height: 24px', 'display: grid', 'border: 0;', 'border-bottom: 1px solid', 'border-radius: 0', 'background: transparent']) {
    assert.ok(row.includes(expected), expected);
  }
  assert.match(css, /\.local-drive-entry:hover:not\(\.selected\) \{[^}]*background:/);
  assert.match(css, /\.local-drive-entry\.selected \{[^}]*background: #23517a;[^}]*color: white/);
  assert.match(css, /\.local-drive-entry:focus-visible \{[^}]*outline: 1px solid[^}]*outline-offset: -1px/);
});

test('clearing details uses the same static document icon as startup and still clears selection safely', () => {
  const app = read('app.js');
  const start = app.indexOf('function clearDetails(');
  assert.ok(start >= 0);
  const addedClasses = [];
  const details = { displayedEntry: { path: 'old.txt' }, innerHTML: '', closest: selector => {
    assert.equal(selector, '.main-layout');
    return { classList: { add: name => addedClasses.push(name) } };
  } };
  const sandbox = { refs: { details }, esc: value => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;') };
  vm.runInNewContext(app.slice(start, app.indexOf('\n}', start) + 2), sandbox);
  for (const message of ['Select a file or folder', 'Select a commit', '<selection> & details']) {
    sandbox.clearDetails(message);
    assert.equal(details.displayedEntry, null);
    assert.match(details.innerHTML, /<strong>/);
    assert.ok(details.innerHTML.includes(sandbox.esc(message)));
    const icon = details.innerHTML.match(/<svg class="ui-icon details-empty-icon"[^]*?<\/svg>/)?.[0];
    assert.ok(icon && html.includes(icon), 'initial and runtime empty states share the icon');
    assert.match(icon, /aria-hidden="true" focusable="false"/);
    assert.doesNotMatch(details.innerHTML, /details-node|spinner|progressbar|<circle/);
  }
  assert.deepEqual(addedClasses, Array(3).fill('details-empty-state'));
  assert.doesNotMatch(css.match(/\.details-empty-icon \{([^}]*)\}/)[1], /animation|border-radius/);
});

test('comparison context is expandable, while revisions and folder actions remain outside it', () => {
  const details = html.match(/<details id="compareContextDetails"[^]*?<\/details>/)[0];
  assert.doesNotMatch(details.split('>')[0], /\bopen\b/);
  assert.match(details, /<summary[^>]*>Details<\/summary>/);
  for (const id of ['compareOrigin', 'compareSnapshotSummary']) {
    assert.match(details, new RegExp(`id="${id}"`));
    assert.equal(html.split(`id="${id}"`).length - 1, 1);
  }
  assert.match(details, /does not contact the server or change your checkout/);
  for (const id of ['branchCompareKindSelect', 'branchCompareLeftRef', 'branchCompareRightRef', 'remoteRef', 'gitCompareTree', 'gitCompareFlatToggle', 'gitCompareDownload']) {
    assert.doesNotMatch(details, new RegExp(`id="${id}"`));
    assert.equal(html.split(`id="${id}"`).length - 1, 1, `${id} is moved, not duplicated`);
  }
  assert.match(css, /\.compare-context-body \{[^}]*max-height: 50vh;[^}]*overflow: auto/);
});

test('comparison revision controls share a row and layout buttons share the folder toolbar', () => {
  const revisions = html.slice(html.indexOf('<div id="gitRevisionToolbar"'), html.indexOf('<div id="folderHistoryControls"'));
  for (const id of ['branchCompareKindSelect', 'remoteRef', 'branchCompareLeftRef', 'branchCompareRightRef', 'branchCompareSwap', 'branchCompareRefresh']) {
    assert.match(revisions, new RegExp(`id="${id}"`));
  }
  const scope = html.slice(html.indexOf('<div class="commander-toolbar compare-scope-toolbar"'), html.indexOf('<div class="commander-head commander-grid"'));
  for (const id of ['commanderBreadcrumbs', 'gitCompareTree', 'gitCompareFlatToggle', 'gitCompareDownload']) assert.match(scope, new RegExp(`id="${id}"`));
  const filters = html.slice(html.indexOf('<div id="gitCompareFlatControls"'), html.indexOf('<div class="compare-layout-choice"', html.indexOf('<div id="gitCompareFlatControls"')));
  assert.doesNotMatch(filters, /id="gitCompareTree"|id="gitCompareFlatToggle"/);
  assert.match(scope,/compare-inline-controls/,'filters share the existing scope toolbar, not another header row');
  assert.match(css, /\.git-revision-toolbar \{[^}]*flex-wrap: wrap/);
  assert.match(css, /\.git-revision-toolbar select \{[^}]*min-width: 0;[^}]*max-width: none/);
});

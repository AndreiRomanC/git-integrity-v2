const test = require('node:test');
const assert = require('node:assert/strict');
const { create, displayEncoding, formatBytes, nativeParentPath } = require('../frontend/local-drive.js');

test('Local Drive formats copy summaries without filesystem or backend work', () => {
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(1024), '1.0 KB');
  assert.equal(formatBytes(5 * 1024 * 1024), '5.0 MB');
});

test('Local Drive explains common Windows text encodings', () => {
  assert.equal(displayEncoding('utf-16le'), 'UTF-16 LE');
  assert.equal(displayEncoding('windows-1252'), 'Windows-1252 / ANSI');
});

test('Local Drive derives a useful left pane from macOS, Linux and Windows repository paths', () => {
  assert.equal(nativeParentPath('/Users/andrei/repos/project'), '/Users/andrei/repos');
  assert.equal(nativeParentPath('D:\\work\\project'), 'D:\\work');
  assert.equal(nativeParentPath('D:\\'), '');
  assert.equal(nativeParentPath('/'), '');
});

function classList() { return { toggle() {} }; }

function fakeNode() {
  const listeners = {};
  return {
    listeners, hidden: false, disabled: false, open: false, value: '', textContent: '', innerHTML: '', title: '',
    classList: { toggle() {}, add() {}, remove() {} },
    addEventListener(type, callback) { listeners[type] = callback; },
    insertAdjacentHTML(_position, html) { this.innerHTML += html; },
    showModal() { this.open = true; },
    close() { this.open = false; },
    focus() {},
  };
}

function fakeComparisonDocument() {
  const ids = [
    'localDriveCompareDialog', 'localDriveCompareSummary', 'localDriveCompareLeftPath', 'localDriveCompareRightPath',
    'localDriveDiffView', 'localDriveDiffRows', 'localDriveMergeEditors', 'localDriveCompareLeftContent',
    'localDriveCompareRightContent', 'showLocalDriveDiff', 'editLocalDriveDiff', 'copyAllLocalDriveRight',
    'copyAllLocalDriveLeft', 'saveLocalDriveCompareLeft', 'saveLocalDriveCompareRight', 'localDriveCompareState',
    'closeLocalDriveCompare', 'cancelLocalDriveCompare',
  ];
  const nodes = Object.fromEntries(ids.map(id => [id, fakeNode()]));
  return {
    nodes,
    querySelector(selector) { return nodes[selector.replace(/^#/, '')] || null; },
    addEventListener() {},
  };
}

function fakeLocalDriveDom() {
  const listeners = {};
  const shortcuts = Object.fromEntries(['compare', 'view', 'edit', 'copy', 'move', 'mkdir', 'trash'].map(action => [action, { setAttribute() {} }]));
  function pane(side) {
    const path = { textContent: '', title: '' };
    const refresh = { disabled: false };
    const list = { innerHTML: '' };
    return {
      dataset: { driveSide: side }, classList: classList(), entryButtons: [],
      querySelector(selector) {
        if (selector === '[data-drive-path]') return path;
        if (selector === '[data-drive-action="refresh"]') return refresh;
        if (selector === '[data-drive-list]') return list;
        return null;
      },
      querySelectorAll(selector) { return selector === '[data-drive-entry]' ? this.entryButtons : []; },
    };
  }
  const panes = { left: pane('left'), right: pane('right') };
  const root = {
    hidden: false,
    querySelector(selector) {
      const side = selector.match(/data-drive-side="(left|right)"/)?.[1];
      if (side) return panes[side];
      const shortcut = selector.match(/data-drive-shortcut="([^"]+)"/)?.[1];
      return shortcut ? shortcuts[shortcut] : null;
    },
    addEventListener(type, callback) { listeners[type] = callback; },
  };
  return { root, panes, listeners };
}

test('F5 copies the selected item from the active right pane into the left pane', async () => {
  const dom = fakeLocalDriveDom();
  const calls = [];
  const invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === 'list_local_directory' && args.path === '/repo') {
      return { path: '/repo', parent: '/work', entries: [{ name: 'a.txt', path: '/repo/a.txt', kind: 'file', size: 1, modified: 0 }] };
    }
    if (command === 'list_local_directory') return { path: args.path, parent: '/', entries: [] };
    if (command === 'copy_local_item') return { destination: '/work/a.txt', files: 1, directories: 0, bytes: 1 };
    throw new Error(`Unexpected command ${command}`);
  };
  const workspace = create({ root: dom.root, document: { querySelector: () => null, addEventListener() {} }, invoke, confirm: async () => true });
  await workspace.activate('/repo');

  const entryButton = { dataset: { driveEntry: '0' }, classList: classList() };
  dom.panes.right.entryButtons = [entryButton];
  dom.listeners.click({ target: { closest: selector => selector === '[data-drive-side]' ? dom.panes.right : selector === '[data-drive-entry]' ? entryButton : null } });
  const shortcut = { dataset: { driveShortcut: 'copy' } };
  dom.listeners.click({ target: { closest: selector => selector === '[data-drive-shortcut]' ? shortcut : null } });
  await new Promise(resolve => setImmediate(resolve));

  const copy = calls.find(call => call.command === 'copy_local_item');
  assert.deepEqual(copy.args, { sourcePath: '/repo/a.txt', destinationDirectory: '/work' });
});

test('F2 compares the selected file from each pane with two reads and no filesystem mutation', async () => {
  const dom = fakeLocalDriveDom();
  const document = fakeComparisonDocument();
  const calls = [];
  const invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === 'list_local_directory' && args.path === '/repo') {
      return { path: '/repo', parent: '/work', entries: [{ name: 'same.txt', path: '/repo/same.txt', kind: 'file', size: 4, modified: 0 }] };
    }
    if (command === 'list_local_directory' && args.path === '/work') {
      return { path: '/work', parent: '/', entries: [{ name: 'same.txt', path: '/work/same.txt', kind: 'file', size: 4, modified: 0 }] };
    }
    if (command === 'read_local_text_file') return { path: args.path, content: 'same\n', bytes: 5 };
    throw new Error(`Unexpected command ${command}`);
  };
  const workspace = create({ root: dom.root, document, invoke, diff: require('../frontend/local-diff.js') });
  await workspace.activate('/repo');

  for (const side of ['left', 'right']) {
    const entryButton = { dataset: { driveEntry: '0' }, classList: classList() };
    dom.panes[side].entryButtons = [entryButton];
    dom.listeners.click({ target: { closest: selector => selector === '[data-drive-side]' ? dom.panes[side] : selector === '[data-drive-entry]' ? entryButton : null } });
  }
  const shortcut = { dataset: { driveShortcut: 'compare' } };
  dom.listeners.click({ target: { closest: selector => selector === '[data-drive-shortcut]' ? shortcut : null } });
  await new Promise(resolve => setImmediate(resolve));

  assert.deepEqual(calls.filter(call => call.command === 'read_local_text_file').map(call => call.args.path), ['/work/same.txt', '/repo/same.txt']);
  assert.equal(calls.some(call => call.command === 'write_local_text_file'), false);
  assert.match(document.nodes.localDriveCompareSummary.textContent, /^Identical text/);
  assert.equal(document.nodes.localDriveCompareDialog.open, true);
});

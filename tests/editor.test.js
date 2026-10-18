const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
const source = app.slice(app.indexOf('let editorSession = null;'), app.indexOf('// Deliberately never touches state.repository/branches/commits/changes/'));
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function harness() {
  const nodes = new Map(), calls = [], reads = [], writes = [], blames = [], logs = [];
  const $ = id => {
    if (!nodes.has(id)) {
      let value = '';
      nodes.set(id, { checked: false, disabled: false, hidden: true, open: false, textContent: '', title: '', clientHeight: 200,
        get value() { return value; }, set value(text) { value = String(text).replace(/\r\n?/g, '\n'); },
        classList: { toggle() {} }, focus() {}, select() {},
        setSelectionRange(start, end) { this.selectionStart = start; this.selectionEnd = end; },
        showModal() { this.open = true; }, close() { this.open = false; } });
    }
    return nodes.get(id);
  };
  const sandbox = { $, document: { activeElement: null }, state: { repository: { path: '/repo' }, currentPath: 'src' }, repoOpenGeneration: 1,
    refs: { editorDialog: $('#editorDialog'), editorContent: $('#editorContent'), editorTitle: $('#editorTitle'), editorPath: $('#editorPath') },
    invoke(command, args) {
      calls.push({ command, ...args });
      const pending = deferred();
      if (command === 'read_text_file') reads.push(pending);
      else if (command === 'write_text_file') writes.push(pending);
      else if (command === 'file_blame') blames.push(pending);
      else { pending.resolve(); }
      return pending.promise;
    }, customConfirm: async () => false, diagnosticErrorCategory: () => 'unclassified', jsPerfLog: text => logs.push(text),
    status() {}, showOperationToast() {}, handleError: error => logs.push(String(error)), getComputedStyle: () => ({ lineHeight: '19' }), directoryCache: { clear() {} },
    loadRepository: async () => {}, refreshConflictsDialog: async () => {} };
  vm.createContext(sandbox); vm.runInContext(source, sandbox);
  const open = name => sandbox.openEditor({ name, relative_path: `src/${name}` });
  async function ready(name = 'a.c', content = 'original\n') { const pending = open(name); reads.at(-1).resolve({ content }); await pending; return pending; }
  return { $, sandbox, calls, reads, writes, blames, logs, open, ready };
}
test('editor cannot save before reading completes or after a failed read', async () => {
  const h = harness(), pending = h.open('a.c');
  assert.equal(h.$('#saveFile').disabled, true); assert.equal(h.$('#editorContent').disabled, true);
  await h.sandbox.saveEditor(); assert.equal(h.writes.length, 0);
  h.reads[0].reject('The file is not valid UTF-8 text'); await pending;
  assert.equal(h.$('#editorDialog').open, true); assert.equal(h.$('#editorError').hidden, false);
  assert.match(h.$('#editorError').textContent, /UTF-8/); assert.match(h.logs[0], /unsupported-encoding/);
  await h.sandbox.saveEditor(); assert.equal(h.writes.length, 0);
});
test('late reads and read failures cannot overwrite or close a newer editor', async () => {
  for (const fail of [false, true]) {
    const h = harness(), a = h.open('a.c'); await h.sandbox.requestEditorClose();
    const b = h.open('b.c'); h.reads[1].resolve({ content: 'B content' }); await b;
    if (fail) h.reads[0].reject('old error'); else h.reads[0].resolve({ content: 'A content' });
    await a;
    assert.equal(h.$('#editorDialog').open, true); assert.equal(h.$('#editorContent').value, 'B content');
    assert.equal(h.$('#editorPath').textContent, 'src/b.c'); assert.equal(h.$('#editorError').hidden, true);
  }
});
test('old blame results do not replace newer line metadata', async () => {
  const h = harness(); await h.ready('a.c'); await h.sandbox.requestEditorClose(); await h.ready('b.c');
  h.blames[1].resolve({ lines: [{ author: 'B', date: '2026-10-18' }] }); await Promise.resolve();
  h.blames[0].resolve({ lines: [{ author: 'A', date: 'old' }] }); await Promise.resolve();
  assert.match(h.$('#editorBlame').textContent, /^B/);
});
test('a late read after a repository switch does not populate the buffer', async () => {
  const h = harness(), pending = h.open('a.c');
  h.sandbox.state.repository.path = '/other'; h.sandbox.repoOpenGeneration++;
  h.reads[0].resolve({ content: 'old repo content' }); await pending;
  assert.equal(h.$('#editorContent').value, ''); await h.sandbox.saveEditor(); assert.equal(h.writes.length, 0);
});
test('repository changes invalidate reads and prevent saving an old session', async () => {
  const h = harness(); await h.ready(); h.$('#editorContent').value = 'edited';
  h.sandbox.state.repository.path = '/other'; await h.sandbox.saveEditor();
  assert.equal(h.writes.length, 0); assert.match(h.$('#editorSaveState').textContent, /Repository changed/);
  h.sandbox.state.repository.path = '/repo'; h.sandbox.repoOpenGeneration++;
  await h.sandbox.saveEditor(); assert.equal(h.writes.length, 0);
});
test('dirty close requires confirmation, and Save locks the buffer and refuses duplicate writes', async () => {
  const h = harness(); await h.ready(); h.$('#editorContent').value = 'changed';
  assert.equal(await h.sandbox.requestEditorClose(), false); assert.equal(h.$('#editorDialog').open, true);
  const saved = h.sandbox.saveEditor();
  assert.equal(h.$('#editorContent').disabled, true); assert.equal(h.$('#saveFile').disabled, true);
  assert.equal(await h.sandbox.requestEditorClose(), false); await h.sandbox.saveEditor();
  assert.equal(h.writes.length, 1);
  assert.deepEqual(h.calls.find(c => c.command === 'write_text_file'), { command: 'write_text_file', repositoryPath: '/repo', relativePath: 'src/a.c', content: 'changed' });
  h.writes[0].reject('Access denied'); await saved;
  assert.equal(h.$('#editorDialog').open, true); assert.equal(h.$('#editorContent').value, 'changed');
  assert.equal(h.$('#editorContent').disabled, false); assert.equal(h.$('#saveFile').disabled, false);
  h.sandbox.customConfirm = async () => true; assert.equal(await h.sandbox.requestEditorClose(), true);
  assert.equal(h.$('#editorDialog').open, false);
});
test('saving preserves uniform CRLF line endings and does not stage normal edits', async () => {
  const h = harness(); await h.ready('a.c', 'one\r\ntwo\r\n');
  assert.equal(h.$('#saveFile').disabled, true); h.$('#editorContent').value = 'one\ntwo edited\n';
  const saved = h.sandbox.saveEditor();
  assert.equal(h.calls.find(c => c.command === 'write_text_file').content, 'one\r\ntwo edited\r\n');
  h.writes[0].resolve(); await saved; assert.equal(h.$('#editorDialog').open, false);
  assert.equal(h.calls.some(c => /stage|resolve_conflict/.test(c.command)), false);
});
test('a failed post-save refresh remains visible without reporting a failed file write', async () => {
  const h = harness(); await h.ready(); h.$('#editorContent').value = 'changed';
  h.sandbox.loadRepository = async () => { throw new Error('Refresh failed'); };
  const saved = h.sandbox.saveEditor(); h.writes[0].resolve(); await saved;
  assert.equal(h.$('#editorDialog').open, false); assert.equal(h.writes.length, 1);
  assert.ok(h.logs.some(text => /Refresh failed/.test(text)));
});
test('conflict editing uses the same load guard and only resolves after a successful write', async () => {
  const h = harness(), options = { title: 'Resolve', repositoryPath: '/conflict', relativePath: 'modules/a.c', conflict: { target: { targetPath: 'modules' }, path: 'a.c' } };
  const opened = h.sandbox.openEditorDocument(options);
  await h.sandbox.saveEditor(); assert.equal(h.writes.length, 0);
  h.reads[0].resolve({ content: 'resolved' }); await opened;
  assert.equal(h.$('#saveFile').disabled, false);
  const saved = h.sandbox.saveEditor(); assert.equal(h.calls.some(c => c.command === 'resolve_conflict'), false);
  h.writes[0].resolve(); await saved;
  assert.deepEqual(h.calls.find(c => c.command === 'resolve_conflict'), { command: 'resolve_conflict', repositoryPath: '/conflict', targetPath: 'modules', relativePath: 'a.c', resolution: 'manual' });
  assert.equal(h.blames.length, 0);
});
test('dropped-file preview cannot overwrite a repository file with the same basename', async () => {
  const h = harness(); await h.sandbox.openEditorDocument({ title: 'View', relativePath: 'a.c', readOnly: true, previewContent: 'external' });
  await h.sandbox.saveEditor(); assert.equal(h.calls.length, 0); assert.equal(h.$('#editorContent').readOnly, true);
});
test('literal search navigates both ways, wraps, honors case, and refreshes after edits', async () => {
  const h = harness(); await h.ready('a.c', 'Needle\nneedle\nx[1]');
  h.$('#editorSearch').value = 'needle'; h.sandbox.updateEditorSearch();
  assert.equal(h.$('#editorSearchStatus').textContent, '2 matches');
  h.sandbox.moveEditorSearch(1); assert.equal(h.$('#editorContent').selectionStart, 0);
  h.sandbox.moveEditorSearch(1); assert.equal(h.$('#editorContent').selectionStart, 7);
  h.sandbox.moveEditorSearch(1); assert.equal(h.$('#editorContent').selectionStart, 0);
  h.sandbox.moveEditorSearch(-1); assert.equal(h.$('#editorContent').selectionStart, 7);
  h.$('#editorMatchCase').checked = true; h.sandbox.updateEditorSearch(); assert.equal(h.$('#editorSearchStatus').textContent, '1 matches');
  h.$('#editorSearch').value = '[1]'; h.sandbox.updateEditorSearch(); assert.equal(h.$('#editorSearchStatus').textContent, '1 matches');
  h.$('#editorContent').value = 'none'; h.sandbox.updateEditorSearch(); assert.equal(h.$('#editorSearchStatus').textContent, 'No matches');
  assert.equal(h.$('#editorSearchNext').disabled, true);
});
test('search bookkeeping is bounded and editor keyboard/close actions are wired', async () => {
  const h = harness(); await h.ready('a.c', 'x'.repeat(50000)); h.$('#editorSearch').value = 'x'; h.sandbox.updateEditorSearch();
  assert.equal(h.$('#editorSearchStatus').textContent, '10000+ matches');
  assert.match(app, /refs\.editorDialog\.addEventListener\('cancel', event => \{ event\.preventDefault\(\); requestEditorClose\(\)/);
  assert.match(app, /event\.key\.toLowerCase\(\) === 'f'/); assert.match(app, /event\.key\.toLowerCase\(\) === 's'/);
  assert.match(html, /id="saveFile" disabled/); assert.match(html, /id="editorContent"[^>]*disabled/);
});
test('case-insensitive literal search retains original Unicode offsets', async () => {
  const h = harness(); await h.ready('a.c', 'İ\nNeedle'); h.$('#editorSearch').value = 'needle';
  h.sandbox.updateEditorSearch(); h.sandbox.moveEditorSearch();
  assert.equal(h.$('#editorContent').selectionStart, 2);
  assert.equal(h.$('#editorContent').selectionEnd, 8);
});

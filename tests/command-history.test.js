const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { fitFloatingPanel } = require('../frontend/panel-resize.js');
const GitActivity = require('../frontend/git-activity.js');
const read = name => fs.readFileSync(path.join(__dirname, '../frontend', name), 'utf8');
const app = read('app.js');
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };

function harness(invoke = null) {
  const refs = {
    statusText: { title: '', textContent: 'Ready' }, statusDot: { classList: { contains: c => c === 'error' } },
    commandHistoryDialog: { open: false, style: {}, showModal() { this.open = true; } },
    commandHistoryList: { innerHTML: '', scrollTop: 30 },
    commandHistoryMaximize: { textContent: '', attrs: {}, setAttribute(k, v) { this.attrs[k] = v; } },
  };
  const c = { refs, invoke, fitFloatingPanel, GitActivity, innerWidth: 1440, innerHeight: 1000,
    esc: value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;'),
  };
  vm.createContext(c);
  vm.runInContext(app.slice(app.indexOf('function commandDurationText('), app.indexOf("refs.statusFooter.addEventListener('dblclick'")), c);
  return { c, refs, close() { refs.commandHistoryDialog.open = false; vm.runInContext('commandHistoryGeneration++', c); } };
}

test('full footer error is visible without any Git command, preserving lines and escaping markup', async () => {
  const { c, refs } = harness();
  refs.statusText.textContent = 'compact error';
  refs.statusText.title = 'Git index is locked\nNo cleanup was started.\n<unsafe> & "quoted path"';
  await c.openCommandHistoryDialog();
  assert.match(refs.commandHistoryList.innerHTML, /APP MESSAGE/);
  assert.match(refs.commandHistoryList.innerHTML, /Git index is locked\nNo cleanup was started/);
  assert.match(refs.commandHistoryList.innerHTML, /&lt;unsafe&gt; &amp; &quot;quoted path&quot;/);
  assert.doesNotMatch(refs.commandHistoryList.innerHTML, /compact error|<unsafe>/);
  assert.match(refs.commandHistoryList.innerHTML, /No Git commands recorded/);
  assert.equal(refs.commandHistoryList.scrollTop, 0);
});

test('opening requests only command history once and preserves the captured footer message', async () => {
  const response = deferred(), calls = [];
  const { c, refs } = harness(command => { calls.push(command); return response.promise; });
  refs.statusText.textContent = 'Original app failure';
  const opening = c.openCommandHistoryDialog();
  assert.match(refs.commandHistoryList.innerHTML, /Original app failure/);
  refs.statusText.textContent = 'Another action';
  await c.openCommandHistoryDialog();
  response.resolve([{ command: 'git checkout --force', stderr: 'Full error\nLast line', success: false, seconds_ago: 1 }]);
  await opening;
  assert.deepEqual(calls, ['recent_git_commands']);
  assert.match(refs.commandHistoryList.innerHTML, /Original app failure/);
  assert.match(refs.commandHistoryList.innerHTML, /Full error\nLast line/);
  assert.doesNotMatch(refs.commandHistoryList.innerHTML, /Another action/);
});

test('late responses from a closed console cannot overwrite a newly opened console', async () => {
  const old = deferred(), fresh = deferred(); let calls = 0;
  const h = harness(() => ++calls === 1 ? old.promise : fresh.promise);
  h.refs.statusText.textContent = 'Old failure';
  const first = h.c.openCommandHistoryDialog();
  h.close(); h.refs.statusText.textContent = 'New failure';
  const second = h.c.openCommandHistoryDialog();
  fresh.resolve([]); await second;
  old.resolve([{ command: 'stale command', stderr: 'stale error' }]); await first;
  assert.match(h.refs.commandHistoryList.innerHTML, /New failure/);
  assert.doesNotMatch(h.refs.commandHistoryList.innerHTML, /Old failure|stale/);
});

test('history load failure leaves the original app error readable', async () => {
  const { c, refs } = harness(async () => { throw new Error('<IPC failed>'); });
  refs.statusText.textContent = 'Git index is locked';
  await c.openCommandHistoryDialog();
  assert.match(refs.commandHistoryList.innerHTML, /Git index is locked/);
  assert.match(refs.commandHistoryList.innerHTML, /Could not load Git commands: Error: &lt;IPC failed&gt;/);
});

test('maximize, restore and drag resize use the viewport without requesting Git data', async () => {
  const calls = [];
  const { c, refs } = harness(async command => { calls.push(command); return []; });
  await c.openCommandHistoryDialog();
  assert.equal(refs.commandHistoryDialog.style.width, '1060px');
  c.resizeCommandHistoryDialog({ width: 1060, height: 720, left: 190, top: 140 }, 100, 80);
  const normal = { ...refs.commandHistoryDialog.style };
  assert.equal(normal.height, '800px');
  c.toggleCommandHistorySize();
  assert.equal(refs.commandHistoryDialog.style.width, '1408px');
  assert.equal(refs.commandHistoryDialog.style.height, '968px');
  assert.equal(refs.commandHistoryMaximize.attrs['aria-pressed'], 'true');
  c.toggleCommandHistorySize();
  assert.deepEqual(refs.commandHistoryDialog.style, normal);
  assert.equal(refs.commandHistoryMaximize.attrs['aria-pressed'], 'false');
  c.innerWidth = 600; c.innerHeight = 400; c.layoutCommandHistoryDialog();
  assert.equal(refs.commandHistoryDialog.style.width, '568px');
  assert.equal(refs.commandHistoryDialog.style.height, '368px');
  assert.deepEqual(calls, ['recent_git_commands']);
});

test('rendered command output keeps every captured line and the backend truncation notice', () => {
  const { c } = harness();
  const output = `${'long line\n'.repeat(500)}Final line\n[… output truncated after 64 KiB …]`;
  const html = c.commandHistoryRowHtml({ command: 'git test', stderr: output, seconds_ago: 1 });
  assert.ok(html.includes(output));
  assert.equal((html.match(/long line/g) || []).length, 500);
});

test('console scrolls as one growing area, with accessible resize and copy controls', () => {
  const css = read('styles.css'), html = read('index.html');
  assert.match(css, /\.command-history-output pre \{[^}]*max-height: none;[^}]*overflow: visible;[^}]*white-space: pre-wrap/);
  assert.match(css, /\.command-history-dialog \.conflicts-list \{[^}]*flex: 1;[^}]*min-height: 0;[^}]*max-height: none/);
  assert.match(html, /id="commandHistoryMaximize" aria-pressed="false"/);
  assert.match(html, /id="commandHistoryResize"[^>]*aria-label="Resize command console"[^>]*tabindex="0"/);
  assert.match(html, /type="button" id="copyCommandHistory"/);
  assert.match(app, /copyText\(refs\.commandHistoryList\.innerText, 'Console output copied'\)/);
});

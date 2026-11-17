const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const api = require('../frontend/settings.js');
const flush = () => new Promise(resolve => setImmediate(resolve));
const local = (path = '/repo') => ({ repository_path: path, github_servers: [{ host: 'github.com', remote: 'origin' }, { host: 'github.enterprise.test', remote: 'company' }], name: { value: 'Commit Author', scope: 'local' }, email: { value: 'commit@test', scope: 'global' } });
function harness() {
  class Node {
    constructor() { this.handlers = {}; this.dataset = {}; this.children = []; this.textContent = ''; this.value = ''; this.disabled = false; this.attributes = {}; }
    addEventListener(type, fn) { (this.handlers[type] ||= []).push(fn); }
    async fire(type, event = {}) { for (const fn of this.handlers[type] || []) await fn(event); }
    setAttribute(key, value) { this.attributes[key] = value; }
    append(child) { this.children.push(child); }
    replaceChildren() { this.children = []; }
    focus() {}
    showModal() { this.open = true; }
    close() { this.open = false; this.fire('close'); }
  }
  const nodes = new Map(), el = name => { if (!nodes.has(name)) nodes.set(name, new Node()); return nodes.get(name); };
  const dialog = new Node(); dialog.open = false;
  const tabs = ['connections', 'diagnostics'].map(name => { const node = new Node(); node.dataset.settingsTab = name; return node; });
  dialog.querySelector = selector => el(selector.match(/"(.*?)"/)[1]);
  dialog.querySelectorAll = selector => selector === '[data-settings-tab]' ? tabs : [el('close')];
  const document = { querySelector: selector => selector === '#settingsDialog' ? dialog : el('open'), createElement: () => new Node() };
  const context = { window: {}, document, console, Date };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../frontend/settings.js'), 'utf8'), context);
  const calls = [], replies = {}, remembered = [], copied = []; let repository = '/repo';
  const settings = context.window.GitDrillDownSettings.create({
    invoke: async (command, args) => { calls.push({ command, args }); return replies[command] ? replies[command](args) : command === 'settings_local' ? local(args.repositoryPath) : {}; },
    getRepositoryPath: () => repository,
    copy: value => copied.push(value), rememberedPolarion: () => ({ id: 'saved-user' }), rememberPolarion: user => remembered.push(user),
  });
  return { settings, el, dialog, tabs, calls, replies, remembered, copied, repo: value => { repository = value; } };
}
test('creating Settings performs no work; opening reads only local config', async () => {
  const h = harness(); assert.equal(h.calls.length, 0);
  await h.settings.open();
  assert.deepEqual(h.calls.map(c => c.command), ['settings_local']);
  assert.equal(h.el('github-user').textContent, 'Account not verified');
  assert.equal(h.el('polarion-state').textContent, 'Not checked');
  assert.match(h.el('polarion-remembered').textContent, /not a connection check/);
  assert.match(h.el('identity-name').textContent, /Commit Author · local/);
});
test('no repository still allows Polarion and global identity without GitHub checks', async () => {
  const h = harness(); h.repo(null); h.replies.settings_local = () => ({ github_servers: [], name: null, email: null });
  await h.settings.open();
  assert.equal(h.el('github-check').disabled, true); assert.equal(h.el('polarion-check').disabled, false);
  assert.match(h.el('github-detail').textContent, /Open a repository/);
});
test('explicit GitHub checks use captured repository and selected host; failures clear old success', async () => {
  const h = harness(); await h.settings.open();
  h.replies.settings_check_github = () => ({ state: 'connected', login: 'account-user', name: 'Someone else', method: 'git_credentials' });
  await h.el('github-check').fire('click');
  assert.equal(h.calls.at(-1).args.interactive, false);
  assert.match(h.el('github-user').textContent, /account-user/);
  assert.match(h.el('identity-name').textContent, /Commit Author/);
  assert.match(h.el('github-checked').textContent, /Last check/);
  h.replies.settings_check_github = () => ({ state: 'sign_in_required', detail: 'Sign in' });
  await h.el('github-check').fire('click');
  assert.equal(h.el('github-user').textContent, 'Account not verified');
  assert.equal(h.el('github-state').textContent, 'Sign-in required');
});
test('results are isolated across host and repository; switching host never checks automatically', async () => {
  const h = harness(); await h.settings.open();
  h.replies.settings_check_github = () => ({ state: 'connected', login: 'first' }); await h.el('github-check').fire('click');
  h.el('github-hosts').value = 'github.enterprise.test'; await h.el('github-hosts').fire('change');
  assert.equal(h.el('github-state').textContent, 'Not checked');
  assert.equal(h.calls.filter(c => c.command === 'settings_check_github').length, 1);
  h.dialog.close(); h.repo('/different'); await h.settings.open();
  assert.equal(h.el('github-state').textContent, 'Not checked');
});
test('pending check is deduplicated across close/reopen and does not overwrite a different host', async () => {
  const h = harness(); await h.settings.open(); let resolve;
  h.replies.settings_check_github = () => new Promise(done => { resolve = done; });
  const pending = h.el('github-check').fire('click');
  h.dialog.close(); await h.settings.open();
  assert.equal(h.el('github-check').disabled, true);
  await h.el('github-check').fire('click');
  h.el('github-hosts').value = 'github.enterprise.test'; await h.el('github-hosts').fire('change');
  resolve({ state: 'connected', login: 'first' }); await pending;
  assert.equal(h.el('github-state').textContent, 'Not checked');
  assert.equal(h.calls.filter(c => c.command === 'settings_check_github').length, 1);
});
test('late local response cannot overwrite a reopened Settings context', async () => {
  const h = harness(); let resolve;
  h.replies.settings_local = () => new Promise(done => { resolve = done; });
  const first = h.settings.open(); h.dialog.close();
  h.repo('/new'); h.replies.settings_local = args => local(args.repositoryPath); await h.settings.open();
  resolve({ github_servers: [], name: { value: 'OLD', scope: 'local' } }); await first;
  assert.equal(h.el('context').textContent, '/new'); assert.match(h.el('identity-name').textContent, /Commit Author/);
});
test('Sign in is explicit; opening the Polarion window does not claim verified access', async () => {
  const h = harness(); await h.settings.open();
  h.replies.settings_check_github = () => ({ state: 'sign_in_required' });
  await h.el('github-connect').fire('click'); assert.equal(h.calls.at(-1).args.interactive, true);
  await h.el('polarion-connect').fire('click'); assert.equal(h.calls.at(-1).command, 'polarion_connect');
  assert.equal(h.el('polarion-state').textContent, 'Not checked');
  h.replies.polarion_read = () => ({ code: 'ok', user_id: 'sg123', user_name: 'Portal user' });
  await h.el('polarion-check').fire('click'); assert.equal(h.calls.at(-1).args.kind, 'user');
  assert.match(h.el('polarion-user').textContent, /sg123/); assert.equal(h.remembered.length, 1);
});
test('Polarion missing identity, forbidden, offline and auth-required remain distinct', () => {
  assert.equal(api.polarionResult({ code: 'ok' }).state, 'failed');
  assert.equal(api.polarionResult({ code: 'forbidden' }).state, 'failed');
  assert.equal(api.polarionResult({ code: 'auth_required' }).state, 'sign_in_required');
  assert.match(api.polarionResult({ code: 'network_error' }).detail, /network/);
});
test('diagnostics load lazily once; copy excludes account and repository data', async () => {
  const h = harness(); await h.settings.open();
  h.replies.settings_diagnostics = () => ({ version: '1', build: 'abc', platform: 'macos', git_version: 'git 2', git_path: '<home>/git', log_path: '/private/log', repository: '/private/repo', token: 'SECRET' });
  await h.tabs[1].fire('click'); await flush(); await h.tabs[1].fire('click'); await flush();
  assert.equal(h.calls.filter(c => c.command === 'settings_diagnostics').length, 1);
  await h.el('copy').fire('click'); assert.match(h.copied[0], /Build: abc/);
  assert.doesNotMatch(h.copied[0], /SECRET|private|Commit Author/);
});
test('gear and dialog are accessible; auth fields are not added to local storage', () => {
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  assert.match(html, /sidebar-footer.*id="openSettings".*aria-label="Settings"/);
  assert.match(html, /settingsDialog.*aria-labelledby="settingsTitle"/);
  const source = fs.readFileSync(path.join(__dirname, '../frontend/settings.js'), 'utf8');
  assert.doesNotMatch(source, /setItem|setInterval/);
});

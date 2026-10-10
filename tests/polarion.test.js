const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { polarionCommitMessage, polarionSearchExplanation } = require('../frontend/polarion.js');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src-tauri/src/repository/polarion-search.js'), 'utf8');
async function portal({ status = 200, origin = 'https://polarion.vitesco.io', contentType = 'application/json', type = 'basic', data = { data: [] }, error = null, token = 'SECRET', kind = 'tasks' } = {}) {
  let request = null, reply = null;
  const location = { origin, set href(value) { reply = JSON.parse(new URL(value).searchParams.get('data')); } };
  const sandbox = { URL, AbortController, setTimeout, clearTimeout, location, window: { getRestApiToken: () => token }, fetch: async (url, options) => {
    request = { url, options };
    if (error) throw error;
    return { status, type, ok: status >= 200 && status < 300, headers: { get: () => contentType }, json: async () => data };
  } };
  await vm.runInNewContext(source.replace('__DDT_ARGUMENTS__', JSON.stringify({ url: 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems', project: 'P', kind, request: '1' })), sandbox);
  return { request, reply };
}
test('selected task reuses existing message syntax and rejects incompatible IDs', () => {
  assert.equal(polarionCommitMessage({ id: 'P-123', title: 'Title\ncontinued' }), 'P:P-123 - Title continued');
  assert.throws(() => polarionCommitMessage({ id: 'bad/id', title: 'x' }));
});
test('portal keeps token inside same-origin request and disables redirects', async () => {
  const { request, reply } = await portal({ data: { data: [{ type: 'workitems', id: 'P/P-123', attributes: { type: 'task', title: 'Test' } }], links: { next: '/page2' } } });
  assert.equal(request.options.headers['X-Polarion-REST-Token'], 'SECRET');
  assert.equal(request.options.credentials, 'same-origin');
  assert.equal(request.options.redirect, 'manual');
  assert.equal(reply.items[0].id, 'P-123');
  assert.equal(reply.more, true);
  assert.equal(JSON.stringify(reply).includes('SECRET'), false);
});
test('portal excludes other types and projects and supports enum task type', async () => {
  const item = (id, type) => ({ id, type: 'workitems', attributes: { type, title: '<unsafe>' } });
  const { reply } = await portal({ data: { data: [item('P/P-1', { id: 'task' }), item('P/P-2', 'package'), item('Other/Other-1', 'task')] } });
  assert.equal(reply.received, 3); assert.equal(reply.items.length, 1); assert.equal(reply.items[0].id, 'P-1');
});
test('portal classifies authentication, permissions, API, redirects and non-JSON without raw bodies', async () => {
  for (const [options, code] of [[{ status: 401 }, 'auth_required'], [{ status: 403 }, 'forbidden'], [{ status: 404 }, 'api_unavailable'], [{ type: 'opaqueredirect' }, 'auth_required'], [{ contentType: 'text/html' }, 'not_json'], [{ error: new Error('secret raw response') }, 'network_error'], [{ error: { name: 'AbortError' } }, 'timeout']]) {
    const { reply } = await portal(options); assert.equal(reply.code, code); assert.ok(!JSON.stringify(reply).includes('secret raw'));
  }
});
test('no request executes while browser is on the identity-provider origin', async () => {
  const { request, reply } = await portal({ origin: 'https://sso.example.test' });
  assert.equal(request, null); assert.equal(reply.code, 'wrong_origin');
});
test('projects are bounded and response structure is checked', async () => {
  const { reply } = await portal({ kind: 'projects', data: { data: Array.from({ length: 30 }, (_, i) => ({ id: `P${i}`, type: 'projects', attributes: { name: `Project ${i}` } })) } });
  assert.equal(reply.items.length, 25);
  assert.equal((await portal({ data: { unexpected: true } })).reply.code, 'invalid_response');
  assert.equal((await portal({ data: { data: [{ type: 'unrecognized' }] } })).reply.code, 'invalid_response');
});
test('UI explains API failure and missing portal token without claiming successful sign-in', () => {
  assert.match(polarionSearchExplanation({ code: 'forbidden', http: 403, session_token: false }), /No portal REST session token/);
  assert.match(polarionSearchExplanation({ code: 'auth_required' }), /Sign in/);
});
test('remote portal cannot invoke desktop Git commands; integration does not create commits', () => {
  const lib = fs.readFileSync(require('node:path').join(__dirname, '../src-tauri/src/lib.rs'), 'utf8');
  assert.match(lib, /invoke_handler\(local_commands_only\(/);
  assert.match(lib, /webview_ref\(\)\.label\(\) != "main"/);
  const controller = fs.readFileSync(require('node:path').join(__dirname, '../frontend/polarion.js'), 'utf8');
  assert.doesNotMatch(controller, /innerHTML|invoke\(['"](?:create_commit|commit_files|commit_staged)/);
});
test('task selection uses the same saved-message storage and preserves manual entries', () => {
  const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
  const block = app.slice(app.indexOf('const COMMIT_MESSAGE_HISTORY_KEY'), app.indexOf('// Opening the drawer no longer auto-stages'));
  const storage = new Map([['git-integrity-default-commit-message-history', JSON.stringify(['My manual checkpoint'])]]);
  let picker, inputEvents = 0;
  const sandbox = { state: {}, refs: { defaultCommitMessage: { value: '', dispatchEvent: () => inputEvents++ } },
    localStorage: { getItem: key => storage.get(key), setItem: (key, value) => storage.set(key, value) },
    document: { querySelectorAll: () => [] }, $: () => ({}), esc: String, Event: class {}, invoke: () => { throw Error('No backend call expected'); },
    createPolarionTaskPicker: options => { picker = options; return {}; }, showOperationToast: () => {},
  };
  vm.runInNewContext(block, sandbox);
  picker.addMessage('P:P-123 - Task title');
  picker.addMessage('P:P-123 - Task title');
  assert.deepEqual(JSON.parse(storage.get('git-integrity-default-commit-message-history')), ['P:P-123 - Task title', 'My manual checkpoint']);
  assert.equal(sandbox.refs.defaultCommitMessage.value, 'P:P-123 - Task title');
  assert.equal(inputEvents, 2);
});

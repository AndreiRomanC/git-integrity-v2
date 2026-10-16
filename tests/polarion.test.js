const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { polarionCommitMessage, polarionSearchExplanation } = require('../frontend/polarion.js');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src-tauri/src/repository/polarion-search.js'), 'utf8');
async function portal({ status = 200, origin = 'https://polarion.vitesco.io', contentType = 'application/json', type = 'basic', data = { data: [] }, error = null, token = 'SECRET', kind = 'tasks', fallbackUrl = null, statuses = null, pages = null, query = '', page = 1, portalWindow = {}, resourceId = '', project = 'P' } = {}) {
  let request = null, reply = null;
  const requests = [];
  const location = { origin, set href(value) { reply = JSON.parse(new URL(value).searchParams.get('data')); } };
  portalWindow.getRestApiToken = () => token;
  const sandbox = { URL, AbortController, setTimeout, clearTimeout, location, window: portalWindow, fetch: async (url, options) => {
    request = { url, options };
    requests.push(request);
    const code = statuses ? statuses[requests.length - 1] : status;
    if (error) throw error;
    return { status: code, type, ok: code >= 200 && code < 300, headers: { get: () => contentType }, json: async () => pages ? pages[requests.length - 1] : data };
  } };
  await vm.runInNewContext(source.replace('__DDT_ARGUMENTS__', JSON.stringify({ url: kind === 'projects' ? 'https://polarion.vitesco.io/polarion/rest/v1/projects?fields[projects]=id,name,trackerPrefix&page[size]=100' : 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems', fallbackUrl, project, resourceId, kind, query, page, request: '1' })), sandbox);
  return { request, reply, requests };
}
test('fragment rejection falls back once to legacy prefix search, not on authentication or empty results', async () => {
  const fallbackUrl = 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems?query=id:AQ4*';
  const result = await portal({ fallbackUrl, statuses: [400, 200] });
  assert.equal(result.requests.length, 2);
  assert.equal(result.requests[1].url, fallbackUrl);
  assert.equal(result.reply.prefix_fallback, true);
  assert.equal(result.reply.code, 'ok');
  assert.equal(JSON.stringify(result.reply).includes('SECRET'), false);
  for (const status of [200, 401, 403, 500]) {
    const check = await portal({ fallbackUrl, status });
    assert.equal(check.requests.length, 1);
    assert.equal(check.reply.prefix_fallback, false);
  }
});
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
  assert.equal(reply.more, true);
  assert.equal((await portal({ data: { unexpected: true } })).reply.code, 'invalid_response');
  assert.equal((await portal({ data: { data: [{ type: 'unrecognized' }] } })).reply.code, 'invalid_response');
});
const projectItem = (id, name = id) => ({ type: 'projects', id, attributes: { name } });
test('project search matches inside names and IDs case-insensitively across all catalog pages', async () => {
  const { reply, requests } = await portal({ kind: 'projects', query: 'aQ4', pages: [
    { data: [projectItem('Other')], links: { next: 'https://unsafe.example/never-follow' } },
    { data: [projectItem('VWAQ4'), projectItem('P2', 'Test aq4 platform')] },
  ] });
  assert.deepEqual(reply.items.map(item => item.id), ['VWAQ4', 'P2']);
  assert.equal(reply.catalog_pages, 2);
  assert.equal(new URL(requests[1].url).searchParams.get('page[number]'), '2');
  assert.ok(requests.every(({ url }) => new URL(url).origin === 'https://polarion.vitesco.io'));
});
test('complete project catalog is reused for new queries and display pages, never partial or another session', async () => {
  const portalWindow = {};
  const data = { data: Array.from({ length: 32 }, (_, i) => projectItem(`VWAQ4_${i}`)) };
  const first = await portal({ kind: 'projects', query: 'aq4', portalWindow, data });
  assert.equal(first.reply.items.length, 25);
  const second = await portal({ kind: 'projects', query: 'aq4', page: 2, portalWindow });
  assert.equal(second.requests.length, 0); assert.equal(second.reply.items.length, 7);
  assert.equal(second.reply.catalog_cached, true);
  const third = await portal({ kind: 'projects', query: 'aq4_31', portalWindow });
  assert.equal(third.reply.items.length, 1);
  const changedSession = await portal({ kind: 'projects', portalWindow, token: 'OTHER', status: 401 });
  assert.equal(changedSession.requests.length, 1); assert.equal(changedSession.reply.code, 'auth_required');
  assert.equal(portalWindow.__ddtProjectCatalog, undefined);
});
test('failed or looping catalog pagination never returns incomplete matching projects', async () => {
  const data = { data: [projectItem('VWAQ4')], links: { next: 'page2' } };
  for (const options of [{ statuses: [200, 403] }, {}]) {
    const portalWindow = {};
    const result = await portal({ kind: 'projects', query: 'aq4', data, portalWindow, ...options });
    assert.notEqual(result.reply.code, 'ok'); assert.equal(result.reply.items, undefined);
    assert.equal(portalWindow.__ddtProjectCatalog, undefined);
  }
});
test('project catalog expiry and missing session token force a fresh read', async () => {
  const data = { data: [projectItem('VWAQ4')] };
  const portalWindow = {};
  await portal({ kind: 'projects', query: 'aq4', portalWindow, data });
  portalWindow.__ddtProjectCatalog.time = Date.now() - 121000;
  const expired = await portal({ kind: 'projects', query: 'aq4', portalWindow, data });
  assert.equal(expired.reply.catalog_cached, false); assert.equal(expired.requests.length, 1);
  const missing = await portal({ kind: 'projects', query: 'aq4', portalWindow, data, token: null });
  assert.equal(missing.reply.catalog_cached, false); assert.equal(missing.requests.length, 1);
  assert.equal(portalWindow.__ddtProjectCatalog, undefined);
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

test('AQ4 also matches only the tracker prefix; All projects keeps explicit display pagination', async () => {
  const data = { data: [{ ...projectItem('Platform', 'Different name'), attributes: { name: 'Different name', trackerPrefix: 'VWAQ4' } }, projectItem('Other')] };
  const fragment = await portal({ kind: 'projects', query: 'aQ4', data });
  assert.deepEqual(fragment.reply.items.map(i => i.id), ['Platform']);
  assert.equal(fragment.reply.matched, 1); assert.equal(fragment.reply.received, 2);
  assert.equal((await portal({ kind: 'projects', query: '', data })).reply.items.length, 2);
});
test('identity comes from /user response, never Git credentials or token contents', async () => {
  const { reply } = await portal({ kind: 'user', data: { data: { type: 'users', id: 'sg123', attributes: { name: 'A User', email: 'private@example.test' } } } });
  assert.equal(reply.user_id, 'sg123'); assert.equal(reply.user_name, 'A User');
  assert.ok(!JSON.stringify(reply).includes('private@example.test')); assert.ok(!JSON.stringify(reply).includes('SECRET'));
});
test('task snapshot returns bounded plain text, assignees and dates but no raw HTML or arbitrary fields', async () => {
  const { reply, request } = await portal({ kind: 'workitem', resourceId: 'P/P-1', data: { data: { type: 'workitems', id: 'P/P-1', revision: '42',
    attributes: { title: 'Title', status: 'inprogress', type: 'task', description: { type: 'text/html', value: '<script>evil()</script><p>Details &amp; dates</p>' }, plannedStart: '2026-10-16', dueDate: '2026-11-01', password: 'MUST_NOT_LEAVE' },
    relationships: { assignee: { data: [{ type: 'users', id: 'sg123' }] } } } } });
  assert.equal(reply.snapshot.description, 'Details & dates\n');
  assert.deepEqual(reply.snapshot.assignees, ['sg123']);
  assert.equal(reply.snapshot.fields.plannedStart, '2026-10-16'); assert.equal(reply.snapshot.fields.revision, '42');
  assert.equal(request.options.method, 'GET');
  assert.doesNotMatch(JSON.stringify(reply), /MUST_NOT_LEAVE|evil|<script/);
});
test('document search covers every space and page, matching path and title fragments', async () => {
  const doc = (id, title) => ({ id, type: 'documents', attributes: { title, type: 'structureDocument' } });
  const { reply } = await portal({ kind: 'documents', query: 'structure', pages: [
    { data: [doc('P/Other/Doc', 'Unrelated')], links: { next: 'https://evil.test' } },
    { data: [doc('P/30 Development/Structure Information', 'Architecture')] },
  ] });
  assert.equal(reply.items.length, 1); assert.equal(reply.items[0].id, 'P/30 Development/Structure Information');
  assert.equal(reply.catalog_pages, 2); assert.equal(reply.matched, 1);
});
test('LiveDoc elements preserve real parts and types; inaccessible details stay visible', async () => {
  const part = (id, workitem) => ({ type: 'document_parts', id: 'P/Space/Doc/' + id, attributes: { type: 'workitem', level: 2 }, relationships: { workItem: { data: { type: 'workitems', id: workitem } } } });
  const { reply } = await portal({ kind: 'elements', resourceId: 'P/Space/Doc', data: {
    data: [part('one', 'P/P-1'), part('two', 'Other/O-2')],
    included: [{ type: 'workitems', id: 'P/P-1', attributes: { title: 'Architecture', type: 'architecturalElement', status: 'approved' } }],
    links: { next: 'page2' },
  } });
  assert.equal(reply.code, 'ok', JSON.stringify(reply));
  assert.equal(reply.elements.length, 2); assert.equal(reply.elements[0].item_type, 'architecturalElement');
  assert.equal(reply.elements[1].id, 'Other/O-2'); assert.match(reply.elements[1].fields.details, /not returned/);
  assert.equal(reply.incomplete, 1); assert.equal(reply.more, true); assert.match(reply.page_key, /\/one/);
});
test('StructDoc work-item links use actual relationship targets, not guessed type IDs', async () => {
  const { reply } = await portal({ kind: 'links', resourceId: 'P/P-1', data: { data: [
    { type: 'linkedworkitems', id: 'P/P-1/contains/P/P-2', attributes: { role: 'contains' }, relationships: { sourceWorkItem: { data: { id: 'P/P-1' } }, workItem: { data: { id: 'P/P-2', type: 'workitems' } } } },
  ], included: [{ id: 'P/P-2', type: 'workitems', attributes: { type: 'solutionElement', title: 'Solution' } }] } });
  assert.equal(reply.code, 'ok', JSON.stringify(reply));
  assert.equal(reply.elements[0].item_type, 'solutionElement'); assert.equal(reply.elements[0].fields.linkRole, 'contains');
});
test('wrong resources, partial metadata errors and oversized element pages fail closed', async () => {
  assert.equal((await portal({ kind: 'workitem', resourceId: 'P/P-1', data: { data: { type: 'workitems', id: 'Other/O-1' } } })).reply.code, 'invalid_response');
  assert.equal((await portal({ kind: 'projects', data: { data: [projectItem('P')], meta: { errors: [{ detail: 'DO_NOT_LOG' }] } } })).reply.code, 'invalid_response');
  assert.equal((await portal({ kind: 'elements', resourceId: 'P/S/D', data: { data: Array(26).fill({}) } })).reply.code, 'invalid_response');
});
test('oversized UTF-8 detail replies report a bridge limit instead of timing out', async () => {
  const { reply } = await portal({ kind: 'workitem', resourceId: 'P/P-1', data: { data: { id: 'P/P-1', type: 'workitems',
    attributes: { description: { value: '界'.repeat(12000) }, title: 'Large detail' },
    relationships: { assignee: { data: Array.from({ length: 100 }, () => ({ id: '界'.repeat(100) })) } },
  } } });
  assert.equal(reply.code, 'response_limit'); assert.equal(reply.snapshot, undefined);
});

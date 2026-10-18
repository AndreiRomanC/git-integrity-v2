const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { polarionCommitMessage, polarionSearchExplanation } = require('../frontend/polarion.js');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src-tauri/src/repository/polarion-search.js'), 'utf8');
async function portal({ status = 200, origin = 'https://polarion.vitesco.io', contentType = 'application/json', type = 'basic', data = { data: [] }, error = null, token = 'SECRET', kind = 'tasks', fallbackUrl = null, readFallback = false, documentGlobalUrl = null, documentModulesUrl = null, documentModuleQueries = null, statuses = null, pages = null, query = '', page = 1, portalWindow = {}, resourceId = '', project = 'P', probes = [], url = null, responseHeaders = {}, body = null, clock = null } = {}) {
  let request = null, reply = null;
  const requests = [];
  const location = { origin, set href(value) { reply = JSON.parse(new URL(value).searchParams.get('data')); } };
  portalWindow.getRestApiToken = () => token;
  const sandbox = { URL, AbortController, TextDecoder, setTimeout, clearTimeout, location, window: portalWindow, fetch: async (url, options) => {
    request = { url, options };
    requests.push(request);
    const code = statuses ? statuses[requests.length - 1] : status;
    if (error) throw error;
    return { status: code, type, ok: code >= 200 && code < 300, body, headers: { get: key => key === 'content-type' ? contentType : responseHeaders[key] || '' }, json: async () => pages ? pages[requests.length - 1] : data };
  } };
  if (clock) sandbox.Date = class extends Date { static now() { return clock(); } };
  await vm.runInNewContext(source.replace('__DDT_ARGUMENTS__', JSON.stringify({ url: url || (kind === 'projects' ? 'https://polarion.vitesco.io/polarion/rest/v1/projects?fields[projects]=id,name,trackerPrefix&page[size]=100' : kind === 'documents' ? 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/documents?page[size]=100' : 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems'), fallbackUrl, readFallback, documentGlobalUrl, documentModulesUrl, documentModuleQueries, project, resourceId, kind, query, page, probes, request: '1' })), sandbox);
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
const documentRoutes = {
  documentGlobalUrl: 'https://polarion.vitesco.io/polarion/rest/v1/all/documents?query=project.id:%22P%22&page[size]=100',
  documentModulesUrl: 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems?query=module.id:(*errm*)&fields[workitems]=id,module&page[size]=100',
};
const documentItem = (id, title = id) => ({ id, type: 'documents', attributes: { title } });
const moduleItem = (id, doc) => ({ id: 'P/' + id, type: 'workitems', relationships: { module: { data: { id: doc, type: 'documents' } } } });
const moduleQueries = [
  ['module_id_contains', 'module.id:(*errm*)'],
  ['location_tokens_prefix', 'location.tokenized:errm*'],
  ['module_prefix', 'module:errm*'],
].map(([variant, query]) => ({ variant, url: 'https://polarion.vitesco.io/polarion/rest/v1/projects/P/workitems?fields[workitems]=id,module&page[size]=100&query=' + encodeURIComponent(query) }));
test('rejected document contains query tries targeted token lookup, paginates and reuses its limited cache', async () => {
  const portalWindow = {}, documentModuleQueries = moduleQueries;
  const options = { kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries, portalWindow };
  const result = await portal({ ...options, statuses: [404,404,400,200,200], pages: [
    {}, {}, { errors: [{ detail: 'Leading wildcard PRIVATE_QUERY is not allowed', source: { parameter: 'query' } }] },
    { data: [moduleItem('P-1', 'P/S/ERRM_1'), { id: 'P/P-2', type: 'workitems', relationships: { module: { data: null } } }], links: { next: 'https://unsafe.example' } },
    { data: [moduleItem('P-3', 'P/S/ERRM_2'), moduleItem('P-4', 'P/S/Not_a_match')] },
  ] });
  assert.equal(result.reply.code, 'ok'); assert.equal(result.reply.document_search_limited, true);
  assert.equal(result.reply.document_query_variant, 'location_tokens_prefix'); assert.equal(result.reply.catalog_pages, 2);
  assert.deepEqual(result.reply.items.map(i => i.id), ['P/S/ERRM_1','P/S/ERRM_2']);
  assert.equal(new URL(result.requests[4].url).searchParams.get('page[number]'), '2');
  assert.equal(new URL(result.requests[4].url).searchParams.get('query'), 'location.tokenized:errm*');
  assert.ok(result.requests.every(r => r.options.method === 'GET' && new URL(r.url).origin === 'https://polarion.vitesco.io'));
  const cached = await portal(options);
  assert.equal(cached.requests.length, 0); assert.equal(cached.reply.catalog_cached, true);
  assert.equal(cached.reply.document_query_variant, 'location_tokens_prefix');
  assert.equal((await portal({ ...options, query: 'other', statuses: [404,404,400,200,200], data: { data: [] } })).requests.length, 5);
  assert.doesNotMatch(JSON.stringify(result.reply), /SECRET|PRIVATE_QUERY|unsafe\.example|location\.tokenized:errm/);
});
test('accepted empty document queries continue to alternatives and remain distinct from all rejected queries', async () => {
  const options = { kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries };
  const found = await portal({ ...options, statuses: [404,404,200,200], pages: [
    {}, {}, { data: [] }, { data: [moduleItem('P-1', 'P/S/ERRM')] },
  ] });
  assert.equal(found.reply.items.length, 1); assert.equal(found.reply.document_query_variant, 'location_tokens_prefix');
  const empty = await portal({ ...options, statuses: [404,404,200,400,400], data: { data: [] } });
  assert.equal(empty.reply.code, 'ok'); assert.equal(empty.reply.items.length, 0);
  assert.equal(empty.reply.document_query_variant, 'module_id_contains'); assert.equal(empty.reply.document_search_limited, true);
  const rejected = await portal({ ...options, statuses: [404,404,400,400,400], data: { data: [] } });
  assert.equal(rejected.reply.code, 'query_rejected'); assert.equal(rejected.reply.items, undefined);
  const unchanged = await portal({ ...options, statuses: [404,404,200], pages: [{},{},{ data: [moduleItem('P-1', 'P/S/ERRM')] }] });
  assert.equal(unchanged.requests.length, 3); assert.equal(unchanged.reply.document_query_variant, 'module_id_contains');
});
test('document syntax alternatives never hide fields, authentication, transport or later-page errors as empty success', async () => {
  for (const status of [401,403,404,405,500,503]) {
    const portalWindow = {};
    const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries,
      statuses: [404,404,status], portalWindow });
    assert.notEqual(result.reply.code, 'ok'); assert.equal(result.requests.length, 3);
    assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
  }
  const fields = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries,
    statuses: [404,404,400], pages: [{},{},{ errors: [{ detail: 'Unknown field PRIVATE_FIELD', source: { parameter: 'fields[workitems]' } }] }] });
  assert.equal(fields.requests.length, 3); assert.equal(fields.reply.code, 'query_rejected');
  const portalWindow = {};
  const later = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries, portalWindow,
    statuses: [404,404,400,200,400], pages: [{},{},{},{ data: [moduleItem('P-1', 'P/S/ERRM')], links: { next: 'next' } },{}] });
  assert.equal(later.reply.code, 'query_rejected'); assert.equal(later.requests.length, 5);
  assert.equal(later.reply.items, undefined); assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
  for (const relationship of [undefined, { data: null, meta: { errors: [{}] } }]) {
    const invalid = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries,
      statuses: [404,404,400,200], pages: [{},{},{},{ data: [{ id: 'P/P-1', type: 'workitems', relationships: { module: relationship } }] }] });
    assert.equal(invalid.reply.code, 'invalid_response'); assert.equal(invalid.reply.items, undefined);
    assert.equal(invalid.reply.diagnostics.at(-1).reason, 'relationship_missing');
    assert.equal(invalid.reply.diagnostics.at(-1).code, 'invalid_response');
    assert.equal(invalid.requests.length, 4);
  }
});
test('document alternatives share one HTTP budget and never publish a partial catalog at its limit', async () => {
  const pages = [{}, {}, ...Array.from({ length: 209 }, (_, i) => ({ data: [moduleItem('P-' + i, 'P/S/Other')], links: i === 199 ? {} : { next: 'next' } }))];
  const portalWindow = {};
  const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, documentModuleQueries: moduleQueries, portalWindow,
    pages, statuses: pages.map((_, i) => i < 2 ? 404 : 200) });
  assert.equal(result.requests.length, 210); assert.equal(result.reply.http_requests, 210);
  assert.equal(result.reply.code, 'catalog_limit'); assert.equal(result.reply.items, undefined);
  assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
});
test('missing project catalog tries the global document catalog, keeps project scope and does not fetch server next URLs', async () => {
  const { reply, requests } = await portal({ kind: 'documents', query: 'eRrM', ...documentRoutes, statuses: [404, 200, 200], pages: [
    {}, { data: [documentItem('Other/S/ERRM')], links: { next: 'https://unsafe.example' } },
    { data: [documentItem('P/Structure Information/P_ERRM_StructDoc'), documentItem('P/S/Unrelated')] },
  ] });
  assert.equal(reply.code, 'ok'); assert.equal(reply.catalog_source, 'all_documents');
  assert.deepEqual(reply.items.map(i => i.id), ['P/Structure Information/P_ERRM_StructDoc']);
  assert.equal(new URL(requests[2].url).pathname, '/polarion/rest/v1/all/documents');
  assert.equal(new URL(requests[2].url).searchParams.get('query'), 'project.id:"P"');
  assert.equal(new URL(requests[2].url).searchParams.get('page[number]'), '2');
  assert.ok(requests.every(r => r.options.method === 'GET' && new URL(r.url).origin === 'https://polarion.vitesco.io'));
  assert.equal(reply.diagnostics[1].route, 'all_documents'); assert.equal(reply.diagnostics[1].step, 'document_global');
});
test('legacy document search derives actual module paths, deduplicates documents across work-item pages and excludes other projects', async () => {
  const doc = 'P/Structure Information/P_ERRM_StructDoc';
  const { reply, requests } = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, statuses: [404, 405, 200, 200, 200], pages: [
    {}, {}, { data: [moduleItem('P-1', doc)], links: { next: 'next' } },
    { data: [moduleItem('P-2', doc)], links: { next: 'next' } },
    { data: [moduleItem('P-3', 'P/S/ERRM_2'), moduleItem('P-4', 'Other/S/ERRM')] },
  ] });
  assert.equal(reply.code, 'ok'); assert.equal(reply.catalog_source, 'work_item_modules'); assert.equal(reply.items.length, 2);
  assert.ok(reply.items.some(i => i.id === doc)); assert.equal(reply.catalog_pages, 3);
  assert.equal(reply.diagnostics.at(-1).step, 'document_modules');
  assert.ok(requests.slice(2).every(r => new URL(r.url).searchParams.get('query') === 'module.id:(*errm*)'));
  assert.doesNotMatch(JSON.stringify(reply), /SECRET|X-Polarion|module\.id:\(|\*errm\*|Other\/S/);
});
test('a successful empty document catalog never triggers another search strategy', async () => {
  const { reply, requests } = await portal({ kind: 'documents', query: 'errm', ...documentRoutes });
  assert.equal(reply.code, 'ok'); assert.equal(reply.items.length, 0); assert.equal(requests.length, 1);
  assert.equal(reply.catalog_source, 'project_documents');
});
test('document compatibility never retries authentication, permissions, transient failures or a failed later page', async () => {
  for (const status of [400, 401, 403, 500, 503]) {
    const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, status });
    assert.notEqual(result.reply.code, 'ok'); assert.equal(result.requests.length, 1);
  }
  const portalWindow = {};
  const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, portalWindow, statuses: [200, 404], pages: [
    { data: [documentItem('P/S/ERRM')], links: { next: 'next' } }, {},
  ] });
  assert.equal(result.requests.length, 2); assert.notEqual(result.reply.code, 'ok'); assert.equal(result.reply.items, undefined);
  assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
});
test('global document search permission failure stops instead of disguising it as an empty result', async () => {
  const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, statuses: [404, 403], pages: [{}, {}] });
  assert.equal(result.reply.code, 'forbidden'); assert.equal(result.requests.length, 2); assert.equal(result.reply.items, undefined);
});
test('legacy search caches only a completed query in the same session, while full catalogs permit new local filters', async () => {
  const portalWindow = {}, data = { data: Array.from({ length: 30 }, (_, i) => moduleItem('P-' + i, 'P/S/ERRM_' + i)) };
  const first = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, portalWindow, statuses: [404, 404, 200], pages: [{}, {}, data] });
  assert.equal(first.reply.items.length, 25); assert.equal(first.reply.more, true);
  const next = await portal({ kind: 'documents', query: 'ERRM', page: 2, ...documentRoutes, portalWindow });
  assert.equal(next.requests.length, 0); assert.equal(next.reply.items.length, 5); assert.equal(next.reply.catalog_source, 'work_item_modules');
  const changed = await portal({ kind: 'documents', query: 'other', ...documentRoutes, portalWindow, status: 403 });
  assert.equal(changed.requests.length, 1); assert.equal(changed.reply.code, 'forbidden');
  assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
  await portal({ kind: 'documents', query: 'errm', portalWindow, data: { data: [documentItem('P/S/ERRM'), documentItem('P/S/OTHER')] } });
  const other = await portal({ kind: 'documents', query: 'other', portalWindow });
  assert.equal(other.requests.length, 0); assert.equal(other.reply.items[0].id, 'P/S/OTHER');
  const session = await portal({ kind: 'documents', query: 'errm', portalWindow, token: 'OTHER', status: 401 });
  assert.equal(session.requests.length, 1); assert.equal(session.reply.code, 'auth_required');
});
test('missing module relationships, looping work-item pages and partial errors never produce a false successful empty search', async () => {
  for (const data of [{ data: [{ id: 'P/P-1', type: 'workitems' }] },
    { data: [moduleItem('P-1', 'P/S/ERRM')], links: { next: 'same' } },
    { data: [{ ...moduleItem('P-1', 'P/S/ERRM'), meta: { errors: [{}] } }] }]) {
    const portalWindow = {};
    const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, portalWindow,
      statuses: [404, 404, 200, 200], pages: [{}, {}, data, data] });
    assert.equal(result.reply.code, 'invalid_response'); assert.equal(result.reply.items, undefined);
    assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
  }
});
test('legacy search limits and failed module reads preserve no partial cache or results', async () => {
  const portalWindow = {};
  const pages = [{}, {}, ...Array.from({ length: 200 }, (_, i) => ({
    data: [moduleItem('P-' + i, 'P/S/ERRM_' + i)], links: { next: 'next' },
  }))];
  const result = await portal({ kind: 'documents', query: 'errm', ...documentRoutes, portalWindow,
    statuses: [404, 404, ...Array(200).fill(200)], pages });
  assert.equal(result.reply.code, 'catalog_limit'); assert.equal(result.reply.items, undefined);
  assert.equal(result.requests.length, 202); assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
  for (const status of [400, 401, 403, 404, 500]) {
    const failed = await portal({ kind: 'documents', query: 'errm', ...documentRoutes,
      statuses: [404, 404, status], pages: [{}, {}, {}] });
    assert.notEqual(failed.reply.code, 'ok'); assert.equal(failed.reply.items, undefined); assert.equal(failed.requests.length, 3);
  }
});
test('document cache expiry and a missing portal token force fresh scoped reads', async () => {
  const portalWindow = {}, data = { data: [documentItem('P/S/ERRM')] };
  await portal({ kind: 'documents', query: 'errm', portalWindow, data });
  portalWindow.__ddtDocumentSearchV2_P.time = Date.now() - 121000;
  const expired = await portal({ kind: 'documents', query: 'errm', portalWindow, data });
  assert.equal(expired.requests.length, 1); assert.equal(expired.reply.catalog_cached, false);
  const missing = await portal({ kind: 'documents', query: 'errm', portalWindow, data, token: null });
  assert.equal(missing.requests.length, 1); assert.equal(missing.reply.catalog_cached, false);
  assert.equal(portalWindow.__ddtDocumentSearchV2_P, undefined);
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

const api = 'https://polarion.vitesco.io/polarion/rest/v1';
test('per-request diagnostics distinguish original query from fallback without leaking queries or bodies', async () => {
  const { reply } = await portal({ statuses: [400, 400], pages: [
    { errors: [{ title: 'SECRET', detail: 'Leading wildcard is not allowed: PRIVATE_SEARCH', source: { parameter: 'query' } }] },
    { errors: [{ title: 'SECRET', detail: 'Lucene parse exception PRIVATE_SEARCH', source: { parameter: 'query' } }] },
  ], url: api + '/projects/P/workitems?query=id:*%20AND%20(id:(*PRIVATE_SEARCH*))', fallbackUrl: api + '/projects/P/workitems?query=id:(PRIVATE_SEARCH*)', responseHeaders: { allow: 'GET, POST, SECRET, Bearer SECRET' } });
  assert.equal(reply.http_requests, 2);
  assert.equal(reply.diagnostics[0].route, 'workitems'); assert.equal(reply.diagnostics[0].reason, 'leading_wildcard');
  assert.equal(reply.diagnostics[0].parameter, 'query'); assert.equal(reply.diagnostics[0].id_star, true); assert.equal(reply.diagnostics[0].leading_wildcard, true);
  assert.equal(reply.diagnostics[1].step, 'prefix_fallback'); assert.equal(reply.diagnostics[1].id_star, false); assert.equal(reply.diagnostics[1].leading_wildcard, false);
  assert.equal(reply.diagnostics[1].reason, 'query_syntax');
  assert.equal(reply.diagnostics[1].error_class, 'query_parser');
  assert.deepEqual(reply.diagnostics[0].allow, ['GET', 'POST']);
  assert.doesNotMatch(JSON.stringify(reply), /SECRET|PRIVATE_SEARCH|Bearer|X-Polarion/);
});
test('HTTP error diagnostics classify fields, permissions and missing resources without declaring every 404 a permission failure', async () => {
  for (const [status, detail, parameter, reason] of [
    [400, 'Unknown field privateFieldName', 'fields[documents]', 'unknown_field'],
    [403, 'You do not have permission to view privateDoc', '', 'permission_denied'],
    [404, 'Private document does not exist', '', 'not_found'],
    [400, 'Unsupported parameter privateValue', 'secretParameter', 'unsupported_parameter'],
  ]) {
    const { reply } = await portal({ status, data: { errors: [{ detail, source: { parameter } }] } });
    assert.equal(reply.diagnostics[0].reason, reason);
    assert.doesNotMatch(JSON.stringify(reply), /privateFieldName|privateDoc|secretParameter|privateValue/);
  }
});
test('error bodies are bounded and non-JSON pages are never parsed or exported', async () => {
  let cancelled = false;
  const body = { getReader: () => ({ read: async () => ({ value: new Uint8Array(8193), done: false }), cancel: async () => { cancelled = true; } }) };
  const bounded = await portal({ status: 400, body });
  assert.equal(bounded.reply.diagnostics[0].reason, 'error_body_limit'); assert.equal(cancelled, true);
  let sent = false;
  const small = { getReader: () => ({ read: async () => sent ? { done: true } : (sent = true, { value: new TextEncoder().encode(JSON.stringify({ errors: [{ detail: 'Leading wildcard PRIVATE_BODY not allowed' }] })), done: false }), cancel: async () => {} }) };
  const streamed = await portal({ status: 400, body: small });
  assert.equal(streamed.reply.diagnostics[0].reason, 'leading_wildcard'); assert.doesNotMatch(JSON.stringify(streamed.reply), /PRIVATE_BODY/);
  const html = await portal({ status: 404, contentType: 'text/html', data: { secret: 'NEVER_EXPORTED' } });
  assert.equal(html.reply.diagnostics[0].media, 'html'); assert.equal(html.reply.diagnostics[0].reason, '');
  assert.doesNotMatch(JSON.stringify(html.reply), /NEVER_EXPORTED/);
});
test('diagnostic bytes do not displace a large snapshot that still fits the bridge', async () => {
  const data = { data: { id: 'P/P-1', type: 'workitems',
    attributes: { title: 'Base', description: { value: '界'.repeat(11600) } },
    relationships: { assignee: { data: Array.from({ length: 100 }, () => ({ id: '界'.repeat(80) })) } },
  } };
  const { reply } = await portal({ kind: 'workitem', resourceId: 'P/P-1', data });
  assert.equal(reply.code, 'ok'); assert.equal(reply.snapshot.description.length, 11600);
  assert.ok(Buffer.byteLength(JSON.stringify(reply)) <= 60000);
  const legacy = { session_token: true, prefix_fallback: false, code: 'ok', http: 200, snapshot: reply.snapshot };
  const spare = 59995 - Buffer.byteLength(JSON.stringify(legacy));
  assert.ok(spare >= 0 && spare < 896, 'controlled snapshot can reach the old UTF-8 boundary');
  const descriptionPadding = Math.min(spare, 400);
  data.data.attributes.description.value += 'x'.repeat(descriptionPadding);
  data.data.attributes.title += 'x'.repeat(spare - descriptionPadding);
  const boundary = await portal({ kind: 'workitem', resourceId: 'P/P-1', data });
  assert.equal(boundary.reply.code, 'ok'); assert.equal(Buffer.byteLength(JSON.stringify(boundary.reply)), 59995);
});
test('catalog diagnostics stay bounded and retain the latest request without changing pagination', async () => {
  const pages = Array.from({ length: 50 }, (_, i) => ({ data: [projectItem('P' + i)], links: i === 49 ? {} : { next: 'next' } }));
  const { reply } = await portal({ kind: 'projects', pages });
  assert.equal(reply.http_requests, 50); assert.equal(reply.diagnostics.length, 40); assert.equal(reply.diagnostics_omitted, 10);
  assert.equal(reply.diagnostics.at(-1).page, 50); assert.equal(reply.received, 50); assert.equal(reply.catalog_pages, 50);
});
test('explicit diagnostics read only, export safe version fields and never claim user data as metadata', async () => {
  const { reply, requests } = await portal({ kind: 'diagnostics', probes: [
    { step: 'metadata', url: api + '/metadata' }, { step: 'current_user', url: api + '/user' },
  ], pages: [
    { data: { type: 'metadata', attributes: { version: '3.25.12', build: '20250613-1404-master-e594c717', node: 'PRIVATE_NODE', apiProperties: { maxPageSize: 200, password: 'SECRET' } } } },
    { data: { type: 'users', id: 'PRIVATE_USER', attributes: { email: 'PRIVATE_EMAIL' } } },
  ] });
  assert.equal(reply.code, 'ok'); assert.equal(requests.length, 2); assert.ok(requests.every(r => r.options.method === 'GET'));
  assert.deepEqual(reply.server_metadata, { version: '3.25.12', build: '20250613-1404-master-e594c717', max_page_size: 200 });
  assert.equal(reply.diagnostics[1].resource_type, 'users'); assert.equal(reply.snapshot, undefined); assert.equal(reply.items, undefined);
  assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_|SECRET/);
  const invalid = await portal({ kind: 'diagnostics', probes: [{ step: 'metadata', url: api + '/metadata' }], data: { data: { type: 'metadata', attributes: { version: 'SECRET', build: 'SECRET' } } } });
  assert.equal(invalid.reply.server_metadata.version, ''); assert.equal(invalid.reply.server_metadata.build, '');
});
test('exact LiveDoc reads bypass a missing catalog and gate the parts probe on the actual resource', async () => {
  const probes = [
    { step: 'documents_catalog', url: api + '/projects/P/documents?page[size]=1' },
    { step: 'document_exact', url: api + '/projects/P/spaces/S/documents/D' },
    { step: 'page_exact', url: api + '/projects/P/spaces/S/pages/D' },
    { step: 'document_parts', url: api + '/projects/P/spaces/S/documents/D/parts?page[size]=1', requires: 'document_exact' },
  ];
  const found = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', probes, statuses: [404,200,404,200], pages: [
    {}, { data: { id: 'P/S/D', type: 'documents', attributes: { title: 'PRIVATE_TITLE' } } }, {}, { data: [], links: { next: 'next' } },
  ] });
  assert.deepEqual(found.reply.diagnostics.map(d => d.code), ['api_unavailable','ok','api_unavailable','ok']);
  assert.equal(found.reply.diagnostics[3].more, true); assert.equal(found.requests.length, 4);
  assert.doesNotMatch(JSON.stringify(found.reply), /PRIVATE_TITLE|P\/S\/D/);
  for (const data of [{ data: { id: 'P/S/Other', type: 'documents' } }, { data: { id: 'P/S/D', type: 'pages' } }, { data: [] }]) {
    const invalid = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', probes: probes.slice(1).filter(p => p.step !== 'page_exact'), data });
    assert.equal(invalid.requests.length, 1); assert.equal(invalid.reply.diagnostics[0].code, 'invalid_response');
    assert.equal(invalid.reply.diagnostics[1].reason, 'prerequisite_missing');
  }
});
test('diagnostic alternatives remain independent and budget exhaustion reports checks not run', async () => {
  const probes = ['query_current', 'query_prefix', 'query_without_id_star'].map(step => ({ step, url: api + '/projects/P/workitems?page[size]=1' }));
  const { reply } = await portal({ kind: 'diagnostics', probes, statuses: [400,400,200], data: { data: [] } });
  assert.deepEqual(reply.diagnostics.map(d => d.code), ['query_rejected','query_rejected','ok']);
  let now = 0;
  const budget = await portal({ kind: 'diagnostics', probes: [...probes, ...probes], clock: () => (now += 6000), data: { data: [] } });
  assert.ok(budget.requests.length < 6); assert.ok(budget.reply.diagnostics.some(d => d.code === 'not_run' && d.reason === 'budget'));
  const wrongOrigin = await portal({ kind: 'diagnostics', probes, origin: 'https://identity.example' });
  assert.equal(wrongOrigin.requests.length, 0); assert.equal(wrongOrigin.reply.code, 'wrong_origin');
});
test('optional LiveDoc metadata rejection falls back once, without pretending a prefix search or inventing fields', async () => {
  const fallbackUrl = api + '/projects/P/spaces/S/documents/D?fields[documents]=title';
  const data = { data: { id: 'P/S/D', type: 'documents', attributes: { title: 'Exact LiveDoc' } } };
  const found = await portal({ kind: 'document', resourceId: 'P/S/D', readFallback: true, fallbackUrl, statuses: [400,200], data });
  assert.equal(found.requests.length, 2); assert.equal(found.reply.metadata_fallback, true); assert.equal(found.reply.prefix_fallback, false);
  assert.equal(found.reply.diagnostics[1].step, 'metadata_fallback');
  assert.equal(found.reply.snapshot.title, 'Exact LiveDoc'); assert.equal(found.reply.snapshot.status, '');
  assert.match(found.reply.snapshot.fields.compatibilityNote, /title only/);
  for (const status of [200,401,403,404,405,500]) {
    const check = await portal({ kind: 'document', resourceId: 'P/S/D', readFallback: true, fallbackUrl, status, data });
    assert.equal(check.requests.length, 1); assert.equal(check.reply.metadata_fallback, undefined);
  }
});
test('compatible element reads preserve actual work-item relationships and never conceal missing details', async () => {
  const fallbackUrl = api + '/projects/P/spaces/S/documents/D/parts?fields[document_parts]=id,type,workItem';
  const { reply } = await portal({ kind: 'elements', resourceId: 'P/S/D', readFallback: true, fallbackUrl, statuses: [400,200], data: {
    data: [{ id: 'P/S/D/part', type: 'document_parts', attributes: { type: 'workitem' }, relationships: { workItem: { data: { id: 'P/P-1', type: 'workitems' } } } }],
  } });
  assert.equal(reply.code, 'ok'); assert.equal(reply.elements.length, 1); assert.equal(reply.elements[0].id, 'P/P-1');
  assert.equal(reply.incomplete, 1); assert.match(reply.elements[0].fields.details, /not returned/);
  assert.equal(reply.metadata_fallback, true); assert.equal(reply.prefix_fallback, false);
});
test('response facts describe technical fields and types without exporting values, IDs or unknown fields', async () => {
  const result = await portal({ kind: 'diagnostics', project: 'P', probes: [{ step: 'workitem_modules', url: api + '/projects/P/workitems?query=module.id:*&fields[workitems]=id,type,status,module,SECRET_FIELD' }],
    data: { data: [{ ...moduleItem('PRIVATE_ID', 'P/PRIVATE_SPACE/PRIVATE_DOC'), attributes: { type: 'architecturalElement', title: 'PRIVATE_TITLE', description: 'PRIVATE_BODY', status: 'PRIVATE_STATUS', password: 'PRIVATE_PASSWORD', SECRET_FIELD: 'SECRET_VALUE' } }] } });
  const facts = result.reply.diagnostics[0];
  assert.equal(facts.code, 'ok'); assert.equal(facts.query_kind, 'module_name'); assert.equal(facts.shape, 'list');
  assert.deepEqual(facts.item_types, ['architecturalElement']); assert.equal(facts.module_links, 1);
  assert.deepEqual(facts.requested_fields, ['workitems.id','workitems.module','workitems.status','workitems.type']);
  assert.deepEqual(facts.returned_fields, ['workitems.description','workitems.status','workitems.title','workitems.type']);
  assert.deepEqual(facts.relationship_fields, ['workitems.module']);
  assert.equal(facts.id_star, false); assert.equal(facts.leading_wildcard, true);
  assert.deepEqual(facts.query_fields, ['module.id']);
  assert.doesNotMatch(JSON.stringify(result.reply), /PRIVATE_|SECRET|X-Polarion/);
  const malformed = await portal({ kind: 'diagnostics', probes: [{ step: 'workitem_modules', url: api + '/projects/P/workitems' }], data: { data: [{ type: 'workitems', id: 'P/P-1' }] } });
  assert.equal(malformed.reply.diagnostics[0].code, 'invalid_response'); assert.equal(malformed.reply.diagnostics[0].reason, 'relationship_missing');
});
test('explicit known-document controls verify query matches against real resources without exporting query values', async () => {
  const probes = [
    { step: 'document_exact', url: api + '/projects/P/spaces/S/documents/PRIVATE_DOC' },
    { step: 'document_parts_expanded', url: api + '/projects/P/spaces/S/documents/PRIVATE_DOC/parts?page[size]=10', requires: 'document_exact' },
    { step: 'workitem_sample', url: api + '/projects/P/workitems?fields[workitems]=id,module', requires: 'document_parts_expanded' },
    { step: 'known_item_query', variant: 'known_item_exact', url: api + '/projects/P/workitems?query=&page[size]=1', requires: 'workitem_sample', known: 'item' },
    { step: 'known_document_query', variant: 'known_module_exact', url: api + '/projects/P/workitems?query=module:%22P/S/PRIVATE_DOC%22&page[size]=1', requires: 'document_exact', known: 'document' },
    { step: 'known_document_query', variant: 'known_module_id_exact', url: api + '/projects/P/workitems?query=module.id:%22P/S/PRIVATE_DOC%22&page[size]=1', requires: 'document_exact', known: 'document' },
  ];
  const pages = [
    { data: { id: 'P/S/PRIVATE_DOC', type: 'documents' } },
    { data: [{ id: 'P/S/PRIVATE_DOC/part', type: 'document_parts', attributes: { type: 'workitem' }, relationships: { workItem: { data: { id: 'P/PRIVATE_ITEM', type: 'workitems' } } } }] },
    { data: moduleItem('PRIVATE_ITEM', 'P/S/PRIVATE_DOC') },
    { data: [moduleItem('PRIVATE_ITEM', 'P/S/PRIVATE_DOC')] },
    { data: [moduleItem('PRIVATE_ITEM', 'P/S/PRIVATE_DOC')] }, { data: [] },
  ];
  const { reply, requests } = await portal({ kind: 'diagnostics', resourceId: 'P/S/PRIVATE_DOC', probes, pages });
  assert.equal(requests.length, 6);
  assert.equal(new URL(requests[3].url).searchParams.get('query'), 'id:"PRIVATE_ITEM"');
  assert.deepEqual(reply.diagnostics.slice(3).map(d => d.known_match), [true,true,false]);
  assert.equal(reply.diagnostics[4].leading_wildcard, false); assert.equal(reply.diagnostics[4].query_variant, 'known_module_exact');
  assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_|SECRET|module:\"|P\/S\//);
  const missing = await portal({ kind: 'diagnostics', resourceId: 'P/S/PRIVATE_DOC', probes, statuses: [404], data: { data: [] } });
  assert.equal(missing.requests.length, 1);
  assert.ok(missing.reply.diagnostics.slice(1).every(d => d.code === 'not_run'));
  assert.equal(missing.reply.diagnostics[3].query_variant, 'known_item_exact');
});
test('explicit document sample follows only one actual same-project work item, without saving or exposing its data', async () => {
  const probes = [
    { step: 'document_exact', url: api + '/projects/P/spaces/S/documents/D' },
    { step: 'document_parts_expanded', url: api + '/projects/P/spaces/S/documents/D/parts?page[size]=10&include=workItem&fields[workitems]=id,type,status', requires: 'document_exact' },
    { step: 'workitem_sample', url: api + '/projects/P/workitems?fields[workitems]=id,title,type,status,module', requires: 'document_parts_expanded' },
  ];
  const part = (id, target) => ({ id: 'P/S/D/' + id, type: 'document_parts', attributes: { type: 'workitem' }, relationships: { workItem: { data: { id: target, type: 'workitems' } } } });
  const pages = [{ data: { id: 'P/S/D', type: 'documents', attributes: { title: 'PRIVATE_DOCUMENT', type: 'Structure_doc' } } },
    { data: [part('one', 'Other/PRIVATE_ID'), part('two', 'P/PRIVATE_ID'), part('three', 'P/NOT_FOLLOWED')],
      included: [{ id: 'P/PRIVATE_ID', type: 'workitems', attributes: { type: 'designElement', status: 'PRIVATE_STATUS' } }], links: { next: 'https://unsafe.example/never-follow' } },
    { data: { id: 'P/PRIVATE_ID', type: 'workitems', attributes: { type: 'designElement', title: 'PRIVATE_TITLE', status: 'PRIVATE_STATUS' } } },
  ];
  const { reply, requests } = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', project: 'P', probes, pages });
  assert.equal(requests.length, 3); assert.equal(new URL(requests[2].url).pathname, '/polarion/rest/v1/projects/P/workitems/PRIVATE_ID');
  assert.ok(requests.every(r => r.options.method === 'GET'));
  assert.deepEqual(reply.diagnostics.map(d => d.code), ['ok','ok','ok']);
  assert.equal(reply.diagnostics[1].workitem_links, 3); assert.equal(reply.diagnostics[1].included_workitems, 1);
  assert.deepEqual(reply.diagnostics[1].item_types, ['designElement','workitem']);
  assert.equal(reply.diagnostics[2].shape, 'single'); assert.equal(reply.snapshot, undefined); assert.equal(reply.items, undefined);
  assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_|NOT_FOLLOWED|unsafe\.example|SECRET|P\/S\/D/);
  const denied = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', project: 'P', probes, pages: [pages[0], pages[1], {}], statuses: [200,200,403] });
  assert.equal(denied.reply.diagnostics[2].code, 'forbidden');
  const noSample = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', project: 'P', probes, pages: [pages[0], { data: [part('one', 'Other/PRIVATE_ID')] }] });
  assert.equal(noSample.requests.length, 2); assert.equal(noSample.reply.diagnostics[2].reason, 'sample_missing');
  const incompatible = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', project: 'P', probes, pages: [pages[0], {}], statuses: [200,400] });
  assert.equal(incompatible.requests.length, 2); assert.equal(incompatible.reply.diagnostics[2].reason, 'prerequisite_missing');
  const ignoresSize = await portal({ kind: 'diagnostics', resourceId: 'P/S/D', project: 'P', probes, pages: [pages[0], { data: Array.from({ length: 11 }, (_, i) => part('part' + i, 'P/PRIVATE_ID')) }] });
  assert.equal(ignoresSize.requests.length, 2); assert.equal(ignoresSize.reply.diagnostics[1].code, 'invalid_response');
  assert.equal(ignoresSize.reply.diagnostics[2].reason, 'prerequisite_missing');
});
test('HTTP 405 is method-not-allowed even when the server error text mentions permissions', async () => {
  const { reply } = await portal({ status: 405, data: { errors: [{ detail: 'User is not allowed to use this method PRIVATE_SECRET' }] } });
  assert.equal(reply.diagnostics[0].reason, 'method_not_allowed'); assert.equal(reply.http, 405);
  assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_SECRET/);
});
test('technical response facts stay bounded on normal large reads without adding HTTP calls', async () => {
  const { reply, requests } = await portal({ kind: 'projects', data: { data: Array.from({ length: 100 }, (_, i) => projectItem('P' + i, 'PRIVATE_NAME')) } });
  assert.equal(requests.length, 1); assert.equal(reply.diagnostics[0].sample_rows, 10);
  assert.ok(reply.diagnostics[0].returned_fields.length <= 32); assert.ok(reply.diagnostics[0].item_types.length <= 12);
  assert.doesNotMatch(JSON.stringify(reply.diagnostics), /PRIVATE_NAME|SECRET/);
});

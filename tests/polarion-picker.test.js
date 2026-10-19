const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function harness() {
  class Element {
    constructor() { this.handlers = {}; this.dataset = {}; this.children = []; this.value = ''; this.checked = false; this.textContent = ''; }
    addEventListener(event, fn) { this.handlers[event] = fn; }
    setAttribute() {}
    append(...children) {
      for (const child of children) {
        if (child.parent) child.parent.children = child.parent.children.filter(c => c !== child);
        child.parent = this; this.children.push(child);
      }
    }
    replaceChildren() { for (const child of this.children) child.parent = null; this.children = []; }
    focus() {}
    showModal() { this.open = true; }
    close() { this.open = false; this.handlers.close?.(); }
  }
  const nodes = new Map();
  const el = id => { if (!nodes.has(id)) nodes.set(id, new Element()); return nodes.get(id); };
  const dialog = new Element(); dialog.querySelector = id => el(id.replace('#polarion', ''));
  const calls = [], overrides = {}, memory = new Map(), messages = [], logs = [], changedSources = [];
  let workspace = {};
  let search = async () => ({ code: 'ok', items: [], more: false });
  const sandbox = { localStorage: { getItem: k => memory.get(k), setItem: (k,v) => memory.set(k,v) }, document: { querySelector: () => dialog, createElement: () => new Element() } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../frontend/polarion.js'), 'utf8'), sandbox);
  const picker = sandbox.createPolarionTaskPicker({ invoke: async (name, args) => {
    calls.push({ name, args });
    if (overrides[name]) return overrides[name](args);
    if (name === 'polarion_project_context') return { repository_path: '/repo', project: null };
    if (name === 'polarion_search') return search();
    if (name === 'polarion_workspace') {
      if (args.action === 'primary') workspace.primary_task = args.snapshot;
      if (args.action === 'document') {
        if (workspace.document?.id !== args.snapshot?.id || workspace.document?.kind !== args.snapshot?.kind) {
          workspace.elements = []; workspace.elements_complete = false; workspace.elements_saved_at = ''; workspace.elements_revision = '';
        }
        workspace.document = args.snapshot;
      }
      if (args.action === 'elements') { workspace.elements = args.elements; workspace.elements_complete = true; workspace.elements_revision = args.snapshot.fields?.revision || ''; workspace.elements_saved_at = args.snapshot.fetched_at; }
      if (args.action === 'element') workspace.elements = workspace.elements.map(i => i.id === args.snapshot.id ? args.snapshot : i);
      return structuredClone(workspace);
    }
  }, addMessage: message => messages.push(message), log: message => logs.push(message), onDocumentChange: path => changedSources.push(path) });
  return { el, dialog, calls, picker, overrides, memory, messages, logs, changedSources, setWorkspace: value => { workspace = value; }, search: fn => { search = fn; } };
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('Polarion flow distinguishes unknown, sign-in required and verified access; tasks require a project', async () => {
  const h = harness();
  await h.picker.open('/repo');
  assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.equal(h.el('TaskSearchButton').disabled, true);
  assert.equal(h.calls.length, 2, 'opening reads local associations only; no search or authentication');
  h.el('ProjectQuery').value = 'aq4';
  h.search(async () => ({ code: 'auth_required' }));
  h.el('ProjectSearchButton').handlers.click(); await tick();
  assert.equal(h.el('Connection').dataset.state, 'required');
  assert.match(h.el('Connection').textContent, /Not connected to Polarion/);
  assert.match(h.el('ConnectionHint').textContent, /Being signed in to Edge does not connect/);
  assert.equal(h.el('ConnectionPanel').dataset.state, 'required');
  assert.match(h.el('Connect').textContent, /Sign in/);
  h.search(async () => ({ code: 'ok', items: [{ id: 'VWAQ4', title: 'Platform' }], more: false }));
  h.el('ProjectSearchButton').handlers.click(); await tick();
  assert.equal(h.el('Connection').dataset.state, 'connected');
  h.el('Results').children[0].handlers.click(); await tick();
  assert.equal(h.el('TaskSearchButton').disabled, false);
  assert.equal(h.el('Pages').hidden, true);
  assert.match(h.el('ChosenProject').textContent, /VWAQ4/);
  assert.equal(h.el('PanelProjects').hidden, false, 'choosing a project must not move to another tab');
  assert.equal(h.el('PanelTasks').hidden, true);
  assert.match(h.el('Status').textContent, /Project selected: VWAQ4/);
  const count = h.calls.length;
  h.el('TabTasks').handlers.click();
  assert.equal(h.el('PanelTasks').hidden, false);
  assert.equal(h.calls.length, count, 'explicit tab switches do not search or authenticate');
});

test('closed portal is clearly disconnected; unrelated network and permission errors do not claim sign-in failed', async () => {
  const h = harness(); await h.picker.open('/repo');
  h.search(async () => { throw 'Open Polarion, sign in if needed, then retry Search.'; });
  await h.el('ProjectSearchButton').handlers.click();
  assert.equal(h.el('Connection').dataset.state, 'closed');
  assert.match(h.el('Connection').textContent, /Not connected/);
  assert.match(h.el('ConnectionHint').textContent, /Sign in there only if asked/);
  assert.match(h.el('Connect').textContent, /Open Polarion/);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0, 'no unexpected sign-in or background probes');
  h.dialog.close(); await h.picker.open('/repo');
  assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.match(h.el('ConnectionHint').textContent, /Saved snapshots do not prove a connection/);
  for (const code of ['network_error', 'forbidden', 'api_unavailable']) {
    h.search(async () => ({ code })); await h.el('ProjectSearchButton').handlers.click();
    assert.equal(h.el('Connection').dataset.state, 'unknown', code + ' does not prove a disconnected session');
  }
});

test('closing and reopening Polarion rejects old search and authentication results', async () => {
  const h = harness(); await h.picker.open('/repo');
  let complete;
  h.search(() => new Promise(resolve => { complete = resolve; }));
  h.el('ProjectSearchButton').handlers.click();
  h.dialog.close(); await h.picker.open('/another');
  complete({ code: 'auth_required' }); await tick();
  assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.equal(h.el('Results').children.length, 0);
  assert.equal(h.el('ProjectSearchButton').disabled, false);
});

test('stash empty spacing is isolated; existing start accuracy explanation is retained', () => {
  const css = fs.readFileSync(path.join(__dirname, '../frontend/styles.css'), 'utf8');
  const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  assert.match(css, /#stashesList > \.stash-empty \{ min-height: 100px; height: auto; padding: 24px 16px/);
  assert.match(app, /class="empty-change stash-empty"/);
  assert.match(app, /not necessarily the original branch start/);
});

const task = (id = 'P/P-1', type = 'task') => ({ id, title: 'Task title', kind: 'workitem', item_type: type, status: 'open', assignees: ['me'], fields: { plannedStart: '2026-10-16', revision: '42' }, fetched_at: '2026-10-16T10:00:00Z' });
async function inProject(h) {
  h.overrides.polarion_project_context = async args => ({ repository_path: args.repositoryPath, project: { id: 'P', name: 'Project' } });
  await h.picker.open('/repo');
}
async function selectedTask(h) {
  await inProject(h);
  h.search(async () => ({ code: 'ok', items: [{ id: 'P-1', title: 'Task title', item_type: 'task' }] }));
  await h.el('TaskSearchButton').handlers.click();
  h.el('Results').children[0].handlers.click();
}
const cardButton = (h, card, label) => h.el(card).children[1].children.find(b => b.textContent === label);

test('manual assignee is remembered once and survives reopening without another API detection', async () => {
  const h = harness(); await inProject(h);
  h.el('Assigned').checked = true; h.el('User').value = 'myuser';
  await h.el('TaskSearchButton').handlers.click();
  h.dialog.close(); await h.picker.open('/repo');
  assert.equal(h.el('User').hidden, true);
  await h.el('TaskSearchButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').at(-1).args.user, 'myuser');
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, 0);
});
test('detected identity is reused; Other user never changes the remembered me', async () => {
  const h = harness(); await inProject(h);
  h.overrides.polarion_read = async () => ({ code: 'ok', user_id: 'me', user_name: 'My name' });
  await h.el('DetectUser').handlers.click();
  h.el('OtherUser').handlers.click(); h.el('User').value = 'colleague';
  await h.el('TaskSearchButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').at(-1).args.user, 'colleague');
  assert.match([...h.memory.values()][0], /"id":"me"/);
  h.el('UseMe').handlers.click(); await h.el('TaskSearchButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').at(-1).args.user, 'me');
});
test('primary task reads details and saves locally, without adding a message or committing', async () => {
  const h = harness(); await selectedTask(h);
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: task() });
  await h.el('PrimaryButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_workspace').at(-1).args.action, 'primary');
  assert.equal(h.el('Detail').hidden, false); assert.equal(h.messages.length, 0);
  h.dialog.close(); await h.picker.open('/repo');
  const reads = h.calls.filter(c => c.name === 'polarion_read').length;
  cardButton(h, 'Primary', 'View saved').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, reads);
  h.el('Add').handlers.click(); assert.equal(h.messages[0], 'P:P-1 - Task title');
});
test('late detail response after closing cannot save into a reopened workspace', async () => {
  const h = harness(); await selectedTask(h);
  let finish; h.overrides.polarion_read = () => new Promise(r => { finish = r; });
  const pending = h.el('PrimaryButton').handlers.click();
  h.dialog.close(); await h.picker.open('/other');
  finish({ code: 'ok', snapshot: task() }); await pending;
  assert.equal(h.calls.filter(c => c.args?.action === 'primary').length, 0);
  assert.equal(h.el('Detail').hidden, true);
});
test('StructDoc reads every page, preserves all actual types and filters only locally', async () => {
  const h = harness(), doc = { ...task('P/Space/Structure'), kind: 'document', item_type: 'structureDocument' };
  h.setWorkspace({ document: doc, elements: [] }); await inProject(h);
  h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => ({ code: 'ok', received: 1, page_key: 'part' + args.page, more: args.page < 3,
    elements: [task('P/P-' + args.page, ['architecturalElement','designElement','solutionElement'][args.page - 1])] });
  await cardButton(h, 'Document', 'Load elements · server').handlers.click();
  assert.equal(h.el('Elements').children.length, 3);
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, 3);
  assert.ok(h.calls.filter(c => c.name === 'polarion_read').every(c => c.args.revision === '42'));
  const before = h.calls.length;
  h.el('ElementType').value = 'designElement'; h.el('ElementType').handlers.change();
  assert.equal(h.el('Elements').children.length, 1); assert.equal(h.calls.length, before);
  assert.equal(h.el('Elements').children[0].disabled, false, 'rows must be selectable after loading');
});
test('failed element page preserves the previous complete local snapshot', async () => {
  const h = harness(); h.setWorkspace({ document: { ...task('P/S/D'), kind: 'document' }, elements: [task('P/OLD')], elements_complete: true }); await inProject(h);
  h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => args.page === 1 ? { code: 'ok', received: 1, more: true, elements: [task('P/NEW')] } : { code: 'forbidden', http: 403 };
  await cardButton(h, 'Document', 'Refresh elements · server').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.action === 'elements').length, 0);
  assert.match(h.el('Elements').children[0].children[0].textContent, /OLD/);
  assert.match(h.el('Status').textContent, /refused/);
});
test('saved StructDoc elements are not rendered during ordinary task searches', async () => {
  const h = harness(); h.setWorkspace({ document: { ...task('P/S/D'), kind: 'document' }, elements: [task('P/P-2')] }); await inProject(h);
  assert.equal(h.el('Elements').children.length, 0);
  h.el('TabDocuments').handlers.click(); assert.equal(h.el('Elements').children.length, 1);
  h.el('TabTasks').handlers.click(); assert.equal(h.el('Elements').children.length, 0);
});

test('diagnostics are explicit, preserve associations and never replace normal search results or commit messages', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DocumentQuery').value = 'Doc';
  h.search(async () => ({ code: 'ok', items: [{ id: 'P/S/D', title: 'Document', item_type: 'structureDocument' }] }));
  await h.el('DocumentSearchButton').handlers.click(); h.el('Results').children[0].handlers.click();
  const before = h.calls.length, selected = h.el('Results').children[0];
  h.el('DiagnosticUrl').value = 'https://polarion.vitesco.io/polarion/#/project/P/wiki/S/D';
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', server_metadata: { version: '3.25.12' }, diagnostics: [
    { step: 'tasks_baseline', code: 'ok', http: 200, duration_ms: 50 }, { step: 'document_exact', code: 'api_unavailable', http: 404, reason: 'not_found' },
  ] });
  await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.length, before + 1); assert.equal(h.calls.at(-1).name, 'polarion_diagnostics');
  assert.equal(h.calls.at(-1).args.project, 'P'); assert.equal(h.calls.at(-1).args.query, '');
  assert.equal(h.calls.at(-1).args.documentQuery, 'Doc');
  assert.equal(h.el('Results').children[0], selected); assert.equal(h.messages.length, 0);
  assert.match(h.el('Status').textContent, /1\/2 accepted/);
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  assert.match(html, /404 alone does not prove/);
  assert.equal(h.el('Diagnostics').disabled, false);
});
test('work-item diagnostics test the current fragment only in that mode', async () => {
  const h = harness(); await inProject(h);
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', diagnostics: [] });
  h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'containers'; h.el('DocumentQuery').value = 'errm';
  await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.at(-1).args.query, 'errm');
  assert.equal(h.calls.at(-1).args.documentQuery, '');
  h.el('DocumentSource').value = 'documents'; await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.at(-1).args.query, '');
  assert.equal(h.calls.at(-1).args.documentQuery, 'errm');
});
test('explicit diagnostics can reuse the same-project saved StructDoc, but never change associations or probe automatically', async () => {
  const h = harness(); h.setWorkspace({ document: { ...task('P/Structure Information/PRIVATE_DOC'), kind: 'document' } }); await inProject(h);
  h.el('TabDocuments').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_diagnostics').length, 0);
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', diagnostics: [
    { step: 'known_document_query', query_variant: 'known_module_exact', code: 'ok', http: 200, count: 0, known_match: false },
    { step: 'document_query', query_variant: 'location_tokens_prefix', code: 'ok', http: 200, count: 1, known_match: true },
  ] });
  await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.at(-1).args.resourceUrl, 'https://polarion.vitesco.io/polarion/#/project/P/wiki/Structure%20Information/PRIVATE_DOC');
  assert.match(h.el('DiagnosticReport').children[0].children[1].textContent, /0 rows in sample.*known reference not in sample/);
  assert.match(h.el('DiagnosticReport').children[1].children[0].textContent, /location tokens prefix/);
  assert.match(h.el('DiagnosticReport').children[1].children[1].textContent, /known reference found/);
  h.el('DiagnosticUrl').value = 'https://polarion.vitesco.io/polarion/#/project/Other/wiki/S/Explicit';
  await h.el('Diagnostics').handlers.click(); assert.equal(h.calls.at(-1).args.resourceUrl, h.el('DiagnosticUrl').value);
  assert.equal(h.calls.filter(c => c.name === 'polarion_workspace' && c.args.action !== 'load').length, 0);
  const { polarionDiagnosticDocumentLink } = require('../frontend/polarion.js');
  assert.equal(polarionDiagnosticDocumentLink('', { kind: 'document', id: 'Other/S/D' }, 'P'), '');
  assert.equal(polarionDiagnosticDocumentLink('', { kind: 'document', id: 'P/../D' }, 'P'), '');
  assert.equal(polarionDiagnosticDocumentLink('', { kind: 'workitem', id: 'P/P-1' }, 'P'), '');
});
test('diagnostic UI shows observed element types and sample limits, not a claim of full document access', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', diagnostics: [
    { step: 'document_exact', code: 'ok', http: 200 },
    { step: 'document_parts_expanded', code: 'ok', http: 200, workitem_links: 3, included_workitems: 2, item_types: ['architecturalElement','designElement'] },
    { step: 'workitem_sample', code: 'forbidden', http: 403 },
  ] });
  await h.el('Diagnostics').handlers.click();
  assert.match(h.el('DiagnosticReport').children[1].children[1].textContent, /architecturalElement, designElement/);
  assert.match(h.el('DiagnosticReport').children[1].children[1].textContent, /sample only/);
  assert.match(h.el('DiagnosticSummary').textContent, /do not verify the whole document/);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect' || c.name === 'polarion_read').length, 0);
  assert.equal(h.calls.filter(c => c.name === 'polarion_workspace' && c.args.action !== 'load').length, 0);
});
test('late diagnostic replies cannot change a reopened dialog and do not trigger sign-in automatically', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  let finish; h.overrides.polarion_diagnostics = () => new Promise(r => { finish = r; });
  const pending = h.el('Diagnostics').handlers.click();
  h.dialog.close(); await h.picker.open('/another');
  finish({ code: 'ok', diagnostics: [{ step: 'current_user', code: 'auth_required', http: 401 }] }); await pending;
  assert.equal(h.el('Connection').dataset.state, 'unknown'); assert.equal(h.el('DiagnosticReport').children.length, 0);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0);
});

const docLink = 'https://polarion.vitesco.io/polarion/#/project/P/wiki/Structure%20Information/Structure_2_0';
const docSnapshot = () => ({ ...task('P/Structure Information/Structure_2_0'), kind: 'document', item_type: 'structureDocument' });
test('Search always checks the server and keeps a clearly marked saved match when the server catalog returns nothing', async () => {
  const h = harness(); h.setWorkspace({ document: { ...docSnapshot(), id: 'P/Structure Information/VWAQ4-SW_ERRM_StructDoc_2_0_0', title: 'VWAQ4-SW_ERRM_StructDoc_2.0.0' } });
  await inProject(h); h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'eRrM';
  const before = h.calls.length; await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.calls.length, before + 1); assert.equal(h.el('Results').children.length, 1);
  assert.equal(h.el('Results').children[0].dataset.origin, 'local');
  assert.match(h.el('Results').children[0].children[2].textContent, /LOCAL · saved snapshot/);
  assert.match(h.el('Status').textContent, /1 saved locally/);
  assert.equal(h.el('Connection').dataset.state, 'connected', 'only the actual successful server request verifies access');
  await h.el('DocumentSearchButton').handlers.click(); assert.equal(h.calls.filter(c => c.name === 'polarion_search').length, 2);
  h.el('ElementQuery').value = 'errm'; h.el('ClearElements').handlers.click(); assert.equal(h.el('ElementQuery').value, '');
});
test('local document actions display saved data without sign-in or server calls, with explicit server refresh buttons', async () => {
  const h = harness(); h.setWorkspace({ document: docSnapshot(), elements: [task()], elements_complete: true, elements_revision: '42' });
  await inProject(h); h.el('TabDocuments').handlers.click(); const before = h.calls.length;
  cardButton(h, 'Document', 'View local snapshot').handlers.click();
  assert.equal(h.calls.length, before); assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.match(h.el('Detail').children[0].textContent, /LOCAL SNAPSHOT · no server request/);
  assert.match(h.el('Status').textContent, /No server request/);
  assert.ok(cardButton(h, 'Document', 'Refresh details · server'));
  assert.ok(cardButton(h, 'Document', 'Refresh elements · server'));
  assert.ok(cardButton(h, 'Document', 'Unlink locally'));
  h.el('ElementQuery').value = 'P-1'; h.el('ElementQuery').handlers.input();
  assert.equal(h.calls.length, before, 'local element filtering never searches Polarion');
});
test('server, server cache and local result origins stay distinct; changing tabs clears the source summary', async () => {
  const h = harness(); h.setWorkspace({ document: { ...docSnapshot(), id: 'P/S/ERRM_LOCAL' } }); await inProject(h);
  h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'errm';
  const remote = { id: 'P/S/ERRM_SERVER', title: 'ERRM server document' };
  h.search(async () => ({ code: 'ok', items: [remote], catalog_cached: true, received: 1, matched: 1 }));
  await h.el('DocumentSearchButton').handlers.click();
  assert.deepEqual(h.el('Results').children.map(row => row.dataset.origin), ['cache','local']);
  assert.match(h.el('ResultSummary').textContent, /1 cached server results · 1 local matches/);
  const before = h.calls.length; h.el('Results').children[1].handlers.click();
  assert.equal(h.calls.length, before); assert.match(h.el('Detail').children[0].textContent, /LOCAL SNAPSHOT/);
  h.search(async () => ({ code: 'ok', items: [remote], received: 1, matched: 1 }));
  await h.el('DocumentSearchButton').handlers.click(); assert.equal(h.el('Results').children[0].dataset.origin, 'server');
  assert.match(h.el('SearchScope').textContent, /document names.*in P/);
  h.el('DocumentSource').value = 'containers'; h.el('DocumentSource').handlers.change();
  assert.match(h.el('SearchScope').textContent, /work-item IDs and titles/);
  h.el('TabTasks').handlers.click(); assert.equal(h.el('ResultSummary').hidden, true);
});
test('failed server search never presents a saved local match as a successful or still-pending server result', async () => {
  const h = harness(); h.setWorkspace({ document: { ...docSnapshot(), id: 'P/S/ERRM_LOCAL' } }); await inProject(h);
  h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'errm';
  h.search(async () => ({ code: 'auth_required' })); await h.el('DocumentSearchButton').handlers.click();
  assert.match(h.el('ResultSummary').textContent, /Server search did not complete/);
  assert.doesNotMatch(h.el('ResultSummary').textContent, /searching Polarion…/);
  assert.equal(h.el('Results').children[0].dataset.origin, 'local'); assert.equal(h.el('Connection').dataset.state, 'required');
});
test('a document element can be inspected and refreshed but cannot replace the StructDoc or erase its saved elements', async () => {
  const h = harness(), doc = docSnapshot(), element = { ...task('P/P-85586', 'designelement'), title: 'errm_adpdg 2.0.0' };
  h.setWorkspace({ document: doc, elements: [element], elements_complete: true }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('Elements').children[0].handlers.click();
  assert.equal(h.el('Associate').disabled, true);
  await h.el('Associate').handlers.click();
  assert.match(h.el('Status').textContent, /document element, not a selected StructDoc/);
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, 0);
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: element });
  await h.el('View').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.action === 'document').length, 0);
  assert.equal(h.calls.filter(c => c.args?.action === 'element').length, 1);
  assert.match(h.el('Document').children[0].children[1].textContent, /P\/Structure Information\/Structure_2_0/);
  assert.equal(h.el('Elements').children.length, 1);
});
test('element filtering refreshes locally across text/type/clear changes without server requests', async () => {
  const h = harness(); h.setWorkspace({ document: docSnapshot(), elements_complete: true, elements: [
    { ...task('P/P-1', 'designelement'), title: 'errm_adpdg 2.0.0' }, { ...task('P/P-2', 'solutionelement'), title: 'errm_dcmngai 2.0.0' },
  ] }); await inProject(h); h.el('TabDocuments').handlers.click(); const before = h.calls.length;
  h.el('ElementQuery').value = 'ERRM ADPDG'; h.el('ElementQuery').handlers.input(); assert.equal(h.el('Elements').children.length, 1);
  h.el('ElementType').value = 'solutionelement'; h.el('ElementType').handlers.change(); assert.equal(h.el('Elements').children.length, 0);
  h.el('ClearElements').handlers.click(); assert.equal(h.el('Elements').children.length, 2); assert.equal(h.calls.length, before);
});
test('opening a LiveDoc link bypasses catalogs without automatically saving or reading its elements', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => {
    assert.equal(args.kind, 'document_link'); assert.equal(args.resourceId, docLink);
    return { code: 'ok', snapshot: docSnapshot() };
  };
  const before = h.calls.length;
  h.el('DiagnosticUrl').value = docLink; h.el('DiagnosticUrl').handlers.input();
  assert.equal(h.el('OpenDocument').disabled, false);
  await h.el('OpenDocument').handlers.click();
  assert.equal(h.calls.length, before + 1); assert.equal(h.calls.at(-1).name, 'polarion_read');
  assert.equal(h.el('Detail').hidden, false); assert.equal(h.el('Associate').disabled, false);
  assert.equal(h.el('Results').hidden, true); assert.equal(h.el('DocumentElements').hidden, true);
  assert.match(h.el('Status').textContent, /Document opened/); assert.match(h.el('DocumentHint').textContent, /Catalog access is not required/);
  assert.equal(h.el('Detail').parent, h.el('DocumentWorkspace')); assert.equal(h.messages.length, 0);
});
test('a link can be opened without selecting or silently changing a project', async () => {
  const h = harness(); await h.picker.open('/repo'); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: docSnapshot() });
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_project_context').length, 1);
  assert.match(h.el('ChosenProject').textContent, /No project selected/);
  assert.equal(h.el('Detail').hidden, false); assert.equal(h.el('Associate').disabled, false);
});
test('link association uses the existing local workspace and paginated element reader', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => args.kind === 'elements'
    ? { code: 'ok', received: 1, page_key: 'part' + args.page, more: args.page < 2, elements: [task('P/P-' + args.page, args.page === 1 ? 'architecturalElement' : 'solutionElement')] }
    : { code: 'ok', snapshot: docSnapshot() };
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  await h.el('Associate').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.action === 'document').length, 1);
  assert.equal(h.el('Elements').children.length, 2); assert.equal(h.el('DocumentElements').hidden, false);
  assert.equal(h.calls.filter(c => c.args?.action === 'elements').length, 1, 'explicit association loads and saves every page without a second action');
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').length, 0);
  assert.ok(h.calls.filter(c => c.args?.kind === 'elements').every(c => c.args.revision === '42'));
});
test('selecting a document with failed element loading keeps its identity but never marks an empty metadata read as complete', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => args.kind === 'elements' ? { code: 'auth_required', http: 401 }
    : { code: 'ok', snapshot: docSnapshot() };
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  await h.el('Associate').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.action === 'document').length, 1);
  assert.equal(h.calls.filter(c => c.args?.action === 'elements').length, 0);
  assert.match(h.el('Status').textContent, /StructDoc selected locally, but elements could not be refreshed/);
  assert.equal(h.el('Connection').dataset.state, 'required');
  assert.match(h.el('Document').children[0].children[2].textContent, /Elements not loaded yet/);
  assert.ok(cardButton(h, 'Document', 'Load elements · server'));
  assert.match(h.logs.join('\n'), /elements_load outcome=failed/);
});
test('failed refresh after reselecting the same document preserves its last complete snapshot', async () => {
  const h = harness(); h.setWorkspace({ document: docSnapshot(), elements: [task('P/P-10')], elements_complete: true, elements_revision: '42' });
  await inProject(h); h.el('TabDocuments').handlers.click();
  cardButton(h, 'Document', 'View local snapshot').handlers.click();
  h.overrides.polarion_read = async args => args.kind === 'elements' ? { code: 'timeout' } : { code: 'ok', snapshot: docSnapshot() };
  await h.el('Associate').handlers.click();
  assert.equal(h.el('Elements').children.length, 1);
  assert.match(h.el('Elements').children[0].children[0].textContent, /P-10/);
  assert.match(h.el('Status').textContent, /Previous saved elements.*preserved/);
});
test('late element pages cannot save results in a reopened workspace', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  let finish;
  h.overrides.polarion_read = async args => args.kind === 'elements' ? new Promise(resolve => { finish = resolve; })
    : { code: 'ok', snapshot: docSnapshot() };
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  const pending = h.el('Associate').handlers.click(); await tick();
  h.dialog.close(); await h.picker.open('/other');
  finish({ code: 'ok', elements: [task('P/P-20')], received: 1, more: false }); await pending;
  assert.equal(h.calls.filter(c => c.args?.action === 'elements').length, 0);
  assert.match(h.logs.join('\n'), /elements_load outcome=stale/);
});
test('parent StructDoc source matches submodule commits while task and project associations retain their repository scope', async () => {
  const h = harness();
  const doc = docSnapshot(), childTask = task('Child/CHILD-1');
  const stores = new Map([['/parent', { document: doc, elements: [], elements_complete: false, primary_task: task('Parent/PARENT-1') }],
    ['/parent/module', { primary_task: childTask, document: { ...doc, id: 'Child/S/Old' } }]]);
  h.overrides.polarion_project_context = async args => ({ repository_path: args.repositoryPath, project: { id: 'Child', name: 'Module project' } });
  h.overrides.polarion_workspace = async args => {
    const data = stores.get(args.repositoryPath);
    if (args.action === 'document') data.document = args.snapshot;
    if (args.action === 'elements') Object.assign(data, { elements: args.elements, elements_complete: true, elements_revision: args.snapshot.fields?.revision || '', elements_saved_at: args.snapshot.fetched_at });
    if (args.action === 'primary') data.primary_task = args.snapshot;
    return structuredClone(data);
  };
  const refs = { repositoryPath: '/parent', moduleName: 'errm_adpdg' };
  await h.picker.open('/parent/module', '', refs); h.el('TabDocuments').handlers.click();
  assert.match(h.el('DocumentScope').textContent, /\/parent · parent project/);
  assert.match(h.el('Document').children[0].children[1].textContent, /P\/Structure Information/);
  const elements = ['simulinkitem','designelement','solutionelement'].map((type, index) => ({ ...task('VWAQ4/VWAQ4-' + (85585 + index), type), title: 'errm_adpdg 2.0.0' }));
  h.overrides.polarion_read = async args => args.kind === 'elements' ? { code: 'ok', elements: [...elements, { ...task('P/P-99'), title: 'errm_dcmngai 2.0.0' }], received: 4, more: false }
    : { code: 'ok', snapshot: doc };
  cardButton(h, 'Document', 'View local snapshot').handlers.click(); await h.el('Associate').handlers.click();
  assert.equal(h.el('ElementModule').checked, true);
  assert.match(h.el('ElementModuleName').textContent, /Only errm_adpdg/);
  assert.equal(h.el('Elements').children.length, 3);
  assert.match(h.el('Primary').children[0].children[1].textContent, /CHILD-1/);
  assert.ok(h.calls.filter(c => ['document','elements'].includes(c.args?.action)).every(c => c.args.repositoryPath === '/parent'));
  assert.equal(stores.get('/parent/module').document.id, 'Child/S/Old', 'no migration or overwrite of the old independent module association');
  const noReads = h.calls.length;
  h.el('Elements').children[0].handlers.click();
  assert.equal(h.el('ElementDetail').hidden, false);
  assert.match(h.el('ElementDetail').children[1].textContent, /errm_adpdg/);
  h.el('ClearElements').handlers.click(); assert.equal(h.el('Elements').children.length, 4); assert.equal(h.calls.length, noReads);
  h.el('TabTasks').handlers.click();
  h.search(async () => ({ code: 'ok', items: [{ id: 'CHILD-2', title: 'New child task', item_type: 'task' }] }));
  await h.el('TaskSearchButton').handlers.click(); h.el('Results').children[0].handlers.click();
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: task('Child/CHILD-2') });
  await h.el('PrimaryButton').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.action === 'primary').at(-1).args.repositoryPath, '/parent/module');
  assert.equal(stores.get('/parent').primary_task.id, 'Parent/PARENT-1');
  const before = h.calls.filter(c => c.name === 'polarion_read').length;
  h.dialog.close(); await h.picker.open('/parent/module', '', refs); h.el('TabDocuments').handlers.click();
  assert.equal(h.el('Elements').children.length, 3, 'all elements survive reopening; the module filter is local');
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, before);
});
test('local element inspection has its own usable explicit refresh and never implies write access', async () => {
  const h = harness(), element = { ...task('P/P-85', 'designelement'), title: 'errm_adpdg 2.0.0' };
  h.setWorkspace({ document: docSnapshot(), elements: [element], elements_complete: true, elements_revision: '42' });
  await inProject(h); h.el('TabDocuments').handlers.click();
  const before = h.calls.length; h.el('Elements').children[0].handlers.click();
  assert.equal(h.calls.length, before, 'selecting an element only shows the saved snapshot');
  assert.equal(h.el('ElementDetail').hidden, false);
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: { ...element, description: '<not executed>' } });
  await h.el('ElementDetail').children.at(-1).handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, 1);
  assert.equal(h.el('ElementDetail').children.at(-1).disabled, false);
  assert.equal(h.calls.filter(c => c.args?.action === 'document').length, 0);
  assert.ok(h.calls.every(c => ['polarion_workspace','polarion_project_context','polarion_read'].includes(c.name)));
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  assert.match(html, /id="polarionCheckWriteAccess"/);
  assert.match(html, /No test edits are sent/);
});
test('permission checks are explicit and scoped to the saved document, with no association changes or REST sign-in side effects', async () => {
  const h = harness(); h.setWorkspace({ document: docSnapshot() }); await inProject(h); h.el('TabDocuments').handlers.click();
  assert.equal(h.calls.filter(c => c.args?.kind === 'permissions').length, 0);
  assert.equal(h.el('CheckWriteAccess').disabled, false);
  h.el('DocumentQuery').value = 'errm'; await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.el('Connection').dataset.state, 'connected');
  h.overrides.polarion_read = async args => {
    assert.equal(args.kind, 'permissions'); assert.equal(args.resourceId, docSnapshot().id);
    return { code: 'ok', permission_checks: [
      { step: 'security_wsdl', code: 'ok', http: 200, duration_ms: 3, operations: ['canModifyInstance','canModifyKey'] },
      { step: 'soap_session', code: 'ok', http: 200, duration_ms: 1, allowed: false },
      { step: 'document_edit', code: 'not_run', http: 0, duration_ms: 0 },
    ] };
  };
  const before = h.calls.length; await h.el('CheckWriteAccess').handlers.click();
  assert.equal(h.calls.length, before + 1);
  assert.match(h.el('WriteSummary').textContent, /no authenticated session.*rights remain unknown.*REST connection is unchanged/);
  assert.equal(h.el('Connection').dataset.state, 'connected');
  assert.match(h.el('WriteReport').children[1].textContent, /Advertised methods, not authorization/);
  assert.equal(h.calls.filter(c => c.name === 'polarion_workspace' && c.args.action !== 'load').length, 0);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0);
  h.dialog.close(); assert.equal(h.changedSources.length, 0);
});
test('actual permission results distinguish allowed, denied and unavailable checks, and late results cannot overwrite a reopened window', async () => {
  const h = harness(); h.setWorkspace({ document: docSnapshot() }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async () => ({ code: 'ok', permission_checks: [
    { step: 'document_edit', code: 'ok', http: 200, duration_ms: 1, allowed: false },
    { step: 'document_text', code: 'ok', http: 200, duration_ms: 1, allowed: true },
    { step: 'project_document_content', code: 'forbidden', http: 403, duration_ms: 1 },
  ] });
  await h.el('CheckWriteAccess').handlers.click();
  assert.match(h.el('WriteSummary').textContent, /Modify document object: denied/);
  assert.match(h.el('WriteReport').children[1].children[1].textContent, /Allowed by this permission check/);
  assert.match(h.el('WriteReport').children[2].children[1].textContent, /not a false permission result/);
  let finish; h.overrides.polarion_read = () => new Promise(resolve => { finish = resolve; });
  const pending = h.el('CheckWriteAccess').handlers.click();
  h.dialog.close(); await h.picker.open('/other');
  finish({ code: 'ok', permission_checks: [{ step: 'document_edit', code: 'ok', http: 200, duration_ms: 1, allowed: true }] }); await pending;
  assert.equal(h.el('WriteReport').children.length, 0); assert.match(h.el('WriteSummary').textContent, /Permissions not checked/);
});
test('permission bridge errors do not leave a stale checking indicator and ordinary task/project use does not run checks', async () => {
  const h = harness(); await inProject(h);
  assert.equal(h.el('CheckWriteAccess').disabled, true);
  assert.equal(h.calls.filter(c => c.name === 'polarion_read').length, 0);
  h.dialog.close(); h.setWorkspace({ document: docSnapshot() }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async () => { throw Error('Bridge unavailable'); };
  await h.el('CheckWriteAccess').handlers.click();
  assert.match(h.el('WriteSummary').textContent, /did not complete.*rights remain unknown/);
  assert.equal(h.el('CheckWriteAccess').disabled, false);
});
test('a failed parent workspace read never exposes a child StructDoc as the parent commit source', async () => {
  const h = harness();
  h.overrides.polarion_project_context = async () => ({ repository_path: '/child', project: { id: 'P' } });
  h.overrides.polarion_workspace = async args => {
    if (args.repositoryPath === '/parent') throw new Error('Cannot read parent workspace');
    return { document: { ...docSnapshot(), title: 'Wrong child source' }, primary_task: task('Child/CHILD-1') };
  };
  await h.picker.open('/child', '', { repositoryPath: '/parent', moduleName: 'errm_adpdg' });
  h.el('TabDocuments').handlers.click();
  assert.doesNotMatch(h.el('Document').children[0].children[1].textContent, /Wrong child source/);
  assert.equal(h.el('DocumentElements').hidden, true);
  assert.match(h.el('Primary').children[0].children[1].textContent, /CHILD-1/);
  assert.equal(h.calls.filter(c => c.name === 'polarion_read' || c.name === 'polarion_search').length, 0);
});
test('changing a StructDoc invalidates old commit selections once on close; previews and task updates do not', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => args.kind === 'elements' ? { code: 'ok', elements: [task('P/P-1')], received: 1, more: false }
    : { code: 'ok', snapshot: docSnapshot() };
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  h.dialog.close(); assert.equal(h.changedSources.length, 0, 'preview does not change commit references');
  await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  await h.el('Associate').handlers.click();
  await cardButton(h, 'Document', 'Refresh elements · server').handlers.click();
  assert.equal(h.changedSources.length, 0, 'do not repeatedly rebuild the underlying drawer during loading');
  h.dialog.close(); assert.deepEqual(h.changedSources, ['/repo']);
  await selectedTask(h); h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: task() });
  await h.el('PrimaryButton').handlers.click(); h.dialog.close();
  assert.equal(h.changedSources.length, 1, 'task updates do not invalidate StructDoc choices');
  const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  const callback = app.slice(app.indexOf('onDocumentChange()'), app.indexOf('}, addMessage(message)', app.indexOf('onDocumentChange()')));
  assert.match(callback, /drawerPolarionReferences\?\.reset\(\)/);
  assert.doesNotMatch(callback, /loadRepository|refreshChangesLightweight|invoke\(/);
});
test('document preview does not replace the selected commit source until Use this StructDoc is confirmed', async () => {
  const h = harness(), old = { ...docSnapshot(), id: 'P/S/Old', title: 'Previous selected document' };
  h.setWorkspace({ document: old }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async args => args.kind === 'elements' ? { code: 'ok', elements: [], received: 0, more: false }
    : { code: 'ok', snapshot: { ...docSnapshot(), title: 'New StructDoc' } };
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  assert.match(h.el('Document').children[0].children[0].textContent, /Selected StructDoc.*source for submodule commit/);
  assert.match(h.el('Document').children[0].children[1].textContent, /Previous selected document/);
  await h.el('Associate').handlers.click();
  assert.match(h.el('Document').children[0].children[1].textContent, /New StructDoc/);
  assert.match(h.el('Status').textContent, /Selected StructDoc for submodule commits: New StructDoc/);
  assert.equal(h.calls.filter(c => c.args?.kind === 'elements').length, 1, 'Use this StructDoc & load elements explicitly reads the selected document, never the preview alone');
  assert.equal(h.messages.length, 0, 'selecting a document never changes the general commit list');
});
test('unavailable catalog guides users to the link without triggering retries or claiming authentication failed', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'errm';
  h.search(async () => ({ code: 'api_unavailable', http: 404, session_token: true }));
  await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').length, 1);
  assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.match(h.el('Status').textContent, /Open document by link/);
  assert.equal(h.el('Results').hidden, true); assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0);
});
test('compatible document name search stays selectable and explains incomplete coverage without saving anything', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'errm';
  h.search(async () => ({ code: 'ok', catalog_source: 'work_item_modules', matched: 1, received: 1,
    items: [{ id: 'P/Structure Information/P_ERRM_StructDoc', title: 'P_ERRM_StructDoc' }] }));
  await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.calls.at(-1).args.query, 'errm');
  assert.match(h.el('DocumentHint').textContent, /Empty documents, reference-only documents and title-only matches may be missing/);
  assert.match(h.el('DocumentHint').textContent, /No complete catalog is implied/);
  h.el('Results').children[0].handlers.click();
  assert.equal(h.el('Associate').disabled, false);
  h.overrides.polarion_read = async args => {
    assert.equal(args.kind, 'document'); assert.equal(args.resourceId, 'P/Structure Information/P_ERRM_StructDoc');
    return { code: 'ok', snapshot: { ...docSnapshot(), id: args.resourceId } };
  };
  await h.el('View').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_workspace' && c.args.action !== 'load').length, 0);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0);
});
test('token-based document discovery explicitly explains narrower matching, including zero results', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'errm';
  h.search(async () => ({ code: 'ok', catalog_source: 'work_item_modules', document_query_variant: 'location_tokens_prefix',
    document_search_limited: true, matched: 0, received: 0, items: [] }));
  await h.el('DocumentSearchButton').handlers.click();
  assert.match(h.el('DocumentHint').textContent, /not unrestricted substring matching/);
  assert.match(h.el('DocumentHint').textContent, /token\/prefix or exact-name lookup/);
  assert.match(h.el('Status').textContent, /Server search: 0 matches/);
  assert.equal(h.el('Associate').disabled, true);
});
test('name search rejects too-short fragments without HTTP and discards a result after closing and reopening', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'er';
  await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').length, 0);
  assert.match(h.el('Status').textContent, /at least three/);
  let finish; h.search(() => new Promise(resolve => { finish = resolve; }));
  h.el('DocumentQuery').value = 'errm'; const pending = h.el('DocumentSearchButton').handlers.click();
  h.dialog.close(); await h.picker.open('/other');
  finish({ code: 'ok', catalog_source: 'work_item_modules', items: [{ id: 'P/S/ERRM', title: 'Stale document' }] }); await pending;
  assert.equal(h.el('Results').children.length, 0); assert.equal(h.el('Connection').dataset.state, 'unknown');
});
test('StructDoc content and messages remain in that tab while shared inspection components follow the active tab', async () => {
  const h = harness(); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: docSnapshot() });
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  h.el('TabBody').scrollTop = 500; h.el('TabTasks').handlers.click();
  assert.equal(h.el('PanelDocuments').hidden, true); assert.equal(h.el('TabBody').scrollTop, 0);
  assert.equal(h.el('Detail').hidden, true); assert.equal(h.el('Detail').parent, h.el('TaskWorkspace'));
  assert.ok(!h.el('DocumentWorkspace').children.includes(h.el('Detail')));
  assert.doesNotMatch(h.el('Status').textContent, /Document opened/);
  assert.equal(h.el('SelectionActions').hidden, true); assert.equal(h.el('Associate').hidden, true);
  h.el('TabDocuments').handlers.click();
  assert.equal(h.el('PanelDocuments').hidden, false); assert.equal(h.el('DiagnosticUrl').value, docLink);
  assert.equal(h.el('Detail').parent, h.el('DocumentWorkspace'));
});
test('a late or failed link read cannot replace a saved association or reopen stale details', async () => {
  const h = harness(); h.setWorkspace({ document: { ...docSnapshot(), id: 'P/S/Original' } }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.el('DiagnosticUrl').value = docLink;
  h.overrides.polarion_read = async () => ({ code: 'forbidden', http: 403 });
  await h.el('OpenDocument').handlers.click();
  assert.match(h.el('Document').children[0].children[1].textContent, /P\/S\/Original/);
  assert.equal(h.calls.filter(c => c.args?.action === 'document').length, 0);
  let finish; h.overrides.polarion_read = () => new Promise(r => { finish = r; });
  const pending = h.el('OpenDocument').handlers.click(); h.dialog.close(); await h.picker.open('/other');
  finish({ code: 'ok', snapshot: docSnapshot() }); await pending;
  assert.equal(h.el('Detail').hidden, true); assert.equal(h.el('DiagnosticUrl').value, '');
});
test('Polarion layout has one stable viewport and keeps all StructDoc-specific controls inside its tab', () => {
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  const css = fs.readFileSync(path.join(__dirname, '../frontend/polarion.css'), 'utf8');
  const start = html.indexOf('<section id="polarionPanelDocuments"');
  const end = html.indexOf('<div class="polarion-status-area">', start);
  const doc = html.slice(start, end);
  for (const id of ['polarionDiagnosticUrl', 'polarionOpenDocument', 'polarionDocumentSource', 'polarionDocumentElements', 'polarionDiagnostics', 'polarionDiagnosticReport']) {
    assert.ok(doc.includes('id="' + id + '"'), id);
    assert.equal(html.split('id="' + id + '"').length - 1, 1, id + ' is unique');
  }
  assert.match(css, /height: min\(780px, 90vh\)/);
  assert.match(css, /\.polarion-task-dialog > form \{ height: 100%/);
  assert.match(css, /\.polarion-task-dialog \[hidden\] \{ display: none !important/);
  assert.match(css, /scrollbar-gutter: stable/);
  assert.ok(doc.indexOf('id="polarionDocument"') < doc.indexOf('Find on Polarion'), 'local selection is clearly separate from server search');
  assert.match(doc, /<details class="polarion-open-document">/);
  assert.match(doc, /<h4>Local elements<\/h4>/);
  assert.match(doc, /This does not search Polarion or require sign-in/);
});

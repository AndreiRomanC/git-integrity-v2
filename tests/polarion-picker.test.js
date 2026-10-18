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
  const calls = [], overrides = {}, memory = new Map(), messages = [];
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
      if (args.action === 'document') { workspace.document = args.snapshot; workspace.elements = []; }
      if (args.action === 'elements') { workspace.elements = args.elements; workspace.elements_complete = true; }
      if (args.action === 'element') workspace.elements = workspace.elements.map(i => i.id === args.snapshot.id ? args.snapshot : i);
      return structuredClone(workspace);
    }
  }, addMessage: message => messages.push(message) });
  return { el, dialog, calls, picker, overrides, memory, messages, setWorkspace: value => { workspace = value; }, search: fn => { search = fn; } };
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
  await cardButton(h, 'Document', 'Read all elements').handlers.click();
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
  await cardButton(h, 'Document', 'Read all elements').handlers.click();
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
test('a matching saved StructDoc is found locally even without a working server catalog; explicit server search remains available', async () => {
  const h = harness(); h.setWorkspace({ document: { ...docSnapshot(), id: 'P/Structure Information/VWAQ4-SW_ERRM_StructDoc_2_0_0', title: 'VWAQ4-SW_ERRM_StructDoc_2.0.0' } });
  await inProject(h); h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'documents'; h.el('DocumentQuery').value = 'eRrM';
  const before = h.calls.length; await h.el('DocumentSearchButton').handlers.click();
  assert.equal(h.calls.length, before); assert.equal(h.el('Results').children.length, 1);
  assert.equal(h.el('SearchServer').hidden, false); assert.match(h.el('DocumentHint').textContent, /local association, not a complete server catalog/);
  assert.equal(h.el('Connection').dataset.state, 'unknown', 'a saved match is not proof of current authentication');
  await h.el('SearchServer').handlers.click(); assert.equal(h.calls.at(-1).name, 'polarion_search');
  h.el('ElementQuery').value = 'errm'; h.el('ClearElements').handlers.click(); assert.equal(h.el('ElementQuery').value, '');
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
  await cardButton(h, 'Document', 'Read all elements').handlers.click();
  assert.equal(h.el('Elements').children.length, 2); assert.equal(h.el('DocumentElements').hidden, false);
  assert.equal(h.calls.filter(c => c.name === 'polarion_search').length, 0);
  assert.ok(h.calls.filter(c => c.args?.kind === 'elements').every(c => c.args.revision === '42'));
});
test('document preview does not replace the selected commit source until Use this StructDoc is confirmed', async () => {
  const h = harness(), old = { ...docSnapshot(), id: 'P/S/Old', title: 'Previous selected document' };
  h.setWorkspace({ document: old }); await inProject(h); h.el('TabDocuments').handlers.click();
  h.overrides.polarion_read = async () => ({ code: 'ok', snapshot: { ...docSnapshot(), title: 'New StructDoc' } });
  h.el('DiagnosticUrl').value = docLink; await h.el('OpenDocument').handlers.click();
  assert.match(h.el('Document').children[0].children[0].textContent, /Selected StructDoc.*source for submodule commit/);
  assert.match(h.el('Document').children[0].children[1].textContent, /Previous selected document/);
  await h.el('Associate').handlers.click();
  assert.match(h.el('Document').children[0].children[1].textContent, /New StructDoc/);
  assert.match(h.el('Status').textContent, /Selected StructDoc for submodule commits: New StructDoc/);
  assert.equal(h.calls.filter(c => c.args?.kind === 'elements').length, 0, 'selection alone must not reread a large document');
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
  assert.match(h.el('Status').textContent, /Limited document discovery/);
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
  assert.match(h.el('Status').textContent, /Limited document discovery.*token\/prefix or exact lookup/);
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
});

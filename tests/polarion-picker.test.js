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
    append(...children) { this.children.push(...children); }
    replaceChildren() { this.children = []; }
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
  assert.match(h.el('Connect').textContent, /Sign in/);
  h.search(async () => ({ code: 'ok', items: [{ id: 'VWAQ4', title: 'Platform' }], more: false }));
  h.el('ProjectSearchButton').handlers.click(); await tick();
  assert.equal(h.el('Connection').dataset.state, 'connected');
  h.el('Results').children[0].handlers.click(); await tick();
  assert.equal(h.el('TaskSearchButton').disabled, false);
  assert.equal(h.el('Pages').hidden, true);
  assert.match(h.el('ChosenProject').textContent, /VWAQ4/);
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
  assert.match(h.el('Elements').children[0].children[0].textContent, /P\/OLD/);
  assert.match(h.el('Status').textContent, /refused/);
});
test('saved StructDoc elements are not rendered during ordinary task searches', async () => {
  const h = harness(); h.setWorkspace({ document: { ...task('P/S/D'), kind: 'document' }, elements: [task('P/P-2')] }); await inProject(h);
  assert.equal(h.el('Elements').children.length, 0);
  h.el('TabDocuments').handlers.click(); assert.equal(h.el('Elements').children.length, 1);
  h.el('TabTasks').handlers.click(); assert.equal(h.el('Elements').children.length, 0);
});

test('diagnostics are explicit, preserve associations and never replace normal search results or commit messages', async () => {
  const h = harness(); await selectedTask(h);
  const before = h.calls.length, selected = h.el('Results').children[0];
  h.el('DiagnosticUrl').value = 'https://polarion.vitesco.io/polarion/#/project/P/wiki/S/D';
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', server_metadata: { version: '3.25.12' }, diagnostics: [
    { step: 'tasks_baseline', code: 'ok', http: 200, duration_ms: 50 }, { step: 'document_exact', code: 'api_unavailable', http: 404, reason: 'not_found' },
  ] });
  await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.length, before + 1); assert.equal(h.calls.at(-1).name, 'polarion_diagnostics');
  assert.equal(h.calls.at(-1).args.project, 'P'); assert.equal(h.calls.at(-1).args.query, '');
  assert.equal(h.el('Results').children[0], selected); assert.equal(h.messages.length, 0);
  assert.match(h.el('Status').textContent, /1\/2 accepted/); assert.match(h.el('Status').textContent, /404 alone does not prove/);
  assert.equal(h.el('Diagnostics').disabled, false);
});
test('work-item diagnostics test the current fragment only in that mode', async () => {
  const h = harness(); await inProject(h);
  h.overrides.polarion_diagnostics = async () => ({ code: 'ok', diagnostics: [] });
  h.el('TabDocuments').handlers.click(); h.el('DocumentSource').value = 'containers'; h.el('DocumentQuery').value = 'errm';
  await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.at(-1).args.query, 'errm');
  h.el('DocumentSource').value = 'documents'; await h.el('Diagnostics').handlers.click();
  assert.equal(h.calls.at(-1).args.query, '');
});
test('late diagnostic replies cannot change a reopened dialog and do not trigger sign-in automatically', async () => {
  const h = harness(); await inProject(h);
  let finish; h.overrides.polarion_diagnostics = () => new Promise(r => { finish = r; });
  const pending = h.el('Diagnostics').handlers.click();
  h.dialog.close(); await h.picker.open('/another');
  finish({ code: 'ok', diagnostics: [{ step: 'current_user', code: 'auth_required', http: 401 }] }); await pending;
  assert.equal(h.el('Connection').dataset.state, 'unknown'); assert.equal(h.el('DiagnosticReport').children.length, 0);
  assert.equal(h.calls.filter(c => c.name === 'polarion_connect').length, 0);
});

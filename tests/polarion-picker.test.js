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
  const calls = [];
  let search = async () => ({ code: 'ok', items: [], more: false });
  const sandbox = { document: { querySelector: () => dialog, createElement: () => new Element() } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../frontend/polarion.js'), 'utf8'), sandbox);
  const picker = sandbox.createPolarionTaskPicker({ invoke: async (name, args) => {
    calls.push({ name, args });
    if (name === 'polarion_project_context') return { repository_path: '/repo', project: null };
    if (name === 'polarion_search') return search();
  }, addMessage: () => {} });
  return { el, dialog, calls, picker, search: fn => { search = fn; } };
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('Polarion flow distinguishes unknown, sign-in required and verified access; tasks require a project', async () => {
  const h = harness();
  await h.picker.open('/repo');
  assert.equal(h.el('Connection').dataset.state, 'unknown');
  assert.equal(h.el('TaskSearchButton').disabled, true);
  assert.equal(h.calls.length, 1, 'opening the picker must not search or prompt for authentication');
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

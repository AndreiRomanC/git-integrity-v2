import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(directory, '../..');
const model = JSON.parse(await readFile(join(directory, 'architecture.json'), 'utf8'));

test('the overview begins with the three real application layers', () => {
  assert.deepEqual(model.primaryLayers.map(layer => layer.entityId), ['presentation', 'tauri-boundary', 'backend']);
  for (const layer of model.primaryLayers) {
    assert.ok(layer.purpose && layer.input && layer.output && layer.why);
    assert.ok(model.entities.some(entity => entity.id === layer.entityId));
  }
});

test('every file and production function is traceable to source and architecture', async () => {
  const entityIds = new Set(model.entities.map(entity => entity.id));
  const fileIds = new Set(model.files.map(file => file.id));
  for (const file of model.files) {
    assert.ok(entityIds.has(file.componentId), `${file.path} has an unknown component`);
    assert.ok((file.componentIds || []).every(id => entityIds.has(id)), `${file.path} has an unknown responsibility`);
    assert.equal(await readFile(join(repositoryRoot, file.path), 'utf8').then(() => true), true);
  }
  for (const fn of model.functions.filter(fn => !fn.test)) {
    assert.ok(fileIds.has(fn.fileId), `${fn.name} has an unknown file`);
    assert.ok(entityIds.has(fn.componentId), `${fn.name} has an unknown component`);
    assert.ok(fn.source.path && fn.source.startLine > 0);
    assert.ok(fn.importanceReasons.length, `${fn.name} has no rank explanation`);
  }
});

test('production importance ranks are unique, ordered and complete', () => {
  const ranked = model.functions.filter(fn => !fn.test).sort((a, b) => a.importanceRank - b.importanceRank);
  assert.equal(ranked.length, model.metadata.productionFunctions);
  assert.deepEqual(ranked.map(fn => fn.importanceRank), Array.from({ length: ranked.length }, (_, index) => index + 1));
});

test('flows only reference known architectural entities', () => {
  const entityIds = new Set(model.entities.map(entity => entity.id));
  for (const flow of model.executionFlows) {
    assert.ok(flow.steps.length >= 3, `${flow.name} is not an end-to-end flow`);
    for (const step of flow.steps) assert.ok(entityIds.has(step.entityId), `${flow.name} references ${step.entityId}`);
  }
});

test('the file fallback is generated from the canonical JSON', async () => {
  const fallback = await readFile(join(directory, 'architecture-data.js'), 'utf8');
  const prefix = 'window.__ARCHITECTURE_MODEL__ = ';
  const start = fallback.indexOf(prefix);
  assert.ok(start >= 0);
  const embedded = JSON.parse(fallback.slice(start + prefix.length).trim().replace(/;$/, ''));
  assert.deepEqual(embedded, model);
});

test('viewer exposes progressive overview and deep-dive landmarks', async () => {
  const [html, app] = await Promise.all([
    readFile(join(directory, 'index.html'), 'utf8'),
    readFile(join(directory, 'app.js'), 'utf8')
  ]);
  for (const id of ['overviewMode', 'deepMode', 'scopeTree', 'breadcrumbs', 'inspector']) assert.match(html, new RegExp(`id="${id}"`));
  for (const renderer of ['renderOverview', 'renderFlows', 'renderComponents', 'renderFiles', 'renderFunctions', 'renderConnections']) assert.match(app, new RegExp(`function ${renderer}\\(`));
});

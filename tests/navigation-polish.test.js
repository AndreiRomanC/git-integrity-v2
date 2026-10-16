'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '..');
const app = fs.readFileSync(path.join(root, 'frontend', 'app.js'), 'utf8');
const html = fs.readFileSync(path.join(root, 'frontend', 'index.html'), 'utf8');
const css = fs.readFileSync(path.join(root, 'frontend', 'styles.css'), 'utf8');

function extractFunction(name) {
  const start = app.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `${name} must exist`);
  const brace = app.indexOf('{', start);
  let depth = 0;
  for (let index = brace; index < app.length; index++) {
    if (app[index] === '{') depth++;
    else if (app[index] === '}' && --depth === 0) return app.slice(start, index + 1);
  }
  throw new Error(`Could not extract ${name}`);
}

function loadFunction(name) {
  const context = { result: null };
  vm.runInNewContext(`${extractFunction(name)}; result = ${name};`, context);
  return context.result;
}

test('Repository History Enter navigation cycles loaded matches in both directions', () => {
  const cycle = loadFunction('cyclicSearchMatchId');
  assert.equal(cycle(['a', 'b', 'c'], null, 1), 'a');
  assert.equal(cycle(['a', 'b', 'c'], 'a', 1), 'b');
  assert.equal(cycle(['a', 'b', 'c'], 'c', 1), 'a');
  assert.equal(cycle(['a', 'b', 'c'], 'a', -1), 'c');
  assert.match(app, /event\.key === 'Enter' && state\.view === 'graph'/);
  assert.match(app, /goToGraphSearchMatch\(event\.shiftKey \? -1 : 1\)/);
});

test('general file comparison groups consecutive changed rows into navigable blocks', () => {
  const blocks = loadFunction('comparisonBlocks');
  assert.deepEqual(JSON.parse(JSON.stringify(blocks([
    { same: true }, { same: false }, { same: false }, { same: true }, { same: false },
  ]))), [{ start: 1, end: 2 }, { start: 4, end: 4 }]);
  assert.match(html, /id="compareSearch"/);
  assert.match(html, /id="previousCompareDifference" disabled>↑ Previous block/);
  assert.match(app, /activeBlock && rowIndex >= activeBlock\.start && rowIndex <= activeBlock\.end/);
});

test('detached dirty submodule guidance is one compact action row', () => {
  assert.match(app, /class="submodule-guidance"/);
  assert.doesNotMatch(app, /const detachedDirtyBanner/);
  assert.match(app, /Local changes · Detached HEAD/);
});

test('visual polish keeps branch names, graph controls and empty details responsive', () => {
  assert.match(css, /\.branch-row\.local-branch:not\(\.active\) \.branch-row-actions \{[^}]*position: absolute/);
  assert.match(css, /\.main-layout\.details-empty-state:not\(\.details-collapsed\)/);
  assert.match(css, /\.graph-toolbar-row \{/);
  assert.match(app, /class="graph-toolbar-row graph-toolbar-context"/);
  assert.match(app, /class="graph-toolbar-row graph-toolbar-controls"/);
  assert.match(html, /<main class="main-layout details-empty-state">/);
});

test('Full Graph and Branch Story keep one stable toolbar and lane gutter', () => {
  assert.match(app, /const fullGraphPrimary = resolvePrimarySelection\(g\)/);
  assert.match(app, /const widthModel = storyMode \|\| onlySearchMatches/);
  assert.match(app, /class="graph-toolbar-meta"/);
  assert.match(app, /class="graph-toolbar-head"/);
  assert.match(app, /class="graph-toolbar-actions"/);
  assert.match(css, /\.graph-toolbar-context \{[^}]*display: grid;[^}]*grid-template-columns:/);
  assert.match(css, /\.primary-branch-picker \{[^}]*grid-template-columns: 72px minmax\(0, 1fr\)/);
});

test('recent repositories disambiguate duplicate names with their parent folder', () => {
  assert.match(app, /const nameCounts = new Map\(\)/);
  assert.match(app, /\$\{parentName\} \/ \$\{repo\.name\}/);
  assert.match(css, /\.recent-repo-btn \{[^}]*max-width: 156px/);
});

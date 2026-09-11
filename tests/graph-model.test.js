// Automated tests for frontend/graph-model.js — run with:
//   node --test
// Node's test runner and assert module are both built in (Node 18+), so this
// needs no npm install, no node_modules, and no package.json: exactly the
// "no Node.js/npm runtime or build step" the README promises for the app
// itself, unaffected — this is a maintainer-only, opt-in correctness check,
// never invoked by the build or by the shipped app.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { reachableFrom, buildGraphModel } = require('../frontend/graph-model.js');

// Minimal fixture builder — buildGraphModel only ever reads id/parents/refs
// off a commit, so tests only set those, matching how sparse real fixtures
// can be without pretending the shape is smaller than it really is.
function commit(id, parents = [], refs = []) { return { id, parents, refs }; }

// Recomputes, from the *input* alone, every child->parent edge that should
// exist per Git itself: a parent counts only if it is also present in the
// commit list — none of the fixtures used with this helper reference a
// parent outside the loaded window (that specific, separate case — a real,
// expected truncation, not a bug — is exercised directly by its own test
// below instead). Independent of buildGraphModel's own logic on purpose, so
// this is a real check against ground truth, not the implementation
// checking itself.
function expectedEdges(commits) {
  const ids = new Set(commits.map(c => c.id));
  const edges = new Set();
  for (const c of commits) {
    for (const p of new Set(c.parents || [])) { if (ids.has(p)) edges.add(`${c.id}>${p}`); }
  }
  return edges;
}

function actualEdges(model) {
  const edges = new Set();
  for (const node of model) { for (const p of node.parents) edges.add(`${node.commitId}>${p.commitId}`); }
  return edges;
}

// ---- reachableFrom ---------------------------------------------------------

test('reachableFrom returns an empty set for a falsy tip', () => {
  assert.deepEqual(reachableFrom(null, [commit('a')]), new Set());
  assert.deepEqual(reachableFrom(undefined, [commit('a')]), new Set());
  assert.deepEqual(reachableFrom('', [commit('a')]), new Set());
});

test('reachableFrom includes a tip with no parents and nothing else', () => {
  const commits = [commit('a'), commit('b')];
  assert.deepEqual(reachableFrom('a', commits), new Set(['a']));
});

test('reachableFrom walks the full ancestor chain, not just one hop', () => {
  const commits = [commit('c', ['b']), commit('b', ['a']), commit('a', [])];
  assert.deepEqual(reachableFrom('c', commits), new Set(['a', 'b', 'c']));
});

test('reachableFrom walks every parent of a merge, not just the first', () => {
  // m merges b1 and b2, which both descend from a root — both branches must
  // count as "belongs to this history", the same way a real merge into the
  // selected branch pulls in everything it merged, not just its first parent.
  const commits = [
    commit('m', ['b1', 'b2']),
    commit('b1', ['root']),
    commit('b2', ['root']),
    commit('root', []),
  ];
  assert.deepEqual(reachableFrom('m', commits), new Set(['m', 'b1', 'b2', 'root']));
});

test('reachableFrom never includes a sibling that the tip cannot actually reach', () => {
  const commits = [commit('main', ['base']), commit('feature', ['base']), commit('base', [])];
  const set = reachableFrom('main', commits);
  assert.ok(set.has('main') && set.has('base'));
  assert.ok(!set.has('feature'), 'feature only shares an ancestor with main — it is not itself an ancestor');
});

test('reachableFrom does not hang on a malformed cyclic input', () => {
  // Real Git history is never cyclic, but the function's own `set.has`
  // dedupe must make this safe regardless — a defensive property worth
  // locking in given nothing upstream guarantees the shape of `commits`.
  const commits = [commit('a', ['b']), commit('b', ['a'])];
  assert.deepEqual(reachableFrom('a', commits), new Set(['a', 'b']));
});

test('reachableFrom on a tip absent from commits still returns just that id', () => {
  assert.deepEqual(reachableFrom('ghost', [commit('a')]), new Set(['ghost']));
});

// ---- buildGraphModel: edge integrity ---------------------------------------
// This is the property documented in graph-model.js's own "DAG-correctness
// contract" comment, checked here for real instead of by hand-tracing: every
// child->parent relationship that exists in the input, and whose parent is
// also in the input, must appear as exactly one edge in the output — lanes
// are pure presentation and must never drop or duplicate one.

function assertEdgeIntegrity(commits, primaryTipId, label) {
  const model = buildGraphModel(commits, primaryTipId);
  const expected = expectedEdges(commits);
  const actual = actualEdges(model);
  assert.deepEqual(actual, expected, `${label}: rendered edges must match Git's real child->parent edges exactly`);
}

test('edge integrity: a plain linear chain', () => {
  const commits = [commit('c3', ['c2']), commit('c2', ['c1']), commit('c1', [])];
  assertEdgeIntegrity(commits, 'c3', 'linear chain');
});

test('edge integrity: the exact regression this algorithm was rewritten for (main: M-Q-P-A, feature: F-P)', () => {
  // Row order and shape exactly as documented in graph-model.js: a
  // primary-reachable parent (P) that a same-row lane-0 occupant (Q) is
  // still waiting on used to be dropped from a non-primary child's (F's)
  // edge list entirely. See that file's comment for the full trace.
  const commits = [
    commit('M', ['Q']),
    commit('F', ['P']),
    commit('Q', ['P']),
    commit('P', ['A']),
    commit('A', []),
  ];
  const model = buildGraphModel(commits, 'M');
  const edges = actualEdges(model);
  assert.deepEqual(edges, new Set(['M>Q', 'F>P', 'Q>P', 'P>A']), 'every real edge, including F->P, must survive');
  const byId = Object.fromEntries(model.map(n => [n.commitId, n]));
  assert.equal(byId.M.isPrimary, true); assert.equal(byId.Q.isPrimary, true); assert.equal(byId.P.isPrimary, true); assert.equal(byId.A.isPrimary, true);
  assert.equal(byId.F.isPrimary, false, 'F is not reachable from M — it must not be marked primary');
});

test('edge integrity: two branches diverging from a shared base', () => {
  const commits = [
    commit('main2', ['base']),
    commit('feature2', ['base']),
    commit('base', []),
  ];
  assertEdgeIntegrity(commits, 'main2', 'diverging branches');
});

test('edge integrity: an octopus merge (three parents)', () => {
  const commits = [
    commit('merge', ['a', 'b', 'c']),
    commit('a', ['root']), commit('b', ['root']), commit('c', ['root']),
    commit('root', []),
  ];
  assertEdgeIntegrity(commits, 'merge', 'octopus merge');
});

test('edge integrity: multiple unrelated root commits (no shared ancestor at all)', () => {
  const commits = [commit('x2', ['x1']), commit('x1', []), commit('y2', ['y1']), commit('y1', [])];
  assertEdgeIntegrity(commits, 'x2', 'unrelated histories');
});

test('edge integrity: a longer history with repeated forks and merges', () => {
  // A denser graph, closer to real project history, so the property is
  // checked against more simultaneous in-flight lanes than the smaller
  // fixtures above happen to exercise.
  const commits = [
    commit('h9', ['h8', 'h7']),   // merge
    commit('h8', ['h6']),
    commit('h7', ['h5']),
    commit('h6', ['h4']),
    commit('h5', ['h4', 'h3']),   // merge
    commit('h4', ['h2']),
    commit('h3', ['h1']),
    commit('h2', ['h1']),
    commit('h1', []),
  ];
  assertEdgeIntegrity(commits, 'h9', 'dense fork/merge history');
});

test('edge integrity: duplicate parent ids collapse into exactly one edge', () => {
  // Not expected from real Git, but dedupe() exists specifically to guard
  // against ever double-rendering an edge if it happened.
  const commits = [{ id: 'c', parents: ['p', 'p'], refs: [] }, commit('p', [])];
  const model = buildGraphModel(commits, 'c');
  const cNode = model.find(n => n.commitId === 'c');
  assert.equal(cNode.parents.length, 1, 'a duplicated parent id must still produce exactly one edge');
  assert.equal(cNode.parents[0].commitId, 'p');
});

test('edge integrity: a parent outside the loaded window still gets a well-formed, self-consistent entry', () => {
  // A real, expected truncation (the backend's own GRAPH_COMMIT_WINDOW) —
  // "off_window" is deliberately absent from `commits`. buildGraphModel
  // places *every* parent id unconditionally (see its own comment), on
  // purpose: this is what lets the primary chain's lane stay reserved
  // across the truncation boundary, so a later "Load older commits" page
  // continues in the same lane instead of needing to reassign one. The
  // edge line itself is never actually drawn for it — drawGraphOverlay
  // skips any parent id with no rendered row (`positions.get(...)` misses)
  // — so this must never crash and must stay self-consistent, not that the
  // entry disappears.
  const commits = [commit('tip', ['off_window'])];
  const model = buildGraphModel(commits, 'tip');
  assert.equal(model.length, 1);
  assert.equal(model[0].parents.length, 1);
  const [parent] = model[0].parents;
  assert.equal(parent.commitId, 'off_window');
  assert.equal(model[0].after[parent.targetLane], 'off_window', 'targetLane must point at wherever it was actually placed');
});

test('buildGraphModel on an empty commit list returns an empty model', () => {
  assert.deepEqual(buildGraphModel([], 'anything'), []);
});

test('buildGraphModel tolerates a primaryTipId that matches no commit (e.g. a stale ref)', () => {
  const commits = [commit('a', ['b']), commit('b', [])];
  const model = buildGraphModel(commits, 'does-not-exist');
  assert.ok(model.every(n => n.isPrimary === false), 'nothing should be marked primary when the tip itself is not loaded');
  // Lane 0 is an exclusive reservation for real primary-chain content (see
  // graph-model.js's own comment) — with nothing on the primary chain at
  // all, it must stay empty rather than a non-primary row squatting on it.
  assert.ok(model.every(n => n.after[0] == null && n.lane !== 0), 'lane 0 (the index, not a commit id) must stay unused when nothing is actually primary');
  assert.equal(model[0].lane, 1, 'the first (non-primary) commit takes lane 1, leaving lane 0 genuinely reserved and empty');
});

test('commit.refs pass through verbatim, defaulting to an empty array', () => {
  const commits = [commit('a', [], ['main', 'origin/main']), commit('b', [])];
  const model = buildGraphModel(commits, 'a');
  assert.deepEqual(model[0].refs, ['main', 'origin/main']);
  assert.deepEqual(model[1].refs, []);
});

// ---- buildGraphModel: lane-layout sanity -----------------------------------

test('a single linear chain never uses more than one lane', () => {
  const commits = Array.from({ length: 20 }, (_, i) => commit(`c${i}`, i < 19 ? [`c${i + 1}`] : []));
  const model = buildGraphModel(commits, 'c0');
  for (const node of model) {
    assert.equal(node.lane, 0, `${node.commitId} should stay in lane 0 for a purely linear chain`);
    assert.ok(node.after.length <= 1);
  }
});

test('two branches that never converge peak at exactly two simultaneous lanes', () => {
  const commits = [
    commit('main3', ['main2']), commit('main2', ['main1']), commit('main1', []),
    commit('feat3', ['feat2']), commit('feat2', ['feat1']), commit('feat1', []),
  ];
  const model = buildGraphModel(commits, 'main3');
  const maxLanes = Math.max(...model.map(n => n.after.length));
  assert.equal(maxLanes, 2, 'two wholly independent chains need exactly two lanes, never more');
});

test('a lane freed by a merge is reused by a later, unrelated fork instead of growing forever', () => {
  const commits = [
    commit('after', ['merge']),
    commit('merge', ['left', 'right']),
    commit('left', ['base']), commit('right', ['base']), commit('base', []),
    // A second, wholly unrelated pair of commits further down history —
    // should be able to reuse the lane the merge above just freed.
    commit('later_child', ['later_parent']), commit('later_parent', []),
  ];
  const model = buildGraphModel(commits, 'after');
  const maxLanes = Math.max(...model.map(n => n.after.length));
  assert.ok(maxLanes <= 2, `lanes must be reused, not grow unboundedly (peaked at ${maxLanes})`);
});

test('no row ever places the same commit id in two lanes at once', () => {
  const commits = [
    commit('h9', ['h8', 'h7']), commit('h8', ['h6']), commit('h7', ['h5']),
    commit('h6', ['h4']), commit('h5', ['h4', 'h3']), commit('h4', ['h2']),
    commit('h3', ['h1']), commit('h2', ['h1']), commit('h1', []),
  ];
  const model = buildGraphModel(commits, 'h9');
  for (const node of model) {
    const occupied = node.after.filter(id => id != null);
    assert.equal(occupied.length, new Set(occupied).size, `row ${node.row} (${node.commitId}) has a duplicate lane occupant in 'after'`);
  }
});

test('targetLane always points at the lane the parent genuinely occupies in "after"', () => {
  const commits = [
    commit('h9', ['h8', 'h7']), commit('h8', ['h6']), commit('h7', ['h5']),
    commit('h6', ['h4']), commit('h5', ['h4', 'h3']), commit('h4', ['h2']),
    commit('h3', ['h1']), commit('h2', ['h1']), commit('h1', []),
  ];
  const model = buildGraphModel(commits, 'h9');
  for (const node of model) {
    for (const parent of node.parents) {
      assert.equal(node.after[parent.targetLane], parent.commitId, `${node.commitId}'s edge to ${parent.commitId} must point at the lane that commit actually lands in`);
    }
  }
});

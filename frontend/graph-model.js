// ---- Git DAG / topology model -------------------------------------------
// Pulled out of app.js into its own file so it can be loaded two ways with
// no build step either way: as a plain global script in the webview (see
// the <script> tag in index.html, which must load this before app.js) and
// as a plain CommonJS module from Node's built-in test runner (see the
// module.exports guard at the bottom) — the project otherwise has no
// Node.js/npm runtime or build step (README.md), and this keeps it that way;
// only a maintainer's own `node --test` invocation ever touches Node,
// never the shipped app or its build.
//
// Assigns each commit a stable vertical lane. Lanes are never renumbered or
// shifted for unrelated commits — a lane slot is only ever (a) kept as-is,
// (b) reused in place by the first still-unseen parent of the commit that
// currently occupies it, or (c) freed and later reused (first-fit) by a
// later, unrelated fork. This is what keeps a linear branch pinned to one
// lane for its entire visible history instead of drifting row to row.
// Every commit reachable from `primaryTipId` (the selected branch's tip) —
// walking ALL parents, not just first-parent, so a merge into the selected
// branch still counts its merged-in history as "belongs to this branch".
function reachableFrom(tipId, commits) {
  const set = new Set(); if (!tipId) return set;
  const byId = new Map(commits.map(c => [c.id, c]));
  const stack = [tipId];
  while (stack.length) {
    const id = stack.pop(); if (set.has(id)) continue; set.add(id);
    const commit = byId.get(id); if (commit) (commit.parents || []).forEach(p => stack.push(p));
  }
  return set;
}

// Lane 0 is reserved for the selected branch's own ancestry. A commit that
// isn't reachable from it — e.g. another branch's commit made after the two
// diverged — is never placed there, even if it's chronologically newer and
// would otherwise be first in line; it gets its own lane instead, and that
// lane only exists for as long as it actually needs to (freed again right
// after its edge reconnects to the primary chain).
//
// DAG-correctness contract, enforced by graph-model.test.js (see
// "edge integrity" there — run it with `node --test`): for every
// commit and every one of its parents that also appears in `commits`,
// buildGraphModel's output must contain exactly one edge for that
// child→parent pair — lanes are pure presentation and must never cause one
// to be silently dropped. The test suite's own regression case is the exact
// bug this once failed on, kept here too since the trace is what explains
// *why* the code below is shaped the way it is:
//   main:    M → Q → P → A
//   feature: F → P
//   row order: M, F, Q, P, A     (primary = main; F is not on it)
// F's only parent, P, IS on the primary chain (reachable from M) — a
// previous version special-cased "a primary-reachable parent always tries
// lane 0", found lane 0 already held by Q (main's own pending next commit)
// at F's row, and — having been excluded from the *secondary*-parent
// placement path specifically because it WAS the primary parent — was
// placed nowhere at all, so the real F→P edge silently vanished. The
// current version places every parent somewhere unconditionally (lane 0 is
// only ever a *preference*, taken solely when actually free); tracing the
// same case now: F→P lands in lane 1 (F's own row's freed lane), and Q's own
// edge to P — processed next — finds P already sitting in lane 1 and simply
// draws into it, the two lanes correctly converging on the one shared commit
// instead of one of them losing its edge.
function buildGraphModel(commits, primaryTipId) {
  const primarySet = reachableFrom(primaryTipId, commits);
  const lanes = []; // lanes[i] = commitId currently occupying that lane, or null
  if (primarySet.size && commits.some(c => c.id === primaryTipId)) lanes[0] = primaryTipId;
  const dedupe = list => (list || []).filter((id, index, all) => id && all.indexOf(id) === index);

  return commits.map((commit, row) => {
    const isPrimary = primarySet.has(commit.id);
    let lane = lanes.indexOf(commit.id);
    if (lane < 0) {
      if (isPrimary) { lane = lanes[0] == null ? 0 : lanes.indexOf(null); if (lane < 0) lane = lanes.length; }
      else { lane = lanes.length > 1 ? lanes.indexOf(null, 1) : -1; if (lane < 0) lane = Math.max(lanes.length, 1); }
    }
    const before = lanes.slice();
    lanes[lane] = null;

    const parentIds = dedupe(commit.parents);
    const newParents = parentIds.filter(id => !lanes.includes(id));
    // Every entry in `newParents` gets a real lane before this row finishes —
    // no branch below is allowed to fall through without placing one, unlike
    // the previous version, where a primary-reachable parent that lost the
    // race for lane 0 (already occupied by another chain still pending, e.g.
    // main's own next commit) was dropped on the floor entirely: it wasn't
    // in lane 0, and having been claimed as "the primary parent" it was also
    // excluded from the secondary-parent placement loop — so it never made
    // it into `lanes` at all, and the real child→parent edge to it silently
    // vanished from the rendered graph (reproduced with main: M→Q→P→A and
    // feature: F→P, rendered in row order M,F,Q,P,A — F's parent P couldn't
    // claim lane 0 since Q was still waiting there, so F→P used to disappear
    // even though it's a completely real, unbroken Git relationship).
    const remaining = newParents.slice();
    // Preferred, cosmetic-only: a parent reachable from the primary branch
    // claims lane 0, but *only* if lane 0 is actually free right now — this
    // never evicts whatever's already pending there, which would just move
    // the exact same bug onto that commit's edge instead of fixing it.
    if (lanes[0] == null) {
      const primaryIndex = remaining.findIndex(id => primarySet.has(id));
      if (primaryIndex >= 0) { lanes[0] = remaining[primaryIndex]; remaining.splice(primaryIndex, 1); }
    }
    // Also cosmetic-only: this row's own just-vacated lane (when it isn't
    // lane 0) is offered to the next parent so a single-parent continuation
    // draws straight down instead of an unnecessary diagonal.
    if (lane !== 0 && lanes[lane] == null && remaining.length) { lanes[lane] = remaining.shift(); }
    // Everything still unplaced — additional merge parents, or a
    // primary-reachable parent that couldn't claim lane 0 above — gets a
    // real lane unconditionally: reuse a freed one if there is any (never
    // lane 0 here, which stays reserved for whichever chain is already
    // pending in it), otherwise the lane set grows. This is what guarantees
    // two lanes can later converge on the very same commit (e.g. Q's edge to
    // P finding P already placed by F's edge above it) instead of one of
    // them being silently lost.
    for (const id of remaining) {
      let slot = lanes.length > 1 ? lanes.indexOf(null, 1) : -1; if (slot < 0) slot = Math.max(lanes.length, 1);
      lanes[slot] = id;
    }
    while (lanes.length && lanes[lanes.length - 1] == null) lanes.pop();
    const after = lanes.slice();

    return {
      commitId: commit.id, row, lane, before, after, isPrimary,
      parents: parentIds.map(id => ({ commitId: id, targetRow: row + 1, targetLane: after.indexOf(id) })).filter(p => p.targetLane >= 0),
      refs: commit.refs || [],
      type: 'commit',
    };
  });
}

// Node (the test runner only — see the file banner above) sees `module`;
// the webview, loading this as a plain <script>, does not, so the two
// functions above stay ordinary globals there, exactly as if this code was
// still inline in app.js. No bundler, no import/export syntax, either way.
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { reachableFrom, buildGraphModel };
}

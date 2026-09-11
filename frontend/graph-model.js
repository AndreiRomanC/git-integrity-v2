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

// Decides *which* ref badges a commit row actually shows and in what
// order — pure data selection, no DOM, so the badge-ordering/limiting
// rules (History & Branch Map rework, point 3) are checkable the same way
// buildGraphModel's lane assignment is, instead of only by eyeballing the
// rendered graph. refsBadges (app.js) does nothing but turn this
// function's output into markup.
//
// Fixed badge order: HEAD, then the release tag(s), then branches (local
// before remote). Only ever one tag badge is shown directly — a commit
// with several tags shows the first plus an aggregate overflowTagCount,
// never a generic "+N" that hides *which* tag actually matters (release
// tags are the whole reason this project cares about tags in the first
// place). Branches share a separate, small cap (maxBranches) with their
// own overflowBranchCount; if the currently checked-out branch happens to
// be one of this commit's own refs, it always claims one of those slots
// rather than risking getting bumped out by an arbitrary earlier ref —
// losing *that specific* answer ("am I on this branch right now") to a
// generic overflow pill was a real, reported problem with the flat-list
// version this replaced.
function selectRefBadges(refs, options) {
  const opts = options || {};
  const isHead = !!opts.isHead;
  const currentBranchName = opts.currentBranchName || null;
  const maxBranches = opts.maxBranches == null ? 2 : opts.maxBranches;

  const tags = (refs || []).filter(ref => ref.kind === 'tag');
  const branches = (refs || []).filter(ref => ref.kind === 'local_branch' || ref.kind === 'remote_branch');

  const badges = [];
  if (isHead) badges.push({ kind: 'head', name: 'HEAD' });

  // The overflow lists are the *actual* excluded refs (not just a count) —
  // precise enough for a caller to build a real tooltip ("which tags?"),
  // rather than reconstructing a guess from the original ref order.
  if (tags.length) badges.push({ kind: 'tag', name: tags[0].name });
  const overflowTags = tags.slice(1);

  const isCurrent = branch => branch.name === currentBranchName;
  // Current branch first (so it survives the slice below regardless of
  // where it happened to sit in the backend's own ref order), then
  // restored to local-before-remote display order via the stable sort
  // right after — priority decides *survival*, kind decides *order*.
  const prioritized = [...branches.filter(isCurrent), ...branches.filter(branch => !isCurrent(branch))];
  const shown = prioritized.slice(0, maxBranches).sort((a, b) => (a.kind === b.kind ? 0 : a.kind === 'local_branch' ? -1 : 1));
  const shownNames = new Set(shown.map(branch => branch.name));
  for (const branch of shown) badges.push({ kind: branch.kind, name: branch.name });
  const overflowBranches = branches.filter(branch => !shownNames.has(branch.name));

  return {
    badges,
    overflowTags, overflowBranches,
    overflowTagCount: overflowTags.length, overflowBranchCount: overflowBranches.length,
  };
}

// Decides which branch rows the sidebar (renderBranches, app.js) shows and
// which one (if any) is HEAD — pure, so the repository-context bug this
// exists to fix (the sidebar always rendering the *parent's* branches,
// even while a submodule's own Branch Map is active — see
// activeRepositoryContext in app.js, which builds `context` here from
// either state.submoduleGraph or state.repository) is checkable the same
// way the rest of this module already is. context is {branches,
// headDetached, headOid} — deliberately shaped so both the parent and a
// submodule's own resolved repository data fit it identically.
//
// The backend already computes each Branch's own `current` flag correctly
// per-repository (repository.rs: `branch_type == Local && branch_name ==
// current_branch`, and current_branch is always "" on a detached HEAD —
// see RepositoryInfo's own contract) — `!detached` here is deliberate
// belt-and-suspenders on top of that, not a workaround for it: a detached
// checkout must never show a branch marked HEAD even if that guarantee
// ever regressed upstream.
function selectBranchRows(context) {
  const detached = !!(context && context.headDetached);
  const rows = ((context && context.branches) || []).map(branch => ({
    name: branch.name, remote: !!branch.remote, isHead: !detached && !!branch.current,
  }));
  return { rows, detached, detachedAt: (context && context.headOid) || '' };
}

// Node (the test runner only — see the file banner above) sees `module`;
// the webview, loading this as a plain <script>, does not, so the two
// functions above stay ordinary globals there, exactly as if this code was
// still inline in app.js. No bundler, no import/export syntax, either way.
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { reachableFrom, buildGraphModel, selectRefBadges, selectBranchRows };
}

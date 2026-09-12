// ---- Submodule branch/tag selector data shaping ---------------------------
// Pulled out of app.js into its own file, loaded the same two ways
// graph-model.js already is (see that file's own banner for why): as a plain
// global script in the webview, and as a plain CommonJS module from Node's
// built-in test runner. No build step either way.
//
// Submodule-branch-selector report, point 3: "Avoid confusing duplicate
// rows. A local branch and its tracking remote should be visually grouped.
// Put branches existing only on the remote in a separate 'Remote only'
// section." The backend (submodule_versions) already lists local branches
// and remote-tracking branches as two separate flat entries with no relation
// between them beyond a shared commit — this is the pure logic that decides
// which remote-tracking entries are "the same branch, already shown via its
// local upstream" (dropped) versus genuinely remote-only (kept, in their own
// section), so it's checkable the same way graph-model.js's own selection
// functions already are.
//
// A local branch's own `upstream` field (set by submodule_versions,
// "<remote>/<branch>" shorthand) is the only signal used to match it to a
// remote-tracking entry's `name` — never a guess from the branch's own name,
// since a local branch can track a differently-named remote one.
function groupSubmoduleBranchVersions(versions) {
  const all = versions || [];
  const local = all.filter(version => version.kind === 'branch');
  const remote = all.filter(version => version.kind === 'remote');
  const trackedRemoteNames = new Set(local.filter(version => version.upstream).map(version => version.upstream));
  const remoteOnly = remote.filter(version => !trackedRemoteNames.has(version.name));
  return { local, remoteOnly };
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { groupSubmoduleBranchVersions };
}

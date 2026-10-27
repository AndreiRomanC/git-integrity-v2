// Read-only navigation: one in-flight verification per repository/open context.
// No timer, persistent cache, remote fetch or recursive submodule inspection.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.GitDrillDownNavigationRefresh = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';
  function create({ invoke, current, changed = () => {}, refreshed = () => {}, log = () => {} }) {
    const jobs = new Map();
    // Identity only, not another status cache. An externally checked-out child
    // must not reuse a still-young status snapshot from its previous HEAD.
    const verifiedHeads = new Map();
    const key = t => JSON.stringify([t.owner, t.generation, t.repositoryPath, t.rootHead, t.rootBranch]);
    const sameHead = (a, b) => a.head_oid === b.head_oid && a.branch === b.branch;
    function ensure(target) {
      const id = key(target);
      const existing = jobs.get(id);
      if (existing) return existing.promise;
      // Drop obsolete UI bookkeeping; already-running reads still finish, but
      // current() prevents them from publishing into a different repository.
      for (const [oldId, job] of jobs) if (!current(job.target)) jobs.delete(oldId);
      for (const [oldId, value] of verifiedHeads) if (!current(value.target)) verifiedHeads.delete(oldId);
      const job = { target, phase: 'checking', error: '', promise: null, refreshStartedAt: null };
      const started = Date.now();
      jobs.set(id, job);
      job.promise = Promise.resolve().then(async () => {
        const before = await invoke('navigation_status', { repositoryPath: target.repositoryPath });
        if (!current(target)) return { obsolete: true };
        if (target.expectedHead && !sameHead(before, target.expectedHead)) {
          throw new Error('The repository branch or HEAD changed outside the app. Use Refresh repository before continuing.');
        }
        const forceFresh = !target.expectedHead && !sameHead(before, verifiedHeads.get(id)?.head || {});
        if (before.fresh && !forceFresh) return { refreshed: false };
        job.phase = 'refreshing'; job.refreshStartedAt = Date.now(); changed(); log('started', target, Date.now() - started);
        // Uses the same backend single-flight status scan and mutation epochs
        // as Working tree/Refresh; navigation clicks join, never queue reruns.
        const changes = await invoke('refresh_navigation_status', {
          repositoryPath: target.repositoryPath, headOid: before.head_oid, branch: before.branch, forceFresh,
        });
        if (!current(target)) return { obsolete: true };
        await refreshed(target, changes);
        verifiedHeads.set(id, { target, head: before });
        log('completed', target, Date.now() - started);
        return { refreshed: true };
      }).catch(error => {
        if (current(target)) {
          job.phase = 'error'; job.error = String(error?.message || error);
          log('failed', target, Date.now() - started);
        }
        throw error;
      }).finally(() => {
        if (job.phase !== 'error' || !current(target)) {
          if (jobs.get(id) === job) jobs.delete(id);
        }
        changed();
      });
      changed();
      return job.promise;
    }
    return {
      ensure,
      states: () => [...jobs.values()].filter(job => current(job.target)),
      retry: () => { for (const [id, job] of jobs) if (job.phase === 'error') jobs.delete(id); changed(); },
    };
  }
  return { create };
});

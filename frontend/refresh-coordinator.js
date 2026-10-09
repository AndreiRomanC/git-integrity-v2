// Small, Git-agnostic single-flight coordinator for frontend refresh work.
//
// A request that arrives during an active refresh is never discarded. It is
// merged into one pending rerun, and every caller waits for the final result.
// The first result is deliberately not applied when a newer request exists:
// a repository mutation may have made that result stale while it was running.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.GitDrillDownRefresh = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  function createRefreshCoordinator({ execute, apply, mergeScope, describeScope, log } = {}) {
    if (typeof execute !== 'function') throw new TypeError('execute must be a function');
    const applyResult = typeof apply === 'function' ? apply : async () => {};
    const merge = typeof mergeScope === 'function' ? mergeScope : ((left, right) => right ?? left);
    const describe = typeof describeScope === 'function' ? describeScope : (scope => String(scope ?? 'default'));
    const writeLog = typeof log === 'function' ? log : () => {};
    const active = new Map();

    function emit(event, details = {}) {
      try { writeLog(event, details); } catch { /* diagnostics must never break refresh */ }
    }

    async function drain(key, entry) {
      let finalResult;
      while (true) {
        const scope = entry.nextScope;
        entry.nextScope = null;
        emit('started', { scope: describe(scope) });
        try {
          finalResult = await execute(key, scope);
        } catch (error) {
          emit('failed', { scope: describe(scope), error: String(error) });
          if (entry.nextScope !== null) {
            entry.nextScope = merge(scope, entry.nextScope);
            emit('rerun', { scope: describe(entry.nextScope), recovery: true });
            continue;
          }
          active.delete(key);
          entry.reject(error);
          return;
        }

        // A newer request arrived while the refresh was in flight. Its
        // existence is enough to make this result non-authoritative for the
        // UI; keep the strongest scope and run once more.
        if (entry.nextScope !== null) {
          entry.nextScope = merge(scope, entry.nextScope);
          emit('completed', { scope: describe(scope), applied: false, pending: true });
          emit('rerun', { scope: describe(entry.nextScope), recovery: false });
          continue;
        }

        let applied;
        try {
          // The integration may reject an obsolete repository/open context.
          // Do not report that ignored response as an applied UI update.
          applied = (await applyResult(key, finalResult, entry.aggregateScope)) !== false;
        } catch (error) {
          emit('apply-failed', { scope: describe(entry.aggregateScope), error: String(error) });
          if (entry.nextScope !== null) {
            entry.nextScope = merge(entry.aggregateScope, entry.nextScope);
            emit('rerun', { scope: describe(entry.nextScope), recovery: true });
            continue;
          }
          active.delete(key);
          entry.reject(error);
          return;
        }

        // Applying a result can itself await a folder repaint. A request that
        // arrived during that await is still a real newer request and receives
        // the same one-rerun treatment.
        if (entry.nextScope !== null) {
          entry.nextScope = merge(entry.aggregateScope, entry.nextScope);
          emit('completed', { scope: describe(entry.aggregateScope), applied, pending: true });
          emit('rerun', { scope: describe(entry.nextScope), recovery: false });
          continue;
        }

        active.delete(key);
        emit('completed', { scope: describe(entry.aggregateScope), applied, pending: false });
        entry.resolve(finalResult);
        return;
      }
    }

    function request(key, scope) {
      if (!key) return Promise.reject(new Error('refresh key is required'));
      emit('requested', { scope: describe(scope) });
      const existing = active.get(key);
      if (existing) {
        const previous = existing.nextScope;
        existing.nextScope = previous === null ? scope : merge(previous, scope);
        const oldAggregate = describe(existing.aggregateScope);
        existing.aggregateScope = merge(existing.aggregateScope, scope);
        const nextAggregate = describe(existing.aggregateScope);
        emit('coalesced', { scope: describe(scope), pendingScope: describe(existing.nextScope) });
        if (oldAggregate !== nextAggregate) emit('upgraded', { from: oldAggregate, to: nextAggregate });
        return existing.promise;
      }

      let resolve;
      let reject;
      const promise = new Promise((resolvePromise, rejectPromise) => {
        resolve = resolvePromise;
        reject = rejectPromise;
      });
      const entry = { nextScope: scope, aggregateScope: scope, promise, resolve, reject };
      active.set(key, entry);
      void drain(key, entry);
      return promise;
    }

    return {
      request,
      isRefreshing: key => active.has(key),
      activeCount: () => active.size,
    };
  }

  return { createRefreshCoordinator };
});

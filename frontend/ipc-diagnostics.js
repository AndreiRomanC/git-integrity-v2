// Passive clocks for selected requests. No Git calls, result mutation or
// refresh scheduling. The watchdog exists only while a sampled call is pending.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.GitDrillDownIpcDiagnostics = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';
  function create({ log, document: doc = globalThis.document, window: win = globalThis,
    now = () => performance.now(), wall = () => Date.now(),
    later = (fn, ms) => setTimeout(fn, ms), cancel = id => clearTimeout(id),
    frame = fn => win.requestAnimationFrame(fn) } = {}) {
    const pending = new Map(), results = new WeakMap(), completed = new Map();
    let timer = null, lastPoll = -Infinity, expected = 0, wallExpected = 0;
    const state = () => `visibility=${doc?.visibilityState === 'visible' ? 'visible' : doc?.visibilityState === 'hidden' ? 'hidden' : 'unknown'} focused=${typeof doc?.hasFocus === 'function' ? doc.hasFocus() : 'unknown'}`;
    function emit(item, event, extra = '') {
      try { log(`ipc_delivery: request=${item.id} command=${item.command} event=${event} unix_ms=${wall()} ${state()}${extra ? ' ' + extra : ''}`, Math.max(0, now() - item.started)); } catch { /* diagnostic only */ }
    }
    function watch() {
      if (timer !== null || !pending.size) return;
      expected = now() + 1000; wallExpected = wall() + 1000;
      timer = later(() => {
        timer = null;
        for (const item of pending.values()) gap(item);
        watch();
      }, 1000);
    }
    function gap(item) {
      const lag = now() - expected, wallLag = wall() - wallExpected;
      if ((lag >= 1500 || wallLag >= 1500) && item.gaps++ < 8) emit(item, 'event_loop_gap', `lag_ms=${Math.round(lag)} wall_lag_ms=${Math.round(wallLag)} cause=unknown`);
    }
    for (const [target, events] of [[doc, ['visibilitychange']], [win, ['focus', 'blur', 'pageshow', 'pagehide']]]) {
      for (const event of events) target?.addEventListener?.(event, () => {
        for (const item of pending.values()) if (item.windowEvents++ < 12) emit(item, 'window_state', `trigger=${event}`);
      });
    }
    const api = {
      wants(command) {
        if (command === 'refresh_status' || command === 'publish_branch') return true;
        // Footer polling is existing behaviour. Sample it, never add polling.
        if (command === 'active_git_commands' && now() - lastPoll >= 10000) { lastPoll = now(); return true; }
        return false;
      },
      start(command, id) {
        completed.delete(command);
        const item = { command, id, started: now(), wallStarted: wall(), gaps: 0, windowEvents: 0 };
        pending.set(id, item); emit(item, 'frontend_dispatch'); watch(); return item;
      },
      finish(item, success, result) {
        if (!item || !pending.delete(item.id)) return;
        // A resolved Promise can run before a delayed timer after suspension.
        // Record the missed tick before cancellation loses that evidence.
        if (timer !== null) gap(item);
        if (!pending.size && timer !== null) { cancel(timer); timer = null; }
        emit(item, 'frontend_received', `outcome=${success ? 'success' : 'error'} wall_elapsed_ms=${wall() - item.wallStarted}`);
        if (success && item.command === 'publish_branch') completed.set(item.command, item);
        if (result && typeof result === 'object') results.set(result, item);
        // This measures scheduling, NOT proof the response has been painted.
        // Do not queue animation callbacks indefinitely in a hidden window.
        if (doc?.visibilityState === 'visible') {
          let done = false;
          const expiry = later(() => { if (!done) { done = true; emit(item, 'next_frame_unobserved'); } }, 5000);
          frame(() => { if (!done) { done = true; cancel(expiry); emit(item, 'next_frame', 'content_painted=not_proven'); } });
        } else emit(item, 'next_frame_skipped', 'reason=not_visible');
      },
      applied(result) {
        const item = result && typeof result === 'object' && results.get(result);
        if (item) { results.delete(result); emit(item, 'status_dom_applied', 'content_painted=not_proven'); }
      },
      publishApplied() {
        const item = completed.get('publish_branch');
        if (item) { completed.delete('publish_branch'); emit(item, 'publish_dom_applied', 'content_painted=not_proven'); }
      },
    };
    // A failed clock/observer/logger must never reject a successful operation.
    return Object.fromEntries(Object.entries(api).map(([name, fn]) => [name, (...args) => {
      try { return fn(...args); } catch { return undefined; }
    }]));
  }
  return { create };
});

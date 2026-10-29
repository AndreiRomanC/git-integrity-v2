const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { create } = require('../frontend/ipc-diagnostics.js');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');

function harness() {
  let time = 0, timestamp = 10000, sequence = 0;
  const timers = new Map(), frames = [], logs = [], events = {};
  const doc = { visibilityState: 'visible', hasFocus: () => true, addEventListener: (name, fn) => { events[name] = fn; } };
  const win = { addEventListener: doc.addEventListener };
  const options = { log: (label, ms) => logs.push({label, ms}), document: doc, window: win,
    now: () => time, wall: () => timestamp,
    later: (fn, delay) => { const id = ++sequence; timers.set(id, {fn, delay}); return id; },
    cancel: id => timers.delete(id), frame: fn => frames.push(fn) };
  const diagnostic = create(options);
  return { diagnostic, options, timers, frames, logs, events, doc,
    advance(ms, wallMs = ms) { time += ms; timestamp += wallMs; },
    fire() { const [id, timer] = timers.entries().next().value; timers.delete(id); timer.fn(); } };
}

test('idle instrumentation does not poll, refresh or log; footer sampling is bounded', () => {
  const h = harness();
  assert.equal(h.timers.size, 0); h.events.blur(); assert.equal(h.logs.length, 0);
  assert.equal(h.diagnostic.wants('active_git_commands'), true);
  assert.equal(h.diagnostic.wants('active_git_commands'), false);
  h.advance(10000); assert.equal(h.diagnostic.wants('active_git_commands'), true);
  assert.equal(h.diagnostic.wants('frontend_perf_log'), false);
  assert.equal(h.diagnostic.wants('refresh_status'), true);
  assert.equal(h.diagnostic.wants('publish_branch'), true);
});

test('correlation keeps result identity and never reads arguments, result fields or errors', () => {
  const h = harness();
  const result = Object.freeze({ get secret() { assert.fail('must not inspect results'); } });
  const item = h.diagnostic.start('refresh_status', 12);
  for (let i=0; i<6; i++) { h.advance(1000); h.fire(); }
  h.diagnostic.finish(item, true, result);
  h.advance(20); h.diagnostic.applied(result); h.frames.shift()();
  assert.equal(h.timers.size, 0);
  assert.deepEqual(h.logs.map(x=>x.label.match(/event=(\S+)/)[1]), ['frontend_dispatch','frontend_received','status_dom_applied','next_frame']);
  assert.ok(h.logs.every(x=>x.label.includes('request=12 command=refresh_status') && x.label.includes('unix_ms=')));
  assert.match(h.logs[1].label, /wall_elapsed_ms=6000/);
  assert.match(h.logs.at(-1).label, /content_painted=not_proven/);
  assert.equal(h.logs[1].ms, 6000);
  h.diagnostic.applied(result); assert.equal(h.logs.length, 4, 'DOM marker only once per result');
});

test('clock gaps and hidden windows are recorded without claiming a freeze or forcing rendering', () => {
  const h = harness(), item = h.diagnostic.start('publish_branch', 1);
  h.doc.visibilityState = 'hidden'; h.events.visibilitychange();
  h.advance(1000, 157000); h.fire();
  assert.ok(h.logs.some(x=>/event=event_loop_gap/.test(x.label) && /cause=unknown/.test(x.label)));
  h.diagnostic.finish(item, false);
  assert.equal(h.frames.length, 0); assert.equal(h.timers.size, 0);
  assert.match(h.logs.at(-1).label, /event=next_frame_skipped.*reason=not_visible/);
  h.events.focus(); assert.equal(h.timers.size, 0);
});

test('response microtask arriving before the overdue watchdog still records the scheduling gap', () => {
  const h = harness(), item = h.diagnostic.start('refresh_status', 1);
  h.advance(157000); h.diagnostic.finish(item, true, []);
  assert.ok(h.logs.some(x=>/event=event_loop_gap.*lag_ms=156000/.test(x.label)));
  h.frames.shift()(); assert.equal(h.timers.size, 0);
});

test('many concurrent requests share one watchdog and stale frame callbacks do not log indefinitely', () => {
  const h = harness(), a = h.diagnostic.start('refresh_status', 1), b = h.diagnostic.start('publish_branch', 2);
  assert.equal(h.timers.size, 1);
  h.diagnostic.finish(a, true, []); h.frames.shift()();
  assert.equal(h.timers.size, 1);
  h.diagnostic.finish(b, true); assert.equal(h.timers.size, 1);
  h.advance(5000); h.fire(); assert.match(h.logs.at(-1).label, /next_frame_unobserved/);
  const count = h.logs.length; h.frames.shift()(); assert.equal(h.logs.length, count);
  assert.equal(h.timers.size, 0);
});

test('real invoke wrapper adds only optional correlation; same response and error reach callers', async () => {
  const h = harness(), calls = [], state = { statusReady: true };
  const context = { state, activeStagingOperation: null, pendingToggleFlight: null, pendingToggles: new Map(),
    performance, jsPerfLog() {}, status() {}, window: {
      GitDrillDownIpcDiagnostics: { create: () => h.diagnostic },
      __TAURI__: {core: {invoke: (command,args) => new Promise((resolve,reject)=>calls.push({command,args,resolve,reject}))}},
    } };
  vm.createContext(context); vm.runInContext(app.slice(0,app.indexOf('const $ =')) + '\nglobalThis.call=invoke;', context);
  const args = Object.freeze({repositoryPath:'PRIVATE_PATH', accessToken:'PRIVATE_TOKEN'}), result = [];
  const request = context.call('refresh_status', args);
  assert.equal(calls[0].args.diagnosticRequestId, 1); assert.equal(args.diagnosticRequestId, undefined);
  calls[0].resolve(result); assert.equal(await request, result);
  const failure = context.call('refresh_status', args), error = new Error('PRIVATE_ERROR');
  calls[1].reject(error); await assert.rejects(failure, value => value === error);
  assert.doesNotMatch(JSON.stringify(h.logs), /PRIVATE/);
  const logging = context.call('frontend_perf_log', {label:'x'}); calls[2].resolve(); await logging;
  assert.equal(calls[2].args.diagnosticRequestId, undefined);
  assert.equal(calls.length, 3, 'no hidden Git/diagnostic probes');
});

test('publish DOM marker is emitted once and diagnostic failures never change an operation outcome', () => {
  const h = harness(), item = h.diagnostic.start('publish_branch', 7);
  h.diagnostic.finish(item, true); h.diagnostic.publishApplied(); h.diagnostic.publishApplied();
  assert.equal(h.logs.filter(x=>/event=publish_dom_applied/.test(x.label)).length, 1);
  const broken = create({...h.options, log() { throw Error('logger unavailable'); }, frame() { throw Error('frame unavailable'); }});
  assert.doesNotThrow(() => {
    const token = broken.start('refresh_status', 8);
    broken.finish(token, true, []);
  });
});

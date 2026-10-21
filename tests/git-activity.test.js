const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const GitActivity = require('../frontend/git-activity.js');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const turn = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const element = () => {
    const classes = new Set();
    return { textContent: '', title: '', hidden: true, disabled: true, listeners: {},
      addEventListener(event, fn) { this.listeners[event] = fn; },
      classList: { contains: c => classes.has(c), add: (...cs) => cs.forEach(c => classes.add(c)), remove: (...cs) => cs.forEach(c => classes.delete(c)), toggle(c, yes) { if (yes) classes.add(c); else classes.delete(c); } } };
  };
  const refs = Object.fromEntries(['statusCommandHint', 'statusStopCommand', 'consoleStopCommand', 'statusFooter', 'statusDot'].map(name => [name, element()]));
  const calls = [], warnings = [], toasts = [];
  let poll, active = [], responder, confirmation = true;
  const context = {
    refs, GitActivity, inFlightBackendRequests: 0,
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (responder) return responder(command, args);
      if (command === 'active_git_commands') return active;
    },
    customConfirm: async (text, options) => { warnings.push({ text, options }); return confirmation; },
    showOperationToast: (...args) => toasts.push(args),
    esc: value => String(value), setInterval: (fn, ms) => { assert.equal(ms, 750); poll = fn; },
  };
  vm.createContext(context);
  vm.runInContext(app.slice(app.indexOf('let commandHintRefreshGeneration ='), app.indexOf('// Keep progress attached')), context);
  return { context, refs, calls, warnings, toasts, poll: () => poll(),
    active: rows => { active = rows; }, respond: fn => { responder = fn; }, confirm: value => { confirmation = value; } };
}
const record = (id = 1, extras = {}) => ({
  id, repo_hint: 'module', command: 'git submodule update --init', started_at_ms: new Date(2026, 9, 20, 9, 14, 5).getTime(),
  seconds_ago: 2, running: true, stoppable: true, stop_requested: false, ...extras,
});

test('commit and process clocks use real timestamps in local time, including epoch zero', () => {
  const date = new Date(2026, 9, 20, 9, 14, 5);
  const formatted = GitActivity.commitTime({ timestamp: date.getTime() / 1000, date: 'wrong date' });
  assert.equal(formatted.date, '2026-10-20');
  assert.equal(formatted.time, '09:14');
  assert.equal(formatted.iso, date.toISOString());
  assert.match(formatted.title, /local timezone/);
  assert.equal(GitActivity.clock(date.getTime()), '09:14:05');
  assert.equal(GitActivity.commitTime({ timestamp: 0 }).iso, '1970-01-01T00:00:00.000Z');
  for (const timestamp of [undefined, null, NaN, Infinity, '123']) {
    const cached = GitActivity.commitTime({ date: '2026-10-20', timestamp });
    assert.equal(cached.date, '2026-10-20');
    assert.equal(cached.time, '', 'old cache must not invent midnight');
  }
  assert.equal(GitActivity.clock(undefined), 'time unavailable');
});

test('footer shows start time, actual command and concurrent job count, then clears finished activity', async () => {
  const h = harness();
  h.active([record(2), record(1, { repo_hint: 'parent' })]);
  await h.context.refreshCommandHint();
  assert.match(h.refs.statusCommandHint.textContent, /^09:14:05 · module: git submodule update --init · \+1 running$/);
  assert.match(h.refs.statusCommandHint.title, /parent: git/);
  assert.equal(h.refs.statusCommandHint.hidden, false);
  assert.equal(h.refs.statusStopCommand.disabled, false);
  assert.equal(h.refs.consoleStopCommand.hidden, false, 'Stop remains accessible inside the modal Terminal');
  assert.equal(h.refs.consoleStopCommand.disabled, false);
  h.active([]);
  await h.context.refreshCommandHint();
  assert.equal(h.refs.statusCommandHint.hidden, true);
  assert.equal(h.refs.statusStopCommand.hidden, true);
  assert.equal(h.refs.consoleStopCommand.hidden, true);
  assert.equal(h.refs.statusCommandHint.textContent, '');
  assert.ok(h.calls.every(call => call.command === 'active_git_commands'), 'no status scan or full command output polling');
});

test('unsupported workflows cannot be stopped; pending stop cannot be requested twice', async () => {
  const h = harness();
  h.active([record(1, { stoppable: false })]);
  await h.context.refreshCommandHint();
  await h.context.stopFooterGitCommand();
  assert.equal(h.refs.statusStopCommand.disabled, true);
  assert.equal(h.warnings.length, 0);
  h.active([record(1, { stop_requested: true })]);
  await h.context.refreshCommandHint();
  assert.equal(h.refs.statusStopCommand.textContent, 'Stopping…');
  await h.context.stopFooterGitCommand();
  assert.equal(h.warnings.length, 0);
});

test('background inspection does not hide the running update or redirect its Stop', async () => {
  const h = harness();
  h.active([record(3, { command: 'git status', stoppable: false }), record(2)]);
  await h.context.refreshCommandHint();
  assert.match(h.refs.statusCommandHint.textContent, /git submodule update/);
  assert.equal(h.refs.statusStopCommand.disabled, false);
  await h.context.stopFooterGitCommand();
  assert.equal(h.calls.find(call => call.command === 'stop_git_command').args.commandId, 2);
});

test('Stop requires a warning and cancellation performs no operation', async () => {
  const h = harness(); h.active([record()]); h.confirm(false);
  await h.context.refreshCommandHint();
  await h.context.stopFooterGitCommand();
  assert.equal(h.warnings.length, 1);
  assert.equal(h.warnings[0].options.danger, true);
  assert.match(h.warnings[0].text, /does NOT undo completed changes/);
  assert.match(h.warnings[0].text, /server may already have accepted a push/);
  assert.match(h.warnings[0].text, /09:14:05/);
  assert.ok(!h.calls.some(call => call.command === 'stop_git_command'));
});

test('a command finishing during confirmation never causes the next command to be stopped', async () => {
  const h = harness(); const answer = deferred();
  h.active([record(11)]); h.confirm(answer.promise);
  await h.context.refreshCommandHint();
  const stopping = h.context.stopFooterGitCommand();
  h.active([record(12)]);
  await h.context.refreshCommandHint();
  assert.equal(h.refs.statusStopCommand.disabled, true, 'confirmation prevents a second Stop');
  h.respond(async (command, args) => {
    if (command === 'active_git_commands') return [record(12)];
    assert.equal(command, 'stop_git_command');
    assert.equal(args.commandId, 11);
    throw new Error('This Git command has already finished. No other command was stopped.');
  });
  answer.resolve(true); await stopping;
  const stops = h.calls.filter(call => call.command === 'stop_git_command');
  assert.equal(stops.length, 1);
  assert.equal(stops[0].args.commandId, 11);
  assert.match(h.toasts.at(-1)[0], /already finished/);
});

test('activity requests are single-flight, Terminal is observed without busy status, and idle stops polling', async () => {
  const h = harness(); const reply = deferred();
  h.poll(); assert.equal(h.calls.length, 0);
  h.context.inFlightBackendRequests = 1;
  h.respond(() => reply.promise);
  h.poll(); h.poll();
  assert.equal(h.calls.length, 1);
  reply.resolve([record()]); await turn();
  h.context.inFlightBackendRequests = 0;
  h.respond(() => []);
  h.poll(); await turn();
  assert.equal(h.calls.length, 2, 'one final check clears a completed job');
  h.poll(); assert.equal(h.calls.length, 2);
});

test('failed activity metadata disables stale Stop and a later request can recover', async () => {
  const h = harness(); h.active([record()]);
  await h.context.refreshCommandHint();
  h.respond(() => { throw new Error('IPC failure'); });
  await h.context.refreshCommandHint();
  assert.equal(h.refs.statusStopCommand.hidden, true);
  await h.context.stopFooterGitCommand();
  assert.equal(h.warnings.length, 0);
  h.respond(() => [record(2)]);
  await h.context.refreshCommandHint();
  assert.equal(h.refs.statusStopCommand.disabled, false);
});

test('clock helper is loaded before app and full command history retains time and output', () => {
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  assert.ok(html.indexOf('src="git-activity.js') < html.indexOf('src="app.js'));
  const h = harness();
  const row = h.context.commandHistoryRowHtml({ ...record(), running: false, success: false, duration_ms: 2000, stdout: 'partial output', error: 'stopped by user' });
  assert.match(row, /09:14:05/);
  assert.match(row, /partial output/);
  assert.match(row, /stopped by user/);
});

test('saved actions do not proceed to a later command after user interruption', async () => {
  const commands = [], messages = [];
  const context = {
    normalizeSavedAction: value => value, setConsoleMode: () => {},
    status: text => messages.push(text),
    runTerminalFromConsole: async command => { commands.push(command); return { success: false, stderr: 'Terminal command stopped by user.' }; },
  };
  vm.createContext(context);
  vm.runInContext(app.match(/async function runSavedAction\(action\) \{[\s\S]*?\n\}/)[0], context);
  const result = await context.runSavedAction({ name: 'Update', commands: ['git submodule update --init', 'git add .'] });
  assert.equal(result.success, false);
  assert.deepEqual(commands, ['git submodule update --init']);
  assert.match(messages.at(-1), /stopped/);
});

test('high-level libgit2 actions show their start clock even without a CLI subprocess', () => {
  const h = harness();
  h.context.refs.statusText = { textContent: '', title: '' };
  h.context.footerStatusResetTimer = null;
  h.context.clearTimeout = () => {};
  h.context.setTimeout = () => 1;
  vm.runInContext(app.match(/function status\(message, kind = ''\) \{[\s\S]*?\n\}/)[0], h.context);
  h.context.status('Committing changes…', 'busy');
  assert.match(h.refs.statusText.textContent, /^\d{2}:\d{2}:\d{2} · Committing changes…$/);
  h.context.status('Commit completed');
  assert.equal(h.refs.statusText.textContent, 'Commit completed');
});

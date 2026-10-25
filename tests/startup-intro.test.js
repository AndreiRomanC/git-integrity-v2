const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createStartupIntro } = require('../frontend/startup-intro');

const rootPath = path.join(__dirname, '..');
const source = fs.readFileSync(path.join(rootPath, 'frontend/startup-intro.js'), 'utf8');
const app = fs.readFileSync(path.join(rootPath, 'frontend/app.js'), 'utf8');
const html = fs.readFileSync(path.join(rootPath, 'frontend/index.html'), 'utf8');
const css = fs.readFileSync(path.join(rootPath, 'frontend/styles.css'), 'utf8');

function harness(missing) {
  let reloads = 0;
  let sequence = 0;
  const listeners = new Map();
  const frames = new Map();
  const timers = new Map();
  const nodes = Object.fromEntries(['startupIntro', 'startupIntroPhase', 'startupIntroProgress', 'startupIntroFailure', 'startupIntroRetry'].map(id => [id, {
    hidden: id === 'startupIntro' || id === 'startupIntroFailure', dataset: {}, attrs: {}, textContent: '',
    setAttribute(name, value) { this.attrs[name] = value; },
    addEventListener(type, fn) { this[type] = fn; },
    removeEventListener(type, fn) { if (this[type] === fn) delete this[type]; },
  }]));
  const host = {
    addEventListener: (type, fn, capture) => listeners.set(type, { fn, capture }),
    removeEventListener(type, fn, capture) {
      const previous = listeners.get(type);
      assert.equal(previous.fn, fn); assert.equal(previous.capture, capture);
      listeners.delete(type);
    },
    location: { reload() { reloads++; } },
    requestAnimationFrame(fn) { const id = ++sequence; frames.set(id, fn); return id; },
    cancelAnimationFrame: id => frames.delete(id),
    setTimeout(fn, ms) { const id = ++sequence; timers.set(id, { fn, ms }); return id; },
    clearTimeout: id => timers.delete(id),
  };
  const intro = createStartupIntro({ getElementById: id => id === missing ? null : nodes[id] }, host);
  return { intro, nodes, host, listeners, frames, timers, reloads: () => reloads,
    emit: (type, event = {}) => listeners.get(type)?.fn(event),
    paint() { const pending = [...frames.values()]; frames.clear(); pending.forEach(fn => fn()); },
    finishExit() { nodes.startupIntro.animationend?.({ target: nodes.startupIntro, animationName: 'startup-exit' }); },
  };
}

test('fast startup becomes ready synchronously but leaves a paint before its decorative exit', () => {
  const h = harness();
  assert.equal(h.nodes.startupIntro.hidden, false);
  assert.equal(h.nodes.startupIntroFailure.hidden, true);
  h.intro.preparing();
  assert.equal(h.nodes.startupIntroPhase.textContent, 'Preparing workspace…');
  assert.equal(h.nodes.startupIntroProgress.attrs['aria-valuetext'], 'Preparing workspace');
  assert.equal(h.nodes.startupIntroProgress.attrs['aria-valuenow'], undefined);
  assert.equal(h.intro.ready(), undefined, 'no promise or wait gates application initialization');
  assert.equal(h.nodes.startupIntro.hidden, false, 'do not remove the intro before the first paint');
  assert.equal(h.nodes.startupIntro.dataset.state, 'ready');
  assert.equal(h.nodes.startupIntroPhase.textContent, 'Ready');
  assert.equal(h.nodes.startupIntroProgress.attrs['aria-valuetext'], 'Interface ready');
  assert.equal(h.listeners.size, 0);
  assert.equal(h.nodes.startupIntroRetry.click, undefined);
  assert.equal(h.reloads(), 0);
  assert.equal(h.frames.size, 1);
  assert.equal(h.timers.size, 0, 'no minimum display duration');
  h.paint();
  assert.equal(h.nodes.startupIntro.dataset.exiting, undefined, 'first rAF runs before the first paint');
  h.paint();
  assert.equal(h.nodes.startupIntro.dataset.exiting, 'true');
  assert.equal(h.frames.size, 0);
  h.finishExit();
  assert.equal(h.nodes.startupIntro.hidden, true);
  assert.equal(h.nodes.startupIntro.animationend, undefined);
  assert.equal(h.timers.size, 0);
});

test('a completed intro never reappears on subsequent renders or errors', () => {
  const h = harness();
  const lateError = h.listeners.get('error').fn;
  h.intro.ready(); h.intro.preparing(); h.intro.ready();
  lateError({ target: h.host });
  h.emit('unhandledrejection');
  assert.equal(h.frames.size, 1, 'repeated readiness cannot schedule multiple exits');
  h.paint(); h.paint(); h.finishExit();
  h.intro.preparing(); h.intro.ready();
  assert.equal(h.nodes.startupIntro.hidden, true);
  assert.equal(h.nodes.startupIntroFailure.hidden, true);
  assert.equal(h.listeners.size, 0);
});

test('only the root exit completes dismissal; interrupted animation has bounded cleanup', () => {
  const h = harness();
  h.intro.ready(); h.paint(); h.paint();
  h.nodes.startupIntro.animationend({ target: h.nodes.startupIntroProgress, animationName: 'startup-progress' });
  assert.equal(h.nodes.startupIntro.hidden, false);
  const [fallback] = h.timers.values();
  assert.equal(fallback.ms, 500);
  fallback.fn();
  assert.equal(h.nodes.startupIntro.hidden, true);
  assert.equal(h.timers.size, 0);
  assert.equal(h.nodes.startupIntro.animationend, undefined);
});

test('without frame support readiness still exposes the app immediately', () => {
  const h = harness();
  delete h.host.requestAnimationFrame;
  h.intro.ready();
  assert.equal(h.nodes.startupIntro.hidden, true);
  assert.equal(h.listeners.size, 0);
  assert.equal(h.timers.size, 0);
});

test('startup errors stop the bar and offer an explicit retry instead of pretending to finish', () => {
  for (const [type, script] of [['error', false], ['error', true], ['unhandledrejection', false]]) {
    const h = harness();
    h.nodes.startupIntroRetry.click(); assert.equal(h.reloads(), 0);
    h.intro.preparing();
    h.emit(type, { target: script ? { tagName: 'SCRIPT' } : h.host });
    assert.equal(h.nodes.startupIntroProgress.hidden, true);
    assert.equal(h.nodes.startupIntroFailure.hidden, false);
    assert.match(h.nodes.startupIntroPhase.textContent, /could not finish/);
    assert.equal(h.listeners.size, 0);
    assert.equal(h.frames.size, 0);
    assert.equal(h.timers.size, 0);
    h.intro.ready(); h.intro.preparing();
    assert.equal(h.nodes.startupIntro.hidden, false, 'do not claim success after an initialization failure');
    h.nodes.startupIntroRetry.click(); assert.equal(h.reloads(), 1);
  }
});

test('missing images cannot hold the app behind an intro; missing bootstrap markup is harmless', () => {
  const h = harness();
  h.emit('error', { target: { tagName: 'IMG' } });
  assert.equal(h.nodes.startupIntroFailure.hidden, true);
  h.intro.ready(); h.paint(); h.paint(); h.finishExit();
  assert.equal(h.nodes.startupIntro.hidden, true);
  for (const missing of Object.keys(h.nodes)) {
    const absent = harness(missing);
    absent.intro.preparing(); absent.intro.ready();
    assert.equal(absent.listeners.size, 0);
    assert.equal(absent.nodes.startupIntro.hidden, true);
  }
});

test('bootstrap precedes dependencies; success follows initial rendering, not background IPC', () => {
  assert.match(html, /id="startupIntro" class="startup-intro" hidden/);
  assert.ok(html.indexOf('id="startupIntroRetry"') < html.indexOf('<script src="startup-intro.js'));
  assert.ok(html.indexOf('<script src="startup-intro.js') < html.indexOf('<script src="polarion.js'));
  assert.ok(app.startsWith('window.GitDrillDownStartup?.preparing();'));
  assert.match(app, /renderRecentRepos\(\);\s*render\(\);[\s\S]*?window\.GitDrillDownStartup\?\.ready\(\);\s*if \(invoke\) invoke\('build_info'\)/);
  assert.equal((app.match(/GitDrillDownStartup\?\.ready\(\)/g) || []).length, 1);
  assert.equal((app.match(/GitDrillDownStartup\?\.preparing\(\)/g) || []).length, 1);
});

test('intro has no polling, IPC, layout reads, startup wait or fake percentage', () => {
  assert.doesNotMatch(source, /setInterval|\bfetch\(|\binvoke\(|getBoundingClientRect|localStorage|offsetWidth|performance\.now|\bawait\b|new Promise/);
  assert.equal((source.match(/setTimeout\(/g) || []).length, 1);
  assert.match(source, /setTimeout\(hide, 500\)/, 'the only timer is an exit cleanup fallback, not a startup wait');
  const markup = html.slice(html.indexOf('id="startupIntro"'), html.indexOf('<div class="app-shell">'));
  assert.match(markup, /role="progressbar"/);
  assert.match(markup, /role="status" aria-live="polite"/);
  assert.doesNotMatch(markup, /aria-valuenow|\d+%|<video|<canvas/);
  assert.match(markup, /assets\/git-drilldown-mark\.png/, 'reuse the existing small brand asset');
  assert.match(css, /\.startup-intro \{[^}]*pointer-events: none/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\) \{\s*\.startup-intro-progress > span \{[^}]*animation: none/);
  assert.match(css, /\.startup-intro\[data-state="ready"\] \.startup-intro-progress > span \{[^}]*animation: none/);
  assert.match(css, /\.startup-intro\[data-exiting="true"\] \{ animation-duration: 1ms/);
});

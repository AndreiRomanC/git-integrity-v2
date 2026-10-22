'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { fitFloatingPanel, fitWorkingTreeHeight, bindPanelResize } = require('../frontend/panel-resize.js');

test('version window expands to the viewport, stays visible, and has a readable minimum', () => {
  assert.deepEqual(fitFloatingPanel({ left: 800, top: 600, width: 2000, height: 1400 }, { width: 1400, height: 900 }),
    { left: 16, top: 16, width: 1368, height: 868 });
  assert.deepEqual(fitFloatingPanel({ left: -10, top: -20, width: 100, height: 90 }, { width: 1400, height: 900 }),
    { left: 16, top: 16, width: 680, height: 340 });
  const small = fitFloatingPanel({ left: 900, top: 900, width: 1000, height: 760 }, { width: 600, height: 300 });
  assert.deepEqual(small, { left: 16, top: 16, width: 568, height: 268 });
});

test('Working tree leaves navigation visible and cannot shrink below useful height', () => {
  assert.equal(fitWorkingTreeHeight(5000, 900), 800);
  assert.equal(fitWorkingTreeHeight(10, 900), 220);
  assert.equal(fitWorkingTreeHeight(280, 900), 280);
  assert.equal(fitWorkingTreeHeight(280, 260), 160);
});

function surface() {
  const listeners = new Map();
  return {
    listeners, captures: new Set(),
    classList: { add() {}, remove() {} },
    addEventListener(name, cb) { if (!listeners.has(name)) listeners.set(name, new Set()); listeners.get(name).add(cb); },
    removeEventListener(name, cb) { listeners.get(name)?.delete(cb); },
    fire(name, fields = {}) { const event = { preventDefault() {}, stopPropagation() {}, button: 0, pointerId: 1, clientX: 100, clientY: 200, ...fields }; [...(listeners.get(name) || [])].forEach(cb => cb(event)); },
    setPointerCapture(id) { this.captures.add(id); },
    hasPointerCapture(id) { return this.captures.has(id); },
    releasePointerCapture(id) { this.captures.delete(id); this.fire('lostpointercapture'); },
  };
}

test('drag resize uses only geometry, cleans up on release/cancel/blur, and supports keyboard/reset', () => {
  const handle = surface(), host = surface(), calls = [];
  const rect = { width: 1060, height: 760 };
  bindPanelResize(handle, { read: () => rect, resize: (...args) => calls.push(args), reset: () => calls.push('reset') }, host);
  handle.fire('pointerdown', { button: 2 });
  host.fire('pointermove', { clientX: 300 });
  assert.equal(calls.length, 0);
  for (const end of ['pointerup', 'pointercancel', 'blur', 'lostpointercapture']) {
    handle.fire('pointerdown');
    host.fire('pointermove', { pointerId: 2, clientX: 800 });
    host.fire('pointermove', { clientX: 140, clientY: 160 });
    assert.deepEqual(calls.at(-1), [rect, 40, -40]);
    (end === 'lostpointercapture' ? handle : host).fire(end);
    assert.equal(handle.captures.size, 0);
    assert.equal(host.listeners.get('pointermove').size, 0);
    const count = calls.length;
    host.fire('pointermove', { clientX: 900 });
    assert.equal(calls.length, count);
  }
  handle.fire('keydown', { key: 'ArrowUp' });
  assert.deepEqual(calls.at(-1), [rect, 0, -24]);
  handle.fire('dblclick');
  assert.equal(calls.at(-1), 'reset');
});

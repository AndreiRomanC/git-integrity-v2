// Small, presentation-only resize helpers. No repository reads or refreshes.
function fitFloatingPanel(rect, viewport) {
  const margin = 16;
  const maxWidth = Math.max(1, viewport.width - margin * 2);
  const maxHeight = Math.max(1, viewport.height - margin * 2);
  const width = Math.min(maxWidth, Math.max(Math.min(680, maxWidth), rect.width));
  const height = Math.min(maxHeight, Math.max(Math.min(340, maxHeight), rect.height));
  return {
    width, height,
    left: Math.max(margin, Math.min(rect.left, viewport.width - width - margin)),
    top: Math.max(margin, Math.min(rect.top, viewport.height - height - margin)),
  };
}

function fitWorkingTreeHeight(height, viewportHeight) {
  const max = Math.max(1, viewportHeight - 100); // keep top bar and status bar accessible
  return Math.min(max, Math.max(Math.min(220, max), height));
}

function bindPanelResize(handle, { read, resize, reset, canStart = () => true }, host = window) {
  let active = null;
  const finish = () => {
    if (!active) return;
    const id = active.id;
    active = null;
    handle.classList.remove('resizing');
    if (handle.hasPointerCapture?.(id)) handle.releasePointerCapture(id);
    host.removeEventListener('pointermove', move);
    host.removeEventListener('pointerup', finish);
    host.removeEventListener('pointercancel', finish);
    host.removeEventListener('blur', finish);
  };
  const move = event => {
    if (!active || event.pointerId !== active.id) return;
    resize(active.rect, event.clientX - active.x, event.clientY - active.y);
  };
  handle.addEventListener('pointerdown', event => {
    if (event.button !== 0 || !canStart(event)) return;
    event.preventDefault(); event.stopPropagation(); finish();
    active = { id: event.pointerId, x: event.clientX, y: event.clientY, rect: read() };
    handle.classList.add('resizing');
    handle.setPointerCapture?.(event.pointerId);
    host.addEventListener('pointermove', move);
    host.addEventListener('pointerup', finish);
    host.addEventListener('pointercancel', finish);
    host.addEventListener('blur', finish);
  });
  handle.addEventListener('lostpointercapture', finish);
  handle.addEventListener('click', event => event.stopPropagation());
  handle.addEventListener('dblclick', event => { if (canStart(event)) { event.preventDefault(); reset?.(); } });
  handle.addEventListener('keydown', event => {
    const steps = { ArrowLeft: [-24, 0], ArrowRight: [24, 0], ArrowUp: [0, -24], ArrowDown: [0, 24] };
    if (!steps[event.key] || !canStart(event)) return;
    event.preventDefault(); event.stopPropagation();
    resize(read(), ...steps[event.key]);
  });
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { fitFloatingPanel, fitWorkingTreeHeight, bindPanelResize };
}

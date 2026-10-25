// Presentation only. The bar is indeterminate: loading local scripts and
// initializing the interface have no meaningful total or time percentage.
// No startup delay, polling or IPC. ready() releases initialization immediately;
// only the decorative exit waits for a paint, so fast boots can show the brand.
function createStartupIntro(doc, host) {
  const root = doc.getElementById('startupIntro');
  const label = doc.getElementById('startupIntroPhase');
  const progress = doc.getElementById('startupIntroProgress');
  const failure = doc.getElementById('startupIntroFailure');
  const retry = doc.getElementById('startupIntroRetry');
  if (!root || !label || !progress || !failure || !retry) return { preparing() {}, ready() {} };

  let state = 'loading';
  let frame = null;
  let exitFallback = null;
  const hide = () => {
    root.hidden = true;
    if (frame !== null) host.cancelAnimationFrame(frame);
    if (exitFallback !== null) host.clearTimeout(exitFallback);
    frame = exitFallback = null;
    root.removeEventListener('animationend', onExit);
  };
  const onExit = event => {
    if (event.target === root && event.animationName === 'startup-exit') hide();
  };
  const cleanup = () => {
    host.removeEventListener('error', onError, true);
    host.removeEventListener('unhandledrejection', onRejected);
  };
  const fail = () => {
    if (state === 'ready' || state === 'failed') return;
    state = 'failed';
    cleanup();
    root.dataset.state = 'failed';
    label.textContent = 'The interface could not finish loading.';
    progress.hidden = true;
    failure.hidden = false;
  };
  const onError = event => {
    // A failed logo/image is not a failed application. A failed dependency
    // or uncaught initialization error must not leave an endless loading bar.
    if (event.target === host || event.target?.tagName === 'SCRIPT') fail();
  };
  const onRejected = () => fail();
  const reload = () => { if (state === 'failed') host.location.reload(); };
  retry.addEventListener('click', reload);
  host.addEventListener('error', onError, true);
  host.addEventListener('unhandledrejection', onRejected);
  // Markup defaults to hidden so a missing bootstrap never covers the app.
  root.hidden = false;

  return {
    preparing() {
      if (state !== 'loading') return;
      state = 'preparing';
      label.textContent = 'Preparing workspace…';
      progress.setAttribute('aria-valuetext', 'Preparing workspace');
    },
    ready() {
      if (state !== 'loading' && state !== 'preparing') return;
      state = 'ready';
      cleanup();
      retry.removeEventListener('click', reload);
      // Loading really is complete. Do not keep the indeterminate bar moving
      // during the purely visual exit, and never gate app work on this exit.
      root.dataset.state = 'ready';
      label.textContent = 'Ready';
      progress.setAttribute('aria-valuetext', 'Interface ready');
      if (typeof host.requestAnimationFrame !== 'function') { hide(); return; }
      // A single rAF runs BEFORE paint and can still remove an unseen intro.
      // Two frames leave one rendering opportunity, including in a fast WebView.
      frame = host.requestAnimationFrame(() => {
        frame = host.requestAnimationFrame(() => {
          frame = null;
          root.addEventListener('animationend', onExit);
          root.dataset.exiting = 'true';
          // Cleanup only: CSS finishes first. Disabled/interrupted animations
          // must not leave the ready overlay visible. This never delays boot.
          exitFallback = host.setTimeout(hide, 500);
        });
      });
    },
  };
}

if (typeof window !== 'undefined') window.GitDrillDownStartup = createStartupIntro(document, window);
if (typeof module !== 'undefined' && module.exports) module.exports = { createStartupIntro };

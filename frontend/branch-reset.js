// Explicit dialog only: no timers, status scans, server queries or mutation here.
(function (global) {
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const modes = {
    soft: { title: 'Soft', summary: 'Keep staging and files', detail: 'Only the branch tip moves. The index (staging) and files stay unchanged.' },
    mixed: { title: 'Mixed', summary: 'Keep files · reset staging', detail: 'The branch tip and index move to the target. Your files stay unchanged; differences from the target remain unstaged.' },
    hard: { title: 'Hard', summary: 'Discard tracked local changes', detail: 'The branch tip, index and tracked files move to the target. Staged and unstaged tracked changes are discarded without a source backup. Untracked or ignored files blocking checkout can also be overwritten. This is not a full Clean: other untracked and ignored files remain.' },
  };
  function warnings(report) {
    const result = [];
    if (report.relation === 'divergent') result.push('This target is on a different line of history, not an earlier or later version of the current branch. Reset does not merge histories.');
    if (report.relation === 'unknown') result.push('History is incomplete or unavailable. The relationship to the current branch could not be established.');
    if (report.removed_commits > 0) result.push(`${report.removed_commits} current commit(s) will no longer be reachable through this branch. Other branches are not moved.`);
    const upstream = report.upstream || 'the configured upstream';
    if (report.push_status === 'non_fast_forward') result.push(`The target does not contain ${upstream}'s known tip. A normal Push to it would be rejected; publishing may require rewriting remote history. No Force Push is performed here.`);
    else if (report.push_status === 'fast_forward') result.push(`The target contains the known tip of ${upstream}. Push ancestry is compatible with this local snapshot, not a guarantee of server permission or freshness.`);
    else if (report.push_status === 'not_configured') result.push('No upstream is configured. Reset does not create one.');
    else if (report.push_status === 'local_upstream') result.push('This branch tracks a local branch, not a remote. Reset keeps that configuration; remote Push compatibility is unknown.');
    else result.push('Upstream history is unavailable or incomplete. Push compatibility could not be established.');
    return result;
  }
  function html(report, repositoryName, isSubmodule) {
    return `<header><div><h2 id="branchResetTitle">Reset current branch</h2><p>${escape(repositoryName)} · <strong>${escape(report.branch)}</strong></p></div><button type="button" data-reset-cancel aria-label="Close reset dialog">×</button></header>
      <div class="branch-reset-body">
      <div class="branch-reset-target"><span>FROM <code title="${escape(report.head)}">${escape(report.head.slice(0, 10))}</code> → TO <code title="${escape(report.target)}">${escape(report.target.slice(0, 10))}</code></span><strong>${escape(report.subject)}</strong><small>You stay on ${escape(report.branch)}. This does not switch to the clicked branch.</small></div>
      <fieldset><legend>Reset type</legend>${Object.entries(modes).map(([value, mode]) => `<label class="branch-reset-choice"><input type="radio" name="branch-reset-mode" value="${value}" ${value === 'soft' ? 'checked' : ''}><span><strong>${mode.title}</strong><small>${mode.summary}</small></span></label>`).join('')}</fieldset>
      <p data-reset-effect aria-live="polite"></p>
      <div class="branch-reset-warnings">${warnings(report).map(message => `<p>${escape(message)}</p>`).join('')}</div>
      <p class="branch-reset-note">Local operation only · no Fetch, Push or recursive Clean. Submodule folders are not updated and may show changes against the new recorded versions.${isSubmodule ? ' The parent project’s recorded submodule revision is not changed.' : ''} Upstream checks use the last fetched local refs.</p>
      <label class="branch-reset-consent"><input type="checkbox" data-reset-consent><span data-reset-consent-text></span></label>
      </div><footer><button type="button" data-reset-cancel autofocus>Cancel</button><button type="button" data-reset-apply class="danger" disabled>Reset branch</button></footer>`;
  }
  function choose(dialog, report, repositoryName, isSubmodule = false) {
    dialog.innerHTML = html(report, repositoryName, isSubmodule);
    return new Promise(resolve => {
      const apply = dialog.querySelector('[data-reset-apply]');
      const consent = dialog.querySelector('[data-reset-consent]');
      const selected = () => dialog.querySelector('input[name="branch-reset-mode"]:checked').value;
      const update = () => {
        const mode = selected();
        dialog.querySelector('[data-reset-effect]').textContent = modes[mode].detail;
        dialog.querySelector('[data-reset-effect]').classList.toggle('is-destructive', mode === 'hard');
        dialog.querySelector('[data-reset-consent-text]').textContent = mode === 'hard'
          ? 'I understand the branch will move and tracked local changes will be permanently discarded.'
          : 'I understand this moves the current branch tip and can affect future Push.';
        apply.textContent = `Reset branch · ${modes[mode].title}`;
        apply.disabled = !consent.checked;
      };
      const finish = value => {
        dialog.removeEventListener('cancel', cancel); dialog.removeEventListener('close', closed);
        dialog.close(); resolve(value);
      };
      const cancel = event => { event?.preventDefault(); finish(null); };
      const closed = () => finish(null);
      dialog.querySelectorAll('[data-reset-cancel]').forEach(button => button.addEventListener('click', cancel));
      dialog.querySelectorAll('input[name="branch-reset-mode"]').forEach(input => input.addEventListener('change', () => { consent.checked = false; update(); }));
      consent.addEventListener('change', update);
      apply.addEventListener('click', () => { if (!apply.disabled && modes[selected()]) finish(selected()); });
      dialog.addEventListener('cancel', cancel); dialog.addEventListener('close', closed);
      update(); dialog.showModal();
    });
  }
  const api = { modes, warnings, html, choose };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  global.GDDBranchReset = api;
})(typeof window !== 'undefined' ? window : globalThis);

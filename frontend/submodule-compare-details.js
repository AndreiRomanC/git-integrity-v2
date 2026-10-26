(function (global) {
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const date = seconds => seconds == null ? 'Unavailable' : new Date(seconds * 1000).toLocaleString();
  const field = (label, value) => `<dt>${escape(label)}</dt><dd>${escape(value || '—')}</dd>`;
  function sideHtml(side, label) {
    const missing = side.kind === 'absent' ? 'Not present in this version' : side.kind !== 'submodule' ? 'This side is not a submodule' : '';
    return `<section class="submodule-detail-side"><h3>${escape(label)}</h3><dl>${field('Parent snapshot', side.source)}${field('Submodule commit', side.revision || missing)}${side.revision ? field('Registered URL', side.configured_url || 'Not registered') + field('Resolved URL', side.resolved_url || 'Unavailable') + field('Configured branch', side.configured_branch || 'None · exact commit is recorded') : ''}</dl>
      ${side.message != null ? `<div class="submodule-detail-message">${escape(side.message)}</div><dl>${field('Author', side.author)}${field('Authored', date(side.authored_at))}${field('Committed', date(side.committed_at))}</dl>` : ''}
      ${side.revision ? `<dl>${field('Local tags at commit', side.tags?.join(', ') || 'None known locally')}${field('Local branch tips', side.branches?.join(', ') || 'None known locally')}</dl>` : ''}
      ${(side.notes || []).map(note => `<p class="submodule-detail-note">${escape(note)}</p>`).join('')}</section>`;
  }
  function detailsHtml(details, labels = ['Start ref', 'Compare with']) {
    return `<div class="submodule-detail-location"><strong>${escape(details.relative_path)}</strong><dl>${field('Parent repository', details.repository_path)}${field('Local location', details.local_path || 'No working directory (bare repository)')}${field('Local remote', details.local_remote_url || 'Unavailable')}</dl></div>
      <div class="submodule-detail-sides">${sideHtml(details.left, labels[0])}${sideHtml(details.right, labels[1])}</div>
      <p class="submodule-detail-note">URLs come from each snapshot’s .gitmodules. Relative URLs use the current parent remote. Tags and branch tips are current local references, not historical branch identities.</p><p class="submodule-detail-note">${escape(details.compare_note)}</p>`;
  }
  function bindRow(node, open) {
    node.title = 'Double-click for submodule versions, repository and commit details';
    node.addEventListener('dblclick', open);
    node.addEventListener('keydown', event => {
      if (event.key === 'Enter') { event.preventDefault(); open(); }
    });
  }
  function create({ dialog, body, compareButton, load, onCompare }) {
    let request = 0, selected = null, current = () => false;
    dialog.addEventListener('close', () => { if (!dialog.open) { request++; selected = null; compareButton.disabled = true; } });
    dialog.querySelectorAll('[data-submodule-details-close]').forEach(button => button.addEventListener('click', () => dialog.close()));
    compareButton.addEventListener('click', async () => {
      const details = selected;
      if (!details?.compare_repository_path || !current()) return;
      compareButton.disabled = true;
      try { await onCompare(details, () => selected === details && dialog.open && current()); if (selected === details) dialog.close(); }
      catch (error) { if (selected === details && dialog.open) { body.innerHTML = `<p class="submodule-detail-note">${escape(error)}</p>`; compareButton.disabled = false; } }
    });
    return {
      async open(args, isCurrent = () => true) {
        const mine = ++request; selected = null; current = isCurrent;
        compareButton.disabled = true;
        body.innerHTML = '<p class="submodule-detail-note" role="status">Reading local submodule metadata…</p>';
        if (!dialog.open) dialog.showModal();
        try {
          const details = await load(args);
          if (mine !== request || !dialog.open) return;
          if (!isCurrent()) { body.innerHTML = '<p class="submodule-detail-note">The comparison changed. Close and open the submodule from the current results.</p>'; return; }
          selected = details;
          body.innerHTML = detailsHtml(details);
          compareButton.disabled = !details.compare_repository_path;
        } catch (error) {
          if (mine === request && dialog.open) body.innerHTML = `<p class="submodule-detail-note">Could not read submodule details: ${escape(error)}</p>`;
        }
      },
    };
  }
  const api = { detailsHtml, bindRow, create };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  global.SubmoduleCompareDetails = api;
})(typeof window !== 'undefined' ? window : globalThis);

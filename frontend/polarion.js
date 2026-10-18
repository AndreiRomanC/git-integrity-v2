// Optional Polarion workspace. No Git mutations or background portal requests.
function polarionCommitMessage(task) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_]*-\d+$/.test(task?.id || '')) throw new Error('This task ID does not match the supported P:PROJECT-123 reference format. No message was added.');
  return 'P:' + task.id + ' - ' + String(task.title || '').replace(/[\r\n\t]+/g, ' ').trim();
}
function polarionSearchExplanation(result) {
  const hints = {
    auth_required: 'Sign in in the Polarion window, then retry.',
    forbidden: 'Polarion refused access. Check account permissions or the portal REST configuration.',
    api_unavailable: 'This resource or REST endpoint is unavailable on this Polarion server.',
    query_rejected: 'Polarion rejected the query. No broad fallback search was run.',
    not_json: 'The server returned a page instead of API results. Complete sign-in in the Polarion window.',
    network_error: 'The browser could not complete the request. Check the connection.',
    timeout: 'Polarion timed out. No partial catalog or element snapshot was applied.',
    wrong_origin: 'Finish sign-in and return to the Polarion portal in its window.',
    invalid_response: 'The response was incomplete or had an unexpected format. Existing snapshots were preserved.',
    catalog_limit: 'The accessible catalog exceeded the safe loading limit. No partial catalog was displayed.',
    response_limit: 'The Polarion reply is too large for this local overview. Existing snapshots were preserved.',
    http_error: 'Polarion returned an unexpected HTTP response.',
  };
  return (hints[result.code] || 'Search failed.') + (result.http ? ' HTTP ' + result.http + '.' : '') +
    (result.session_token ? ' Portal session token detected.' : ' No portal REST session token detected.') + ' Diagnostics are in the application log (no credentials).';
}
function polarionFilterElements(elements, query = '', type = '') {
  const words = polarionSearchText(query).split(/\s+/).filter(Boolean);
  return (elements || []).filter(item => {
    if (type && (item.item_type || '(unknown)').toLowerCase() !== type.toLowerCase()) return false;
    if (!words.length) return true;
    const text = polarionSearchText([item.id, item.title, item.item_type, item.status].filter(Boolean).join(' '));
    return words.every(word => text.includes(word));
  });
}
function polarionSearchText(value) { return String(value || '').normalize('NFKC').toLowerCase().replace(/[_\-\/]+/g, ' ').replace(/\s+/g, ' ').trim(); }
function polarionSavedDocumentMatches(workspace, project, query) {
  const doc = workspace?.document;
  if (doc?.kind !== 'document' || !doc.id?.startsWith(project + '/') || !query.trim()) return [];
  const text = polarionSearchText(doc.id + ' ' + doc.title);
  return polarionSearchText(query).split(' ').every(word => text.includes(word))
    ? [{ id: doc.id, title: doc.title, item_type: doc.item_type, local: true }] : [];
}
// Metadata only, reused from existing local reads/saves. No portal request or
// workspace reload just to label a commit dialog; opting in verifies it again.
const polarionDocumentSources = new Map();
function polarionRememberDocument(repositoryPath, workspace) {
  if (!repositoryPath) return;
  polarionDocumentSources.delete(repositoryPath);
  const doc = workspace?.document;
  polarionDocumentSources.set(repositoryPath, doc ? { id: doc.id, title: doc.title } : null);
  if (polarionDocumentSources.size > 32) polarionDocumentSources.delete(polarionDocumentSources.keys().next().value);
}
function polarionReferenceId(id) { return typeof id === 'string' && id.length <= 240 && /^[A-Za-z0-9][A-Za-z0-9_-]*-\d+$/.test(id); }
function polarionModuleElements(elements, moduleName) {
  const module = polarionSearchText(moduleName);
  if (!module) return [];
  const seen = new Set();
  return (elements || []).filter(item => {
    const id = String(item.id || '').split('/').pop();
    const title = polarionSearchText(item.title);
    if (!polarionReferenceId(id) || seen.has(id) || !(title === module || title.startsWith(module + ' '))) return false;
    seen.add(id); return true;
  });
}
function polarionElementRole(type) {
  const key = String(type || '').toLowerCase().replace(/[_\s-]/g, '');
  if (['architecturalelement','architectureelement','simulinkitem'].includes(key)) return 'architecture';
  if (key === 'designelement') return 'design';
  if (key === 'solutionelement') return 'solution';
  return key;
}
function polarionCommitMessageWithReferences(message, ids) {
  const added = [...new Set(ids)].filter(id => polarionReferenceId(id) &&
    !String(message).split(/[^A-Za-z0-9_-]+/).includes(id));
  return added.length ? String(message).trimEnd() + '\n\nPolarion: ' + added.join(', ') : message;
}

// Explicit opt-in only. This controller never modifies the shared/default
// commit message, project associations or saved commit-message history.
function createPolarionCommitReferences({ host, invoke, onChange = () => {}, log = () => {} }) {
  let context = null, generation = 0, enabled = false, pending = false, failure = '', source = null, sourceTime = '', rows = [], selected = new Set();
  const make = (tag, text = '') => { const node = document.createElement(tag); node.textContent = text; return node; };
  function render() {
    host.hidden = !context; host.replaceChildren(); if (!context) return;
    const label = make('label'), toggle = make('input'); toggle.type = 'checkbox'; toggle.checked = enabled;
    toggle.addEventListener('change', () => { enabled = toggle.checked; ++generation; failure = ''; pending = false; source = null; sourceTime = ''; rows = []; selected.clear(); render(); onChange(); if (enabled) load(); });
    label.append(toggle, make('span', 'Include StructDoc references for ' + context.moduleName)); host.append(label);
    const known = source || polarionDocumentSources.get(context.repositoryPath), identity = make('div'); identity.className = 'polarion-commit-source';
    if (known) {
      identity.append(make('strong', 'StructDoc: ' + (known.title || known.id)), make('small', known.id +
        (source ? ' · saved source verified for this selection' : ' · remembered selection; verified when checked')));
    } else identity.append(make('small', 'StructDoc: use the parent project’s selected document in P → StructDocs. Its name is loaded when checked.'));
    host.append(identity, make('small', 'Only this submodule commit · matching saved elements · no server search.'));
    if (!enabled) return;
    if (pending) { host.append(make('small', 'Finding matching document elements…')); return; }
    if (failure) {
      host.append(make('small', failure + ' Uncheck this option to commit without references.'));
      const retry = make('button', 'Retry'); retry.type = 'button'; retry.addEventListener('click', load); host.append(retry); return;
    }
    if (!rows.length) { host.append(make('small', 'No matching saved elements. Open P → StructDocs and associate/read the document first. No references will be added.')); return; }
    const list = make('div'); list.className = 'polarion-commit-reference-list';
    const preview = make('div'), summary = make('small'), ids = make('code'); preview.className = 'polarion-commit-preview'; preview.append(summary, ids);
    const updatePreview = () => {
      summary.textContent = selected.size + ' selected / ' + rows.length + ' matching elements · appended below the commit description';
      ids.textContent = selected.size ? 'Polarion: ' + [...selected].join(', ') : 'No references selected. The original commit message stays unchanged.';
    };
    for (const item of rows) {
      const id = item.id.split('/').pop(), row = make('label'), choice = make('input'); choice.type = 'checkbox'; choice.checked = selected.has(id);
      choice.addEventListener('change', () => { if (choice.checked) selected.add(id); else selected.delete(id); updatePreview(); onChange(); });
      row.append(choice, make('span', id + ' · ' + item.title), make('small', (item.item_type || 'Unknown type') + ' · ' + (item.status || 'Unknown status'))); list.append(row);
    }
    updatePreview();
    host.append(list, preview, make('small', 'Saved snapshot: ' + (sourceTime || 'date unavailable') + '. Refresh in P → StructDocs when needed. Review obsolete items; multiple candidates of the same type are not selected automatically.'));
  }
  async function load() {
    if (!context || !enabled) return;
    const ticket = ++generation, target = { ...context }; pending = true; failure = ''; render(); onChange();
    const current = () => ticket === generation && enabled && context?.key === target.key;
    try {
      const workspace = await invoke('polarion_workspace', { repositoryPath: target.repositoryPath, action: 'load', snapshot: null, elements: null, documentId: null });
      if (!current()) return;
      polarionRememberDocument(target.repositoryPath, workspace);
      source = workspace.document ? { id: workspace.document.id, title: workspace.document.title } : null;
      if (!workspace.document) throw new Error('No StructDoc is associated with the parent project.');
      if (!workspace.elements_complete || workspace.elements_revision !== (workspace.document.fields?.revision || '')) {
        throw new Error('The saved element list is missing or outdated. Read all elements in P → StructDocs first.');
      }
      sourceTime = workspace.elements_saved_at || workspace.document.fetched_at || '';
      rows = polarionModuleElements(workspace.elements, target.moduleName);
      const counts = new Map(); rows.forEach(item => { const role = polarionElementRole(item.item_type); if (role) counts.set(role, (counts.get(role) || 0) + 1); });
      selected = new Set(rows.filter(item => { const role = polarionElementRole(item.item_type); return role && counts.get(role) === 1; }).map(item => item.id.split('/').pop()));
      log(`polarion: commit_references source=saved_elements candidates=${rows.length} selected=${selected.size} no_http_requests=true`);
    } catch (error) { if (current()) { failure = String(error?.message || error); log('polarion: commit_references failed source=saved_elements no_http_requests=true'); } }
    finally { if (current()) { pending = false; render(); onChange(); } }
  }
  return {
    setContext(next) {
      const key = next ? JSON.stringify([next.repositoryPath, next.moduleName, next.commitKey || '']) : '';
      if ((context?.key || '') === key) return;
      ++generation; context = next ? { ...next, key } : null; enabled = false; pending = false; failure = ''; source = null; sourceTime = ''; rows = []; selected.clear(); render();
    },
    canSubmit: () => !enabled || (!pending && !failure),
    message(base) { if (enabled && (pending || failure)) throw new Error('StructDoc references are not ready. Retry or uncheck the option.'); return enabled ? polarionCommitMessageWithReferences(base, [...selected]) : base; },
    reset() { ++generation; enabled = false; pending = false; failure = ''; source = null; sourceTime = ''; rows = []; selected.clear(); render(); },
    dispose() { ++generation; context = null; host.replaceChildren(); host.hidden = true; },
  };
}
function polarionRememberedUser(storage, user = null) {
  const key = 'ddt-polarion-user-polarion.vitesco.io';
  try {
    if (user && /^[\w.@-]{1,120}$/.test(user.id)) storage?.setItem(key, JSON.stringify({ id: user.id, name: String(user.name || '').slice(0, 500), source: user.source === 'portal' ? 'portal' : 'manual' }));
    const saved = JSON.parse(storage?.getItem(key) || 'null');
    return saved && /^[\w.@-]{1,120}$/.test(saved.id) ? saved : null;
  } catch { return user || null; }
}
function polarionDiagnosticDocumentLink(link, document, projectId) {
  if (String(link || '').trim()) return String(link).trim();
  if (document?.kind !== 'document') return '';
  const parts = String(document.id || '').split('/');
  if (parts.length !== 3 || parts[0] !== projectId || !/^[A-Za-z0-9_-]{1,120}$/.test(parts[0]) ||
    parts.some(part => !part || part === '.' || part === '..' || part.length > 240 || /[\x00-\x1f\\]/.test(part))) return '';
  return 'https://polarion.vitesco.io/polarion/#/project/' + encodeURIComponent(parts[0]) + '/wiki/' + parts.slice(1).map(encodeURIComponent).join('/');
}
function createPolarionTaskPicker({ invoke, addMessage, log = () => {} }) {
  const dialog = document.querySelector('#polarionTaskDialog');
  const el = id => dialog.querySelector('#polarion' + id);
  const storage = typeof localStorage === 'undefined' ? null : localStorage;
  let identity = polarionRememberedUser(storage), otherUser = false;
  let generation = 0, context = null, project = null, selected = null, detail = null, busy = false;
  let mode = 'projects', lastSearch = null, page = 1, more = false, workspace = {};
  let mountedMode = '';
  const statusByMode = {};
  const modeHint = () => mode === 'projects' ? 'Find a project, then select it to continue.' : mode === 'tasks' ? 'Find tasks in the selected project. Add a selected task to your commit list explicitly.' : 'Find a StructDoc by name in the selected project, or open an exact Polarion document link.';
  const say = text => { statusByMode[mode] = text; el('Status').textContent = text; };
  const make = (tag, text, className = '') => { const node = document.createElement(tag); node.textContent = text; node.className = className; return node; };
  const button = (text, action) => { const node = make('button', text); node.type = 'button'; node.disabled = busy; node.addEventListener('click', action); return node; };
  const current = ticket => ticket === generation && dialog.open && context;
  const connection = (state, text) => {
    el('Connection').dataset.state = state; el('ConnectionPanel').dataset.state = state; el('Connection').textContent = text;
    el('Connect').textContent = state === 'required' ? 'Sign in to Polarion ↗' : 'Open Polarion ↗';
    el('ConnectionHint').textContent = state === 'required'
      ? 'Open the Polarion window, complete sign-in there, then retry. Being signed in to Edge does not connect this window.'
      : state === 'closed' ? 'Open the Polarion window to reconnect, then retry. Sign in there only if asked; Edge has a separate session.'
      : state === 'connected' ? 'The last read request succeeded. This Polarion window has its own session; Edge sign-in is separate.'
      : 'Access has not been verified. Open Polarion to sign in, or Search to check an existing session. Saved snapshots do not prove a connection.';
  };
  const update = () => {
    if (mountedMode !== mode) {
      // Reuse the existing result/detail components, but keep them physically
      // inside the active tab. No StructDoc content leaks into Tasks/Projects.
      const host = el(mode === 'projects' ? 'ProjectWorkspace' : mode === 'tasks' ? 'TaskWorkspace' : 'DocumentWorkspace');
      host.append(el('Results'), el('Pages'), el('SelectionActions'), el('Detail'));
      mountedMode = mode; el('TabBody').scrollTop = 0;
    }
    for (const [name, section] of [['Projects','projects'], ['Tasks','tasks'], ['Documents','documents']]) {
      el('Panel' + name).hidden = mode !== section;
      el('Tab' + name).setAttribute('aria-selected', String(mode === section));
      el('Tab' + name).disabled = busy;
    }
    for (const name of ['ProjectSearchButton', 'AllProjects', 'DetectUser', 'OtherUser', 'UseMe', 'Diagnostics']) el(name).disabled = busy || !context;
    el('DiagnosticUrl').disabled = busy;
    el('OpenDocument').disabled = busy || !context || !el('DiagnosticUrl').value.trim();
    el('DocumentSource').disabled = busy;
    el('TaskSearchButton').disabled = el('DocumentSearchButton').disabled = busy || !project || !context;
    el('TaskQuery').disabled = el('DocumentQuery').disabled = el('Assigned').disabled = !project || busy;
    el('Connect').disabled = busy;
    el('View').disabled = busy || !selected;
    el('SelectionActions').hidden = !selected || mode === 'projects';
    el('PrimaryButton').hidden = mode !== 'tasks'; el('PrimaryButton').disabled = busy || selected?.kind !== 'tasks';
    el('Associate').hidden = mode !== 'documents'; el('Associate').disabled = busy || !['documents', 'containers'].includes(selected?.kind);
    el('Add').hidden = mode !== 'tasks'; el('Add').disabled = busy || selected?.kind !== 'tasks';
    el('Previous').disabled = busy || !lastSearch || page <= 1; el('Next').disabled = busy || !lastSearch || !more;
    el('Pages').hidden = !lastSearch || (page <= 1 && !more);
    el('User').hidden = !otherUser && (!el('Assigned').checked || !!identity?.id);
    el('UseMe').hidden = !otherUser; el('OtherUser').hidden = otherUser;
    el('Identity').textContent = otherUser ? 'Filtering another user' : identity ? (identity.name || identity.id) + ' · ' + (identity.source === 'portal' ? 'detected / remembered' : 'remembered user ID') : 'User not detected yet';
    el('ChosenProject').textContent = project ? project.id + ' · ' + project.name : 'No project selected · choose one in Projects';
  };
  const clearResults = () => { selected = null; lastSearch = null; el('Results').replaceChildren(); el('Results').hidden = true; el('SearchServer').hidden = true; more = false; update(); };
  const clearDetail = () => { detail = null; el('Detail').hidden = true; el('Detail').replaceChildren(); };
  function switchMode(next) { if (busy) return; ++generation; mode = next; clearResults(); clearDetail(); update(); renderElements(); el('Status').textContent = statusByMode[mode] || modeHint(); }
  async function action(work) {
    if (busy || !context) return;
    const ticket = ++generation; busy = true; update(); renderSaved();
    try { await work(ticket); }
    catch (error) {
      if (current(ticket)) {
        if (['auth_required', 'wrong_origin', 'not_json'].includes(error?.polarionCode)) {
          connection('required', 'Not connected to Polarion · sign-in required');
        } else if (String(error).includes('Open Polarion, sign in if needed, then retry Search.')) {
          connection('closed', 'Not connected to Polarion · window is closed');
        }
        say(String(error));
      }
    }
    finally { if (current(ticket)) { busy = false; update(); renderSaved(); renderElements(); } }
  }
  function checked(result) {
    if (result.code !== 'ok') {
      const error = new Error(polarionSearchExplanation(result)); error.polarionCode = result.code; throw error;
    }
    return result;
  }
  const read = async (kind, resourceId = '', page = 1, revision = null) => checked(await invoke('polarion_read', { kind, resourceId, page, revision }));
  const save = async (kind, snapshot = null, elements = null, documentId = null) => invoke('polarion_workspace', { repositoryPath: context.repository_path, action: kind, snapshot, elements, documentId });
  function showDetail(snapshot) {
    detail = snapshot; const pane = el('Detail'); pane.hidden = false; pane.replaceChildren();
    pane.append(make('h3', snapshot.title), make('small', snapshot.id + ' · Local snapshot read ' + (snapshot.fetched_at || 'at an unknown time') + '. Refresh explicitly for current data.'));
    const list = make('dl', '');
    const fields = { Type: snapshot.item_type || 'Not returned', Status: snapshot.status || 'Not returned', ...(snapshot.kind === 'workitem' ? { Assignee: snapshot.assignees?.join(', ') || 'Not returned / unassigned' } : {}), ...(snapshot.fields || {}) };
    for (const [key, value] of Object.entries(fields)) list.append(make('dt', key.replace(/([a-z])([A-Z])/g, '$1 $2')), make('dd', value));
    pane.append(list, make('p', snapshot.description || (snapshot.kind === 'document' ? 'Read the document elements from its saved association below.' : 'No description in this saved overview. Read details for live data.')));
  }
  function selectSnapshot(snapshot) {
    selected = { id: snapshot.id.split('/').pop(), title: snapshot.title, resourceId: snapshot.id,
      kind: snapshot.kind === 'document' ? 'documents' : snapshot.item_type === 'task' ? 'tasks' : 'containers' };
    showDetail(snapshot); update();
  }
  function savedCard(target, title, snapshot, isDoc) {
    target.replaceChildren();
    const copy = make('div', ''); copy.append(make('strong', title), make('small', snapshot ? snapshot.id + ' · ' + snapshot.title + ' · ' + (snapshot.status || 'Status not returned') : isDoc ? 'No StructDoc associated. Open a document to preview and associate it.' : 'No primary task yet. Select a task to save its details.'));
    target.append(copy); if (!snapshot) return;
    const actions = make('div', '', 'polarion-card-actions');
    actions.append(button('View saved', () => { if (!busy) selectSnapshot(snapshot); }),
      button('Refresh info', () => action(async ticket => {
        const result = await read(snapshot.kind, snapshot.id); if (!current(ticket)) return;
        connection('connected', 'Connected · Polarion read access verified');
        const saved = await save(isDoc ? 'document' : 'primary', result.snapshot); if (!current(ticket)) return;
        workspace = saved; showDetail(result.snapshot); say('Snapshot refreshed and saved locally. Polarion unchanged.');
      })));
    if (isDoc) actions.append(button('Read all elements', loadElements));
    actions.append(button('Remove association', () => action(async ticket => {
      const saved = await save(isDoc ? 'document' : 'primary'); if (!current(ticket)) return;
      workspace = saved; clearDetail(); if (isDoc) renderElements(); say('Local association removed. Nothing deleted in Polarion.');
    })));
    target.append(actions);
  }
  function renderSaved() {
    if (context) polarionRememberDocument(context.repository_path, workspace);
    savedCard(el('Primary'), 'Primary task · local association', workspace.primary_task, false);
    savedCard(el('Document'), workspace.document ? 'Selected StructDoc · source for submodule commit references' : 'StructDoc for submodule commit references', workspace.document, true);
    el('DocumentElements').hidden = !workspace.document;
  }
  function renderElements() {
    // Task/project searches must not rebuild a potentially large hidden
    // StructDoc list. Its saved data is rendered only when that tab is used.
    if (mode !== 'documents') { el('Elements').replaceChildren(); return; }
    const query = el('ElementQuery').value || '', type = el('ElementType').value || '';
    const all = workspace.elements || [];
    const types = [...new Set(all.map(item => item.item_type || '(unknown)'))].sort();
    el('ElementType').replaceChildren();
    for (const value of ['', ...types]) { const option = make('option', value || 'All types'); option.value = value; el('ElementType').append(option); }
    el('ElementType').value = types.find(value => value.toLowerCase() === type.toLowerCase()) || '';
    const rows = polarionFilterElements(all, query, el('ElementType').value);
    el('Elements').replaceChildren();
    for (const item of rows) {
      const row = button('', () => { if (!busy) selectSnapshot(item); }); row.className = 'polarion-element';
      row.append(make('span', item.id.split('/').pop() + ' · ' + item.title), make('small', (item.item_type || 'Type unavailable') + ' · ' + (item.status || 'Status unavailable')));
      el('Elements').append(row);
    }
    el('ElementSummary').textContent = workspace.document ? rows.length + ' shown / ' + all.length + ' saved elements · ' +
      (workspace.elements_complete ? 'All source pages read ' + workspace.elements_saved_at : 'Elements not loaded yet') +
      (workspace.elements_revision ? ' · source revision ' + workspace.elements_revision : '') +
      (workspace.elements_complete && workspace.elements_revision !== (workspace.document.fields?.revision || '') ? '. Document metadata changed; Read all elements to refresh the saved list.' : '') +
      (workspace.document.kind === 'workitem' ? '. Direct outgoing linked items only; not a recursive hierarchy.' : '. Document work-item order is preserved.') +
      ' Types come from Polarion; unavailable details are not silently excluded.' : 'Associate a document, then read its elements.';
  }
  async function loadElements() {
    if (!workspace.document) return;
    const documentSnapshot = { ...workspace.document };
    await action(async ticket => {
      const all = [], seen = new Set(); let incomplete = 0, limited = false;
      for (let page = 1; ; page++) {
        if (page > 200) throw new Error('Element loading exceeded 200 pages. Previous saved elements were preserved.');
        say('Reading StructDoc elements · page ' + page + ' · ' + all.length + ' loaded…');
        const result = await read(documentSnapshot.kind === 'document' ? 'elements' : 'links', documentSnapshot.id, page, documentSnapshot.fields?.revision || null);
        if (!current(ticket)) return;
        limited ||= Boolean(result.metadata_fallback);
        // IDs may legitimately repeat in documents; preserve every occurrence.
        // Detect a server ignoring pagination using stable resource IDs instead.
        const pageKey = result.page_key || (result.elements || []).map(s => s.id).join('|') + ':' + result.received;
        if (result.more && seen.has(pageKey)) throw new Error('Polarion repeated an element page. Previous saved elements were preserved.');
        if (pageKey) seen.add(pageKey);
        all.push(...(result.elements || [])); incomplete += result.incomplete || 0;
        if (all.length > 5000) throw new Error('More than 5000 elements. Previous saved elements were preserved.');
        if (!result.more) break;
      }
      documentSnapshot.fetched_at = new Date().toISOString();
      const saved = await save('elements', documentSnapshot, all); if (!current(ticket)) return;
      workspace = saved; renderElements();
      connection('connected', 'Connected · Polarion read access verified');
      say('Saved ' + all.length + ' elements outside Git.' + (incomplete ? ' ' + incomplete + ' entries have unavailable details; select and Read details.' : '') +
        (limited ? ' Optional element metadata was unavailable; the compatible field set was used.' : '') +
        (!documentSnapshot.fields?.revision ? ' The server did not supply a revision; concurrent edits cannot be ruled out.' : ' Source revision: ' + documentSnapshot.fields.revision));
    });
  }
  async function detectUser(ticket) {
    const result = await read('user'); if (!current(ticket)) return false;
    connection('connected', 'Connected · signed-in Polarion user verified');
    identity = { id: result.user_id, name: result.user_name, source: 'portal' };
    polarionRememberedUser(storage, identity); update(); return true;
  }
  async function chooseProject(item) {
    await action(async ticket => {
      project = { id: item.id, name: item.title }; clearResults(); clearDetail();
      const loaded = await invoke('polarion_project_context', { repositoryPath: context.repository_path, relativePath: '', project });
      if (!current(ticket)) return;
      project = loaded.project || project;
      say('Project selected: ' + project.id + '. Stay here to change it, or open Tasks / StructDocs when ready.');
    });
  }
  function render(items, kind) {
    el('Results').replaceChildren(); el('Results').hidden = false;
    if (!items.length) el('Results').append(make('p', 'No matching ' + (kind === 'projects' ? 'projects' : kind === 'tasks' ? 'tasks' : 'StructDocs') + '. Try another fragment.', 'polarion-empty'));
    for (const item of items) {
      const row = button('', () => {
        if (busy) return;
        if (kind === 'projects') { chooseProject(item); return; }
        selected = { ...item, kind, resourceId: kind === 'documents' ? item.id : project.id + '/' + item.id };
        clearDetail();
        for (const sibling of el('Results').children) sibling.setAttribute('aria-pressed', String(sibling === row));
        update();
      });
      // Results remain selectable after the operation's busy state ends.
      row.disabled = false; row.className = 'polarion-result'; row.setAttribute('aria-pressed', 'false');
      row.append(make('strong', item.id), make('span', item.title + (item.item_type ? ' · ' + item.item_type : '')));
      el('Results').append(row);
    }
  }
  async function search(kind, requestedPage = 1, previous = null) {
    await action(async ticket => {
      if (kind !== 'projects' && !project) return;
      const params = previous || { kind, project: project?.id || '', query: el(kind === 'projects' ? 'ProjectQuery' : kind === 'tasks' ? 'TaskQuery' : 'DocumentQuery').value.trim(), user: '' };
      if (kind === 'tasks' && !previous && el('Assigned').checked) {
        if (!otherUser && !identity?.id && !el('User').value.trim()) await detectUser(ticket);
        if (!current(ticket)) return;
        params.user = otherUser ? el('User').value.trim() : identity?.id || el('User').value.trim();
        if (!params.user) throw new Error('Enter your Polarion user ID once, or Detect signed-in user. It is not inferred from Git.');
      }
      if (kind === 'documents' && params.query.length < 3) {
        clearResults(); clearDetail(); throw new Error('Type at least three characters of the document name or path.');
      }
      el('SearchServer').hidden = true;
      const savedDocuments = kind === 'documents' && requestedPage === 1 && !params.serverOnly ? polarionSavedDocumentMatches(workspace, params.project, params.query) : [];
      if (savedDocuments.length) {
        clearResults(); clearDetail(); render(savedDocuments, kind);
        el('DocumentHint').textContent = 'Matching saved StructDoc · local association, not a complete server catalog. Read details refreshes it from Polarion.';
        say('Found the saved document locally. No server search was needed. Use Search server to look for other documents.');
        el('SearchServer').hidden = false;
        log(`polarion: document_search source=saved_association matches=${savedDocuments.length} no_http_requests=true`);
        return;
      }
      clearResults(); clearDetail(); say(kind === 'documents' ? 'Finding documents · checking compatible read-only search…' : 'Searching Polarion…');
      const { serverOnly, ...request } = params;
      const response = await invoke('polarion_search', { ...request, page: requestedPage });
      if (!current(ticket)) return;
      if (kind === 'documents' && ['api_unavailable', 'forbidden'].includes(response.code)) {
        el('DocumentHint').textContent = 'Document search could not complete for this server/session. You can still try opening an exact Polarion document link.';
        say((response.code === 'forbidden' ? 'Polarion refused search access; no further route was tried' : 'Document search unavailable') +
          (response.http ? ' · HTTP ' + response.http : '') + '. Use Open document by link. The log records each attempted search route.');
        return;
      }
      const result = checked(response);
      connection('connected', 'Connected · Polarion read access verified');
      if (kind === 'tasks' && params.user && !otherUser && !identity) {
        identity = { id: params.user, source: 'manual' }; polarionRememberedUser(storage, identity);
      }
      lastSearch = params; page = requestedPage; more = result.more; render(result.items, kind);
      const moduleSearch = kind === 'documents' && result.catalog_source === 'work_item_modules';
      const tokenSearch = moduleSearch && result.document_query_variant && !['module_id_contains', 'module_contains'].includes(result.document_query_variant);
      if (kind === 'documents') el('DocumentHint').textContent = moduleSearch
        ? 'Compatibility search via owning Work Item → Document links.' + (tokenSearch ? ' This server used a token/prefix or exact-name lookup, not unrestricted substring matching.' : '') + ' Empty documents, reference-only documents and title-only matches may be missing. No complete catalog is implied.'
        : 'Search covers accessible documents in the selected project. Select a result to read its full details; an exact link is optional.';
      say((kind === 'projects' || kind === 'documents' ? result.matched + ' matches / ' + result.received + ' accessible entries · ' : '') +
        'Page ' + page + ' · ' + result.items.length + ' shown' + (more ? ' · Next shows more' : '') +
        (result.prefix_fallback ? '. Server rejected contains search; prefix fallback was used.' : '') +
        (result.catalog_cached ? kind === 'documents' ? '. Search data cached for up to 2 minutes.' : '. Complete catalog cached for up to 2 minutes.' : '') +
        (moduleSearch ? '. Limited document discovery' + (tokenSearch ? ' · token/prefix or exact lookup' : '') + '; zero matches do not prove the document is absent' : '') +
        (kind === 'projects' ? '. Select a project.' : '. Select a result to read details or associate it locally.'));
    });
  }
  async function openDocument() {
    if (mode !== 'documents') return;
    const link = el('DiagnosticUrl').value.trim();
    if (!link) return;
    await action(async ticket => {
      clearResults(); clearDetail(); say('Reading the exact LiveDoc · no catalog search…');
      const result = await read('document_link', link);
      if (!current(ticket)) return;
      if (result.snapshot?.kind !== 'document') throw new Error('Polarion did not return the requested LiveDoc; no association was changed.');
      selectSnapshot(result.snapshot);
      connection('connected', 'Connected · read access verified');
      el('DocumentHint').textContent = 'Exact LiveDoc read successfully. Catalog access is not required for this document.';
      say('Document opened for preview. Use this StructDoc to select it for submodule commits, then Read all elements.' + (result.metadata_fallback ? ' Only basic metadata was available on this server.' : '') + ' Nothing changed in Polarion.');
    });
  }
  async function readSelected(saveAs = '') {
    if (!selected) return;
    const choice = { ...selected };
    await action(async ticket => {
      const result = await read(choice.kind === 'documents' ? 'document' : 'workitem', choice.resourceId);
      if (!current(ticket)) return;
      connection('connected', 'Connected · Polarion read access verified');
      if (saveAs === 'primary' && result.snapshot.item_type !== 'task') throw new Error('The selected work item is not a task; no primary task was changed.');
      let write = saveAs;
      if (!write && workspace.primary_task?.id === result.snapshot.id) write = 'primary';
      if (!write && workspace.document?.id === result.snapshot.id) write = 'document';
      if (!write && workspace.elements?.some(s => s.id === result.snapshot.id)) write = 'element';
      if (write) {
        const saved = await save(write, result.snapshot, null, workspace.document?.id || null); if (!current(ticket)) return;
        workspace = saved; if (write === 'document') renderElements();
      }
      showDetail(result.snapshot); say(write === 'document'
        ? 'Selected StructDoc for submodule commits: ' + (result.snapshot.title || result.snapshot.id) + '. Read all elements before including references. Polarion unchanged.'
        : write ? 'Snapshot saved outside Git. Polarion unchanged.' : 'Read-only details loaded. Use Set primary task / Use this StructDoc to save the association.');
    });
  }
  for (const [name, next] of [['Projects','projects'], ['Tasks','tasks'], ['Documents','documents']]) el('Tab' + name).addEventListener('click', () => switchMode(next));
  el('Connect').addEventListener('click', async () => {
    const ticket = generation;
    try { await invoke('polarion_connect'); if (generation === ticket && dialog.open) { connection('unknown', 'Portal opened · complete sign-in if needed'); say('Sign in, then Detect signed-in user or Search. Edge sign-in is separate.'); } }
    catch (error) { if (generation === ticket) say(String(error)); }
  });
  el('DetectUser').addEventListener('click', () => action(async ticket => { if (await detectUser(ticket)) say('Signed-in user detected and remembered on this computer.'); }));
  const runDiagnostics = () => mode !== 'documents' ? Promise.resolve() : action(async ticket => {
    el('DiagnosticReport').replaceChildren();
    say('Checking read-only Polarion endpoints · up to 25 seconds. No associations or data will be changed.');
    const result = checked(await invoke('polarion_diagnostics', {
      project: project?.id || '', resourceUrl: polarionDiagnosticDocumentLink(el('DiagnosticUrl').value, workspace.document, project?.id),
      query: mode === 'documents' && el('DocumentSource').value === 'containers' ? el('DocumentQuery').value.trim() : '',
      documentQuery: el('DocumentSource').value !== 'containers' && el('DocumentQuery').value.trim().length >= 3 ? el('DocumentQuery').value.trim() : '',
    }));
    if (!current(ticket)) return;
    const rows = result.diagnostics || [], ok = rows.filter(d => d.code === 'ok'), skipped = rows.filter(d => d.code === 'not_run');
    const names = { metadata:'Server version', current_user:'Current user', project_access:'Project access', tasks_baseline:'Task query baseline', documents_catalog:'Project document catalog', documents_global:'Global catalog · selected project', workitem_modules:'Work Item → Document links', document_name_query:'Current document name query', document_query:'Document search syntax', known_document_query:'Known document · exact query', known_item_query:'Known work item · exact query', document_parts_expanded:'LiveDoc sample · first 10 parts + linked items', workitem_sample:'One linked work item · direct read', spaces:'Project spaces', document_exact:'Exact LiveDoc', page_exact:'Exact page', space_documents:'Space document catalog', space_pages:'Space page catalog', document_parts:'LiveDoc elements endpoint', query_current:'Current work-item query', query_prefix:'Prefix query with id:*', query_without_id_star:'Prefix query without id:*' };
    const report = el('DiagnosticReport');
    if (result.server_metadata?.version) report.append(make('strong', 'Polarion ' + result.server_metadata.version + (result.server_metadata.build ? ' · build ' + result.server_metadata.build : '')));
    for (const row of rows) {
      const line = make('div', '', 'polarion-diagnostic-result'); line.dataset.state = row.code;
      line.append(make('span', (names[row.step] || 'Read check') + (row.query_variant ? ' · ' + row.query_variant.replace(/_/g, ' ') : '')), make('small',
        (row.code === 'ok' ? 'GET accepted' : row.code.replace(/_/g, ' ')) + (row.http ? ' · HTTP ' + row.http : '') +
        (row.reason ? ' · ' + row.reason.replace(/_/g, ' ') : '') + ' · ' + (row.duration_ms || 0) + ' ms' +
        (row.query_variant && row.http === 200 ? ' · ' + (row.count || 0) + ' rows in sample' : '') +
        (typeof row.known_match === 'boolean' ? row.known_match ? ' · known reference found' : ' · known reference not in sample' : '') +
        (row.item_types?.length ? ' · types: ' + row.item_types.join(', ') : '') +
        (row.step === 'document_parts_expanded' && row.code === 'ok' ? ' · ' + (row.workitem_links || 0) + ' work-item links / ' + (row.included_workitems || 0) + ' included details (sample only)' : '')));
      report.append(line);
    }
    if (ok.length) connection('connected', 'Connected · read access verified');
    else if (rows.some(d => d.code === 'auth_required')) connection('required', 'Not connected to Polarion · sign-in required');
    const exact = rows.some(d => d.step === 'document_exact' && d.code === 'ok');
    const parts = rows.some(d => d.step === 'document_parts' && d.code === 'ok');
    el('DiagnosticSummary').textContent = (exact ? 'Exact LiveDoc readable' + (parts ? ' · elements endpoint readable' : '') + '. ' : ok.length + '/' + rows.length + ' read checks accepted. ') +
      'Syntax checks distinguish accepted/empty queries from rejected queries. A pasted link or saved StructDoc supplies exact reference checks. Element checks sample at most 10 parts and one linked work item; they do not verify the whole document. No association is changed.';
    say('Read checks: ' + ok.length + '/' + rows.length + ' accepted' + (skipped.length ? ' · ' + skipped.length + ' not run' : '') + '. Details are in this tab and the application log. No data or associations changed.');
  });
  el('Diagnostics').addEventListener('click', runDiagnostics);
  el('OpenDocument').addEventListener('click', openDocument);
  el('DiagnosticUrl').addEventListener('input', update);
  el('DiagnosticUrl').addEventListener('keydown', event => {
    if (event.key === 'Enter' && !event.isComposing) { event.preventDefault(); openDocument(); }
  });
  el('OtherUser').addEventListener('click', () => { otherUser = true; el('Assigned').checked = true; el('User').value = ''; update(); el('User').focus(); });
  el('UseMe').addEventListener('click', () => { otherUser = false; el('User').value = ''; update(); });
  el('Assigned').addEventListener('change', update);
  el('ProjectSearchButton').addEventListener('click', () => search('projects'));
  el('AllProjects').addEventListener('click', () => { el('ProjectQuery').value = ''; search('projects'); });
  el('TaskSearchButton').addEventListener('click', () => search('tasks'));
  el('DocumentSearchButton').addEventListener('click', () => search(el('DocumentSource').value || 'documents'));
  el('SearchServer').addEventListener('click', () => search('documents', 1, { kind: 'documents', project: project?.id || '', query: el('DocumentQuery').value.trim(), user: '', serverOnly: true }));
  el('DocumentSource').addEventListener('change', () => { if (!busy) { ++generation; clearResults(); clearDetail(); } });
  for (const [id, kind] of [['ProjectQuery','projects'], ['TaskQuery','tasks'], ['User','tasks'], ['DocumentQuery',null]]) el(id).addEventListener('keydown', event => {
    if (event.key === 'Enter') { event.preventDefault(); search(kind || el('DocumentSource').value || 'documents'); }
  });
  const filterElements = () => { renderElements(); log(`polarion: element_filter query_len=${el('ElementQuery').value.length} type_filter=${!!el('ElementType').value} total=${workspace.elements?.length || 0} matched=${el('Elements').children.length} no_http_requests=true`); };
  el('ElementQuery').addEventListener('input', filterElements); el('ElementType').addEventListener('change', filterElements);
  el('ClearElements').addEventListener('click', () => { el('ElementQuery').value = ''; el('ElementType').value = ''; filterElements(); });
  el('Previous').addEventListener('click', () => { if (lastSearch) search(lastSearch.kind, page - 1, lastSearch); });
  el('Next').addEventListener('click', () => { if (lastSearch) search(lastSearch.kind, page + 1, lastSearch); });
  el('View').addEventListener('click', () => readSelected());
  el('PrimaryButton').addEventListener('click', () => readSelected('primary'));
  el('Associate').addEventListener('click', () => readSelected('document'));
  el('Add').addEventListener('click', () => {
    if (selected?.kind !== 'tasks' || busy) return;
    try { addMessage(polarionCommitMessage(selected)); dialog.close(); } catch (error) { say(String(error)); }
  });
  dialog.addEventListener('close', () => { if (!dialog.open) { ++generation; context = null; selected = null; } });
  return {
    async open(repositoryPath, relativePath = '') {
      if (!repositoryPath || dialog.open) return;
      const ticket = ++generation;
      context = null; project = null; selected = null; detail = null; workspace = {}; busy = true; lastSearch = null;
      identity = polarionRememberedUser(storage) || identity; otherUser = false; mode = 'projects';
      for (const key of Object.keys(statusByMode)) delete statusByMode[key];
      connection('unknown', 'Connection not checked · Search verifies access');
      for (const id of ['ProjectQuery','TaskQuery','DocumentQuery','ElementQuery','User']) el(id).value = '';
      el('DiagnosticUrl').value = ''; el('DiagnosticReport').replaceChildren();
      el('SearchServer').hidden = true;
      el('DiagnosticSummary').textContent = 'Optional, explicit GET checks. Results stay in this tab and the log.';
      el('DocumentHint').textContent = 'Search by name above, or paste an exact LiveDoc link to open it directly.';
      el('Context').textContent = ''; el('ElementType').value = ''; clearResults(); clearDetail(); renderSaved(); renderElements();
      dialog.showModal(); say('Reading local Polarion associations…');
      try {
        const loaded = await invoke('polarion_project_context', { repositoryPath, relativePath, project: null });
        if (generation !== ticket || !dialog.open) return;
        context = loaded; project = loaded.project; mode = project ? 'tasks' : 'projects'; el('Context').textContent = loaded.repository_path; el('Context').title = loaded.repository_path;
        const loadedWorkspace = await invoke('polarion_workspace', { repositoryPath: loaded.repository_path, action: 'load', snapshot: null, elements: null, documentId: null });
        if (!current(ticket)) return;
        workspace = loadedWorkspace;
        say('Local snapshots loaded; no server request made. Search tasks, choose a project or open StructDocs.');
      } catch (error) { if (generation === ticket) say(String(error)); }
      finally { if (generation === ticket && dialog.open) { busy = false; update(); renderSaved(); renderElements(); el(project ? 'TaskQuery' : 'ProjectQuery').focus(); } }
    },
  };
}
if (typeof module !== 'undefined' && module.exports) module.exports = { polarionCommitMessage, polarionSearchExplanation, polarionFilterElements, polarionRememberedUser, polarionDiagnosticDocumentLink, polarionSavedDocumentMatches, polarionModuleElements, polarionElementRole, polarionCommitMessageWithReferences, createPolarionCommitReferences };

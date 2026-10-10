// Optional, read-only portal search. Commit history/storage remain owned by app.js.
function polarionCommitMessage(task) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_]*-\d+$/.test(task?.id || '')) throw new Error('This task ID does not match the supported P:PROJECT-123 reference format. No message was added.');
  return `P:${task.id} - ${String(task.title || '').replace(/[\r\n\t]+/g, ' ').trim()}`;
}
function polarionSearchExplanation(result) {
  const hints = {
    auth_required: 'Sign in in the Polarion window, then retry Search.',
    forbidden: 'Polarion refused API access. Your account permissions or portal REST-token configuration may prevent this search.',
    api_unavailable: 'The Polarion REST API is unavailable on this server.',
    query_rejected: 'Polarion rejected the search query. Try a shorter name or ID.',
    not_json: 'The server returned a page instead of API results. Complete sign-in in the Polarion window, then retry.',
    network_error: 'The browser could not complete the API request. Check the Polarion window and connection.',
    timeout: 'Polarion search timed out. You can retry; no commit or message was added.',
    wrong_origin: 'Finish signing in and return to the Polarion portal in its window, then retry Search.',
    invalid_response: 'The API response did not have the expected format.',
    http_error: 'Polarion returned an unexpected HTTP response.',
  };
  return `${hints[result.code] || 'Search failed.'}${result.http ? ` HTTP ${result.http}.` : ''} ${result.session_token ? 'Portal session token detected.' : 'No portal REST session token detected.'} Diagnostics are in the application log (no credentials).`;
}
function createPolarionTaskPicker({ invoke, addMessage }) {
  const dialog = document.querySelector('#polarionTaskDialog');
  const el = id => dialog.querySelector(`#polarion${id}`);
  let generation = 0, context = null, project = null, selected = null, busy = false;
  let lastSearch = null, page = 1, more = false;
  const say = text => { el('Status').textContent = text; };
  const update = () => {
    el('ProjectSearchButton').disabled = busy || !context;
    el('TaskSearchButton').disabled = busy || !project || !context;
    el('Add').disabled = busy || !selected;
    el('Previous').disabled = busy || !lastSearch || page <= 1;
    el('Next').disabled = busy || !lastSearch || !more;
    el('User').hidden = !el('Assigned').checked;
    el('ChosenProject').textContent = project ? `${project.id} · ${project.name}` : 'No project selected';
  };
  const clearResults = () => { selected = null; el('Results').replaceChildren(); more = false; update(); };
  async function chooseProject(item) {
    const ticket = ++generation;
    project = { id: item.id, name: item.title }; lastSearch = null; clearResults();
    busy = true; update(); say('Saving the project association outside Git…');
    try {
      await invoke('polarion_project_context', { repositoryPath: context.repository_path, relativePath: '', project });
      if (generation === ticket) say('Project remembered. Search tasks below.');
    } catch (error) { if (generation === ticket) say(`Project selected for this session, but could not be saved: ${error}`); }
    finally { if (generation === ticket) { busy = false; update(); el('TaskQuery').focus(); } }
  }
  function render(items, kind) {
    el('Results').replaceChildren();
    for (const item of items) {
      const row = document.createElement('button'); row.type = 'button'; row.className = 'polarion-result';
      row.setAttribute('aria-pressed', 'false');
      const id = document.createElement('strong'); id.textContent = item.id;
      const title = document.createElement('span'); title.textContent = item.title;
      row.append(id, title);
      row.addEventListener('click', () => {
        if (busy) return;
        if (kind === 'projects') { chooseProject(item); return; }
        selected = item;
        for (const sibling of el('Results').children) sibling.setAttribute('aria-pressed', String(sibling === row));
        update();
      });
      el('Results').append(row);
    }
  }
  async function search(kind, requestedPage = 1, saved = null) {
    if (busy || !context) return;
    const params = saved || { kind, project: project?.id || '', query: el(kind === 'projects' ? 'ProjectQuery' : 'TaskQuery').value.trim(), user: kind === 'tasks' && el('Assigned').checked ? el('User').value.trim() : '' };
    if (kind === 'tasks' && !project) return;
    if (kind === 'tasks' && !saved && el('Assigned').checked && !params.user) { say('Enter your Polarion user ID for “Assigned to me”. It is not inferred from Git.'); el('User').focus(); return; }
    const ticket = ++generation;
    busy = true; clearResults(); say('Searching Polarion…');
    try {
      const result = await invoke('polarion_search', { ...params, page: requestedPage });
      if (generation !== ticket || !dialog.open) return;
      if (result.code !== 'ok') { lastSearch = null; say(polarionSearchExplanation(result)); return; }
      lastSearch = params; page = requestedPage; more = result.more;
      render(result.items, kind);
      say(`${kind === 'projects' ? 'Projects' : 'Tasks'} · page ${page} · ${result.items.length} results${more ? ' · more available' : ''}. ${kind === 'projects' ? 'Select a project.' : 'Select a task to add it to the saved commit messages.'}`);
    } catch (error) { if (generation === ticket) { lastSearch = null; say(String(error)); } }
    finally { if (generation === ticket) { busy = false; update(); } }
  }
  el('Connect').addEventListener('click', async () => {
    const ticket = generation;
    try { await invoke('polarion_connect'); if (generation === ticket) say('Sign in in the Polarion window, return here and press Search. The system browser session is not imported.'); }
    catch (error) { if (generation === ticket) say(String(error)); }
  });
  el('ProjectSearchButton').addEventListener('click', () => search('projects'));
  el('TaskSearchButton').addEventListener('click', () => search('tasks'));
  for (const [id, kind] of [['ProjectQuery', 'projects'], ['TaskQuery', 'tasks']]) {
    el(id).addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); search(kind); } });
  }
  el('Assigned').addEventListener('change', update);
  el('User').addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); search('tasks'); } });
  el('Previous').addEventListener('click', () => search(lastSearch.kind, page - 1, lastSearch));
  el('Next').addEventListener('click', () => search(lastSearch.kind, page + 1, lastSearch));
  el('Add').addEventListener('click', () => {
    if (!selected || busy) return;
    try { addMessage(polarionCommitMessage(selected)); dialog.close(); }
    catch (error) { say(String(error)); }
  });
  dialog.addEventListener('close', () => {
    // Native close events are queued: an immediate reopen owns a new context.
    if (dialog.open) return;
    ++generation; context = null; selected = null;
  });
  return {
    async open(repositoryPath, relativePath = '') {
      if (!repositoryPath || dialog.open) return;
      const ticket = ++generation;
      context = null; project = null; selected = null; busy = true; lastSearch = null;
      el('ProjectQuery').value = ''; el('TaskQuery').value = ''; el('Context').textContent = ''; clearResults();
      dialog.showModal(); say('Reading the repository’s saved Polarion project…');
      try {
        const loaded = await invoke('polarion_project_context', { repositoryPath, relativePath, project: null });
        if (generation !== ticket) return;
        context = loaded; project = loaded.project;
        el('Context').textContent = loaded.repository_path;
        say('Open Connect / Polarion to sign in if needed. Search runs only when requested; adding a task never creates a commit.');
      } catch (error) { if (generation === ticket) say(String(error)); }
      finally { if (generation === ticket) { busy = false; update(); el(project ? 'TaskQuery' : 'ProjectQuery').focus(); } }
    },
  };
}
if (typeof module !== 'undefined' && module.exports) module.exports = { polarionCommitMessage, polarionSearchExplanation };

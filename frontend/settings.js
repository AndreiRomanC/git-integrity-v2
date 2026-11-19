/* Settings has no startup I/O. Local reads happen on open; account checks only
   on explicit clicks. Service/host results never contain or persist tokens. */
(function (root) {
  'use strict';
  const methods = { session: 'Existing GitHub API session', environment_token: 'Token from environment', git_credentials: 'Git credential manager / configured helper', github_cli: 'GitHub CLI', portal_session: 'Polarion window session' };
  const labels = { unchecked: 'Not checked', checking: 'Checking…', connected: 'Connected · last check', sign_in_required: 'Sign-in required', failed: 'Connection not verified' };
  function key(service, host, repository = '') { return JSON.stringify([service, host.toLowerCase(), repository]); }
  function polarionResult(result) {
    if (result.code === 'ok' && result.user_id) return { state: 'connected', login: result.user_id, name: result.user_name || '', method: 'portal_session', detail: 'Signed-in user verified. Document edit permissions are checked in the Polarion workspace.' };
    const state = ['auth_required', 'wrong_origin', 'not_json'].includes(result.code) ? 'sign_in_required' : 'failed';
    const reasons = { auth_required: 'Sign in in the Polarion window, then check again.', wrong_origin: 'Finish sign-in in the Polarion window, then check again.', not_json: 'Complete sign-in in the Polarion window, then check again.', forbidden: 'Polarion refused this account check. Verify access with your administrator.', timeout: 'Polarion did not respond in time. Retry when the connection is available.', network_error: 'Cannot reach Polarion. Check the company network/VPN.' };
    return { state, method: 'portal_session', detail: reasons[result.code] || 'The signed-in Polarion user could not be verified.' };
  }
  function diagnosticSummary(info) {
    // Deliberately excludes repository paths, accounts, email, credentials and
    // server responses. The log itself is opened separately, never auto-uploaded.
    return ['Git DrillDown ' + info.version, 'Build: ' + info.build, 'Platform: ' + info.platform, 'Git: ' + info.git_version, 'Git executable: ' + info.git_path].join('\n');
  }
  function create({ invoke, getRepositoryPath, copy, polarionConnection = null, rememberedPolarion = () => null, rememberPolarion = () => {} }) {
    const dialog = document.querySelector('#settingsDialog');
    const el = name => dialog.querySelector('[data-settings="' + name + '"]');
    const tabs = [...dialog.querySelectorAll('[data-settings-tab]')];
    const results = new Map(), pending = new Set();
    let generation = 0, context = null, host = '', localLoading = false, diagnostics = null, diagnosticsPending = false;
    const alive = ticket => dialog.open && ticket === generation;
    const accountKey = service => key(service, service === 'github' ? host : 'polarion.vitesco.io', service === 'github' ? context || '' : '');
    const text = (name, value) => { el(name).textContent = value; };
    function account(service) {
      const result = service === 'polarion' && polarionConnection ? polarionConnection.snapshot() : results.get(accountKey(service));
      const busy = pending.has(accountKey(service));
      const state = busy ? 'checking' : result?.state || 'unchecked';
      text(service + '-state', labels[state] || labels.failed);
      el(service + '-state').dataset.state = state;
      text(service + '-user', result?.login ? result.login + (result.name && result.name !== result.login ? ' · ' + result.name : '') : service === 'polarion' && result?.state === 'connected' ? 'Current user not identified' : 'Account not verified');
      text(service + '-method', methods[result?.method] || (service === 'polarion' ? methods.portal_session : 'Detected when you check or sign in. Existing GitHub credentials are reused.'));
      text(service + '-detail', busy ? (result?.interactive ? 'Complete sign-in in the Git credential manager or browser window. You can close Settings while waiting.' : 'Checking this account…') : result?.detail || (service === 'github' ? (localLoading ? 'Reading repository settings…' : host ? 'Check to identify the account used by GitHub features.' : context ? 'No supported GitHub remote in this repository.' : 'Open a repository to see its GitHub connection.') : 'This app’s Polarion window has its own session; Edge sign-in is separate.'));
      text(service + '-checked', result?.checkedAt ? 'Last check: ' + new Date(result.checkedAt).toLocaleString() : '');
      el(service + '-check').disabled = !invoke || busy || (service === 'github' && (!host || localLoading));
      el(service + '-connect').disabled = !invoke || busy || (service === 'github' && (!host || localLoading));
      if (service === 'github') el('github-open').disabled = !invoke || !host;
    }
    function showAccounts() { account('github'); account('polarion'); }
    polarionConnection?.subscribe(() => { if (dialog.open) account('polarion'); });
    function setTab(name) {
      el('body').scrollTop = 0;
      for (const tab of tabs) { const selected = tab.dataset.settingsTab === name; tab.setAttribute('aria-selected', String(selected)); tab.tabIndex = selected ? 0 : -1; }
      el('connections').hidden = name !== 'connections';
      el('diagnostics').hidden = name !== 'diagnostics';
      if (name === 'diagnostics') loadDiagnostics();
    }
    async function open() {
      const ticket = ++generation;
      context = getRepositoryPath() || null; host = ''; localLoading = true;
      text('context', context || 'No repository open');
      el('context').title = context || '';
      text('notice', invoke ? '' : 'Connections are available in the desktop app.');
      text('identity-name', 'Loading…'); text('identity-email', 'Loading…'); text('identity-detail', '');
      el('github-hosts').replaceChildren(); el('github-hosts-label').hidden = true;
      text('github-host', '');
      const saved = rememberedPolarion();
      text('polarion-remembered', saved ? 'Remembered user: ' + saved.id + ' · not a connection check' : '');
      setTab('connections'); showAccounts();
      if (!dialog.open) dialog.showModal();
      if (!invoke) { localLoading = false; showAccounts(); text('identity-name', 'Desktop app required'); text('identity-email', '—'); return; }
      try {
        const result = await invoke('settings_local', { repositoryPath: context });
        if (!alive(ticket)) return;
        host = result.github_servers[0]?.host || '';
        for (const server of result.github_servers) {
          const option = document.createElement('option'); option.value = server.host; option.textContent = server.host + ' · ' + server.remote; el('github-hosts').append(option);
        }
        el('github-hosts-label').hidden = result.github_servers.length < 2;
        text('github-host', host || '');
        const identity = field => field ? field.value + ' · ' + field.scope : 'Not configured';
        text('identity-name', identity(result.name)); text('identity-email', identity(result.email));
        text('identity-detail', result.identity_error ? 'Some Git identity settings could not be read. No settings were changed.' : (context ? 'Configured for this repository.' : 'Global configuration; no repository open.') + ' This is the commit identity, separate from your GitHub account.');
      } catch (_) {
        if (alive(ticket)) { text('notice', 'Local settings could not be read. Close and reopen Settings to retry.'); text('identity-name', 'Unavailable'); text('identity-email', 'Unavailable'); }
      } finally { if (alive(ticket)) { localLoading = false; showAccounts(); } }
    }
    async function check(service, interactive = false) {
      const capturedHost = host, capturedPath = context, resultKey = accountKey(service);
      if (!invoke || pending.has(resultKey) || (service === 'github' && (!host || localLoading))) return;
      pending.add(resultKey);
      // Starting a new verification clears the old account rather than leaving
      // a green success or a different user's name next to a failed sign-in.
      results.set(resultKey, { state: 'checking', interactive }); showAccounts();
      try {
        const result = service === 'github'
          ? await invoke('settings_check_github', { repositoryPath: capturedPath, host: capturedHost, interactive })
          : polarionConnection ? await polarionConnection.check(invoke) : polarionResult(await invoke('polarion_read', { kind: 'user', resourceId: '', page: 1, revision: null }));
        results.set(resultKey, { ...result, checkedAt: Date.now() });
        if (service === 'polarion' && result.state === 'connected' && result.login) rememberPolarion({ id: result.login, name: result.name, source: 'portal' });
      } catch (error) {
        const closed = service === 'polarion' && String(error).includes('Open Polarion');
        results.set(resultKey, { state: closed ? 'sign_in_required' : 'failed', detail: closed ? 'Open Polarion, finish sign-in if asked, then check again.' : 'The account check did not complete. Retry; details are in the application log.', checkedAt: Date.now() });
      } finally {
        pending.delete(resultKey);
        if (dialog.open) showAccounts();
      }
    }
    async function openPolarion() {
      if (!invoke) return;
      const ticket = generation;
      try {
        el('polarion-connect').disabled = true;
        await invoke('polarion_connect');
        results.delete(accountKey('polarion'));
        if (alive(ticket)) { showAccounts(); text('polarion-detail', 'Polarion window opened. Finish sign-in if asked, then use Check connection to verify access.'); }
      } catch (_) { if (alive(ticket)) text('notice', 'Could not open Polarion. Retry from the Polarion workspace.'); }
      finally { if (alive(ticket)) el('polarion-connect').disabled = false; }
    }
    async function loadDiagnostics() {
      if (diagnostics || diagnosticsPending || !invoke) return;
      diagnosticsPending = true; text('diagnostics-state', 'Reading local application information…');
      try {
        diagnostics = await invoke('settings_diagnostics');
        for (const field of ['version', 'build', 'platform', 'git_version', 'git_path', 'log_path']) text(field, diagnostics[field] || 'Unavailable');
        text('diagnostics-state', ''); el('copy').disabled = false;
      } catch (_) { text('diagnostics-state', 'Could not read diagnostics. Switch tabs to retry.'); }
      finally { diagnosticsPending = false; }
    }
    document.querySelector('#openSettings').addEventListener('click', open);
    dialog.addEventListener('close', () => { generation++; });
    dialog.querySelectorAll('[data-settings-close]').forEach(button => button.addEventListener('click', () => dialog.close()));
    for (const tab of tabs) {
      tab.addEventListener('click', () => setTab(tab.dataset.settingsTab));
      tab.addEventListener('keydown', event => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
        event.preventDefault();
        const next = event.key === 'Home' ? tabs[0] : event.key === 'End' ? tabs.at(-1) : tabs[(tabs.indexOf(tab) + 1) % tabs.length];
        setTab(next.dataset.settingsTab); next.focus();
      });
    }
    el('github-hosts').addEventListener('change', () => { host = el('github-hosts').value; text('github-host', host); account('github'); });
    el('github-check').addEventListener('click', () => check('github'));
    el('github-connect').addEventListener('click', () => check('github', true));
    el('polarion-check').addEventListener('click', () => check('polarion'));
    el('polarion-connect').addEventListener('click', openPolarion);
    el('github-open').addEventListener('click', async () => {
      if (!invoke || !host) return;
      try { await invoke('open_status_check_url', { url: 'https://' + host + '/' }); }
      catch (_) { text('notice', 'Browser opening could not be confirmed.'); }
    });
    el('logs').disabled = !invoke;
    el('logs').addEventListener('click', async () => {
      if (!invoke) return;
      try { await invoke('settings_open_logs'); }
      catch (_) { text('diagnostics-state', 'Folder opening could not be confirmed. Use the log path shown above.'); }
    });
    el('copy').addEventListener('click', () => { if (diagnostics) copy(diagnosticSummary(diagnostics), 'Diagnostic summary copied.'); });
    return { open };
  }
  const api = { create, key, polarionResult, diagnosticSummary };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.GitDrillDownSettings = api;
})(typeof window === 'undefined' ? globalThis : window);

// One session-only view of actual portal reads. No startup I/O, polling,
// credentials, persisted connection flags or requests on Settings open.
(function (root) {
  'use strict';
  function create({ now = Date.now, log = () => {} } = {}) {
    let result = { state: 'unchecked', method: 'portal_session' }, identityUnavailable = false, pending = null;
    const listeners = new Set();
    const snapshot = () => ({ ...result });
    function publish(next, kind, code) {
      result = { method: 'portal_session', ...next };
      log(`polarion: connection state=${result.state} source=${kind} code=${code} identity_verified=${!!result.login} no_credentials_logged=true`);
      for (const listener of listeners) {
        try { listener(snapshot()); } catch { log('polarion: connection_listener_failed no_server_request=true'); }
      }
    }
    function observe(command, args = {}, data, error) {
      if (!['polarion_read', 'polarion_search'].includes(command)) return;
      const kind = args?.kind;
      if (!['user', 'connection', 'projects', 'tasks', 'documents', 'containers', 'document', 'document_link', 'workitem', 'elements', 'links'].includes(kind)) return;
      if (error) {
        if (String(error).includes('Open Polarion, sign in if needed')) publish({ state: 'sign_in_required', windowClosed: true, checkedAt: now(), detail: 'Open the Polarion window, then check again. Sign in there only if asked.' }, kind, 'window_closed');
        else if (!String(error).includes('A Polarion search is already running.')) publish({ state: 'failed', checkedAt: now(), detail: 'The Polarion read did not complete. Retry; details are in the application log.' }, kind, 'request_failed');
        // A busy bridge is not a failed connection or a signed-out session.
        return;
      }
      const code = data?.code;
      if (['auth_required', 'wrong_origin', 'not_json'].includes(code)) {
        publish({ state: 'sign_in_required', checkedAt: now(), detail: 'Finish sign-in in the Polarion window, then check again.' }, kind, code);
      } else if (kind === 'user' && ['api_unavailable', 'forbidden', 'query_rejected', 'invalid_response'].includes(code)) {
        identityUnavailable = code === 'api_unavailable' && [404, 405, 501].includes(data.http);
        // An unsupported identity endpoint must not revoke working read access.
        publish({ ...result, detail: result.state === 'connected' ? 'Polarion read access verified. Current-user lookup is unavailable through this REST session.' : 'Current-user lookup is unavailable on this server. Check connection to verify read access.' }, kind, code);
      } else if (['network_error', 'timeout'].includes(code) || (kind === 'connection' && code !== 'ok')) {
        publish({ state: 'failed', checkedAt: now(), detail: code === 'forbidden' ? 'Polarion refused this read-access check. Resource permissions may differ.' : code === 'timeout' ? 'Polarion did not respond in time. Retry when the connection is available.' : 'Polarion read access could not be verified. Check the company network/VPN and retry.' }, kind, code || 'invalid_response');
      } else if (code === 'ok' && !data.catalog_cached) {
        if (kind === 'user' && !data.user_id) return;
        if (kind === 'user') identityUnavailable = false;
        const identity = kind === 'user' ? { login: data.user_id, name: data.user_name || '' } : {};
        publish({ state: 'connected', ...identity, checkedAt: now(), detail: identity.login ? 'Signed-in user verified. Document edit permissions are checked separately in the Polarion workspace.' : 'Polarion read access verified. Current user not identified; document edit permissions are checked separately.' }, kind, code);
      }
    }
    async function read(invoke, kind) {
      const args = { kind, resourceId: '', page: 1, revision: null };
      const data = await invoke('polarion_read', args);
      // invoke observes every portal request before returning, including
      // requests made in P. Avoid publishing the same result twice.
      return data;
    }
    function check(invoke) {
      if (pending) return pending;
      pending = (async () => {
        if (!identityUnavailable) {
          const data = await read(invoke, 'user');
          if (data.code === 'ok' && data.user_id) return snapshot();
          if (!['ok', 'api_unavailable', 'forbidden', 'query_rejected', 'invalid_response'].includes(data.code)) return snapshot();
        }
        // One bounded GET, not the full project catalog or task search.
        await read(invoke, 'connection');
        return snapshot();
      })().finally(() => { pending = null; });
      return pending;
    }
    return { snapshot, observe, check, subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); } };
  }
  const api = { create };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.GitDrillDownPolarionConnection = api;
})(typeof window === 'undefined' ? globalThis : window);

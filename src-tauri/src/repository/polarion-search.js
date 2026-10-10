// Executed only on explicit Search. No credentials or raw responses cross the
// bridge: the session token is used inside its own origin and never returned.
(async () => {
  const args = __DDT_ARGUMENTS__;
  let session_token = false;
  const reply = data => {
    const target = new URL('https://polarion.vitesco.io/polarion/__ddt_search_result');
    target.searchParams.set('request', args.request);
    target.searchParams.set('data', JSON.stringify({ session_token, ...data }));
    location.href = target.href; // intercepted and cancelled by the native host
  };
  if (location.origin !== 'https://polarion.vitesco.io') return reply({ code: 'wrong_origin' });
  const abort = new AbortController();
  const timeout = setTimeout(() => abort.abort(), 25000);
  try {
    const headers = { Accept: 'application/json' };
    if (typeof window.getRestApiToken === 'function') {
      const token = window.getRestApiToken();
      if (token) { headers['X-Polarion-REST-Token'] = token; session_token = true; }
    }
    // Never forward the REST token to a redirect target (including SSO).
    const response = await fetch(args.url, { headers, credentials: 'same-origin', redirect: 'manual', signal: abort.signal });
    if (response.type === 'opaqueredirect') return reply({ code: 'auth_required' });
    const http = response.status;
    if (!response.ok) return reply({ http, code: http === 401 ? 'auth_required' : http === 403 ? 'forbidden' : [404,405,501,503].includes(http) ? 'api_unavailable' : http === 400 ? 'query_rejected' : 'http_error' });
    if (!response.headers.get('content-type')?.includes('json')) return reply({ code: 'not_json', http });
    const data = await response.json();
    if (!Array.isArray(data.data)) return reply({ code: 'invalid_response', http });
    const items = data.data.slice(0, 25).filter(item => args.kind === 'projects'
      ? item.type === 'projects'
      : item.type === 'workitems' && item.id?.startsWith(args.project + '/') && (item.attributes?.type?.id || item.attributes?.type) === 'task')
      .map(item => ({ id: args.kind === 'projects' ? item.id : item.id.split('/').pop(), title: String(item.attributes?.name || item.attributes?.title || item.id).slice(0, 500) }));
    reply({ code: data.data.length && !items.length ? 'invalid_response' : 'ok', http, received: data.data.length, items, more: Boolean(data.links?.next) });
  } catch (error) {
    reply({ code: error?.name === 'AbortError' ? 'timeout' : 'network_error' });
  } finally { clearTimeout(timeout); }
})();

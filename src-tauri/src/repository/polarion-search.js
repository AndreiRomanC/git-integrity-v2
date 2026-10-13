// Executed only on explicit Search. No credentials or raw responses cross the
// bridge: the session token is used inside its own origin and never returned.
(async () => {
  const args = __DDT_ARGUMENTS__;
  let session_token = false;
  let prefix_fallback = false;
  const reply = data => {
    const target = new URL('https://polarion.vitesco.io/polarion/__ddt_search_result');
    target.searchParams.set('request', args.request);
    target.searchParams.set('data', JSON.stringify({ session_token, prefix_fallback, ...data }));
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
    const options = { headers, credentials: 'same-origin', redirect: 'manual', signal: abort.signal };
    if (args.kind === 'projects') {
      // Explicit project search only. Do not depend on Lucene leading-wildcard
      // support or case rules. Never present a partial catalog as complete.
      const cacheKey = '__ddtProjectCatalog';
      const token = headers['X-Polarion-REST-Token'];
      const cached = window[cacheKey];
      const catalog_cached = Boolean(token && cached?.token === token && Date.now() - cached.time < 120000);
      let projects = catalog_cached ? cached.items : [];
      let catalog_pages = 0;
      if (!catalog_cached) {
        delete window[cacheKey];
        const seen = new Set();
        for (let page = 1; ; page++) {
          if (page > 200) return reply({ code: 'catalog_limit', catalog_pages });
          const url = new URL(args.url);
          url.searchParams.set('page[number]', String(page));
          url.searchParams.set('sort', 'id');
          const response = await fetch(url.href, options);
          catalog_pages++;
          if (response.type === 'opaqueredirect') return reply({ code: 'auth_required', catalog_pages });
          const http = response.status;
          if (!response.ok) return reply({ http, catalog_pages, code: http === 401 ? 'auth_required' : http === 403 ? 'forbidden' : [404,405,501,503].includes(http) ? 'api_unavailable' : 'http_error' });
          if (!response.headers.get('content-type')?.includes('json')) return reply({ code: 'not_json', http, catalog_pages });
          const data = await response.json();
          if (!Array.isArray(data.data) || data.data.some(item => item.type !== 'projects' || typeof item.id !== 'string')) return reply({ code: 'invalid_response', http, catalog_pages });
          let added = 0;
          for (const item of data.data) {
            if (seen.has(item.id)) continue;
            seen.add(item.id); added++;
            projects.push({ id: item.id, title: String(item.attributes?.name || item.id).slice(0, 500), prefix: String(item.attributes?.trackerPrefix || '') });
          }
          if (!data.links?.next) break;
          if (!added) return reply({ code: 'invalid_response', http, catalog_pages });
        }
        // Cache only a complete catalog, in this portal session, for two minutes.
        // No token or credential crosses the native bridge or is persisted here.
        if (token) window[cacheKey] = { token, time: Date.now(), items: projects };
      }
      const words = String(args.query || '').toLowerCase().trim().split(/\s+/).filter(Boolean);
      const matches = projects.filter(item => words.every(word => `${item.id} ${item.title} ${item.prefix}`.toLowerCase().includes(word)));
      const start = ((args.page || 1) - 1) * 25;
      return reply({ code: 'ok', http: 200, catalog_cached, catalog_pages, received: projects.length,
        items: matches.slice(start, start + 25).map(({ id, title }) => ({ id, title })), more: matches.length > start + 25 });
    }
    let response = await fetch(args.url, options);
    if (response.status === 400 && args.fallbackUrl && args.fallbackUrl !== args.url) {
      prefix_fallback = true;
      response = await fetch(args.fallbackUrl, options);
    }
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

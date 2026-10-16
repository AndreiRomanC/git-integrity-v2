// Read-only requests run in the authenticated portal on explicit UI actions.
// Only bounded allowlisted data leaves it; never credentials or raw HTML.
(async () => {
  const args = __DDT_ARGUMENTS__;
  let session_token = false, prefix_fallback = false;
  const reply = data => {
    const target = new URL('https://polarion.vitesco.io/polarion/__ddt_search_result');
    target.searchParams.set('request', args.request);
    let payload = JSON.stringify({ session_token, prefix_fallback, ...data });
    // Stay below the native bridge's UTF-8 limit. Return a useful error
    // instead of silently losing an oversized reply and timing out.
    if (payload.length > 20000 && encodeURIComponent(payload).replace(/%[0-9A-F]{2}/g, 'x').length > 60000) payload = JSON.stringify({ code: 'response_limit', session_token });
    target.searchParams.set('data', payload);
    location.href = target.href; // intercepted/cancelled locally, never sent
  };
  if (location.origin !== 'https://polarion.vitesco.io') return reply({ code: 'wrong_origin' });
  const abort = new AbortController();
  const catalog = args.kind === 'projects' || args.kind === 'documents';
  const timeout = setTimeout(() => abort.abort(), catalog ? 90000 : 25000);
  const text = (value, limit = 500) => String(value ?? '').slice(0, limit);
  const enumValue = value => text(value && typeof value === 'object' ? value.id || value.name : value, 240);
  const validId = (id, count) => typeof id === 'string' && id.split('/').length === count && id.split('/').every(p => p && p !== '.' && p !== '..' && p.length <= 240 && !/[\x00-\x1f\\]/.test(p));
  const hasErrors = item => Boolean(item?.meta?.errors?.length);
  const snapshot = (item, summary = false) => {
    const a = item.attributes || {}, kind = item.type === 'documents' ? 'document' : 'workitem';
    const fields = {};
    for (const key of ['created', 'updated', 'plannedStart', 'plannedEnd', 'dueDate', 'startDate', 'endDate', 'actualStart', 'actualEnd', 'resolvedOn', 'resolution', 'priority', 'initialEstimate', 'remainingEstimate', 'timeSpent', 'moduleFolder', 'moduleName', 'outlineNumber']) {
      if (a[key] != null) fields[key] = enumValue(a[key]);
    }
    if (item.revision) fields.revision = text(item.revision, 120);
    const assignee = item.relationships?.assignee;
    if (assignee?.meta?.totalCount > (assignee?.data?.length || 0)) fields.assigneeNote = 'The server returned only part of the assignee list.';
    const raw = summary ? '' : text(a.description?.value ?? a.description ?? '', 12000);
    if (!summary && String(a.description?.value ?? a.description ?? '').length > 12000) fields.descriptionNote = 'Description truncated to 12000 characters in this local overview.';
    const description = raw.replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, '').replace(/<br\s*\/?\s*>|<\/p>/gi, '\n').replace(/<[^>]*>/g, '').replace(/&(?:nbsp|amp|lt|gt|quot);/g, entity => ({ '&nbsp;': ' ', '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"' }[entity]));
    return { id: item.id, title: text(a.title || a.moduleName || item.id), kind, item_type: enumValue(a.type), status: enumValue(a.status),
      assignees: (Array.isArray(assignee?.data) ? assignee.data : []).slice(0, 100).map(u => text(u.id, 240)), fields, description,
      fetched_at: new Date().toISOString() };
  };
  try {
    const headers = { Accept: 'application/json' };
    if (typeof window.getRestApiToken === 'function') {
      const token = window.getRestApiToken();
      if (token) { headers['X-Polarion-REST-Token'] = token; session_token = true; }
    }
    const options = { method: 'GET', headers, credentials: 'same-origin', redirect: 'manual', signal: abort.signal };
    const request = async url => {
      const parsed = new URL(url);
      if (parsed.origin !== location.origin || !parsed.pathname.startsWith('/polarion/rest/v1/')) throw { code: 'invalid_response' };
      const response = await fetch(parsed.href, options), http = response.status;
      if (response.type === 'opaqueredirect') throw { code: 'auth_required' };
      if (!response.ok) throw { http, code: http === 401 ? 'auth_required' : http === 403 ? 'forbidden' : [404,405,501,503].includes(http) ? 'api_unavailable' : http === 400 ? 'query_rejected' : 'http_error' };
      if (!response.headers.get('content-type')?.includes('json')) throw { http, code: 'not_json' };
      const data = await response.json();
      if (hasErrors(data)) throw { http, code: 'invalid_response' };
      return data;
    };
    if (catalog) {
      const isProject = args.kind === 'projects', key = isProject ? '__ddtProjectCatalog' : '__ddtDocumentCatalog_' + args.project;
      const token = headers['X-Polarion-REST-Token'], cached = window[key];
      const catalog_cached = Boolean(token && cached?.token === token && Date.now() - cached.time < 120000);
      let items = catalog_cached ? cached.items : [], catalog_pages = 0;
      if (!catalog_cached) {
        delete window[key];
        const seen = new Set();
        for (let page = 1; ; page++) {
          if (page > 200) throw { code: 'catalog_limit', catalog_pages };
          const url = new URL(args.url); url.searchParams.set('page[number]', String(page)); url.searchParams.set('sort', 'id');
          const data = await request(url.href); catalog_pages++;
          if (!Array.isArray(data.data) || data.data.some(item => hasErrors(item) || (isProject ? item.type !== 'projects' || !/^[\w-]+$/.test(item.id) : item.type !== 'documents' || !validId(item.id, 3) || !item.id.startsWith(args.project + '/')))) throw { code: 'invalid_response', catalog_pages };
          let added = 0;
          for (const item of data.data) {
            if (seen.has(item.id)) continue;
            seen.add(item.id); added++;
            const a = item.attributes || {};
            items.push({ id: item.id, title: text(isProject ? a.name || item.id : a.title || a.moduleName || item.id), prefix: text(a.trackerPrefix), item_type: enumValue(a.type) });
          }
          if (!data.links?.next) break;
          if (!added) throw { code: 'invalid_response', catalog_pages };
        }
        if (token) window[key] = { token, time: Date.now(), items };
      }
      const words = String(args.query || '').toLowerCase().trim().split(/\s+/).filter(Boolean);
      const matches = items.filter(item => words.every(word => (item.id + ' ' + item.title + ' ' + item.prefix).toLowerCase().includes(word)));
      const start = ((args.page || 1) - 1) * 25;
      return reply({ code: 'ok', http: 200, catalog_cached, catalog_pages, received: items.length, matched: matches.length,
        items: matches.slice(start, start + 25).map(({ id, title, item_type }) => ({ id, title, item_type })), more: matches.length > start + 25 });
    }
    let data;
    try { data = await request(args.url); }
    catch (error) {
      if (error.http !== 400 || !args.fallbackUrl || args.fallbackUrl === args.url) throw error;
      prefix_fallback = true; data = await request(args.fallbackUrl);
    }
    if (args.kind === 'user') {
      if (data.data?.type !== 'users' || typeof data.data.id !== 'string' || !/^[\w.@-]{1,120}$/.test(data.data.id) || hasErrors(data.data)) throw { code: 'invalid_response' };
      return reply({ code: 'ok', http: 200, user_id: data.data.id, user_name: text(data.data.attributes?.name) });
    }
    if (args.kind === 'workitem' || args.kind === 'document') {
      const item = data.data;
      if (!item || item.id !== args.resourceId || item.type !== (args.kind === 'document' ? 'documents' : 'workitems') || hasErrors(item)) throw { code: 'invalid_response' };
      return reply({ code: 'ok', http: 200, snapshot: snapshot(item) });
    }
    if (!Array.isArray(data.data)) throw { code: 'invalid_response' };
    if (args.kind === 'elements' || args.kind === 'links') {
      if (data.data.length > 25 || (data.included != null && !Array.isArray(data.included))) throw { code: 'invalid_response' };
      const elements = [], included = new Map((data.included || []).filter(i => i.type === 'workitems').map(i => [i.id, i]));
      let incomplete = 0;
      for (const part of data.data.slice(0, 25)) {
        if (hasErrors(part) || part.type !== (args.kind === 'elements' ? 'document_parts' : 'linkedworkitems')) throw { code: 'invalid_response' };
        if (args.kind === 'elements' && !part.id?.startsWith(args.resourceId + '/')) throw { code: 'invalid_response' };
        if (args.kind === 'elements' && part.attributes?.type !== 'workitem') continue;
        if (args.kind === 'links' && part.relationships?.sourceWorkItem?.data?.id !== args.resourceId) throw { code: 'invalid_response' };
        const related = part.relationships?.workItem?.data;
        if (!related || related.type !== 'workitems' || !validId(related.id, 2)) throw { code: 'invalid_response' };
        const item = included.get(related.id), available = item && !hasErrors(item);
        if (!available) incomplete++;
        const saved = snapshot(available ? item : { id: related.id, type: 'workitems' }, true);
        if (!available) saved.fields.details = 'Details not returned by Polarion; open this item to read them.';
        if (args.kind === 'elements' && Number.isInteger(part.attributes?.level)) saved.fields.level = String(part.attributes.level);
        if (args.kind === 'links') saved.fields.linkRole = enumValue(part.attributes?.role);
        elements.push(saved);
      }
      return reply({ code: 'ok', http: 200, elements, page_key: data.data.slice(0, 25).map(p => p.id).join('|'), received: data.data.length, incomplete, more: Boolean(data.links?.next) });
    }
    if (data.data.length > 25) throw { code: 'invalid_response' };
    const items = data.data.filter(item => item.type === 'workitems' && validId(item.id, 2) && item.id.startsWith(args.project + '/') && !hasErrors(item)
      && (args.kind === 'containers' || enumValue(item.attributes?.type) === 'task'))
      .map(item => ({ id: item.id.split('/').pop(), title: text(item.attributes?.title || item.id), item_type: enumValue(item.attributes?.type) }));
    reply({ code: data.data.length && !items.length ? 'invalid_response' : 'ok', http: 200, received: data.data.length, items, more: Boolean(data.links?.next) });
  } catch (error) {
    reply({ code: error?.code || (error?.name === 'AbortError' ? 'timeout' : 'network_error'), http: error?.http || 0, catalog_pages: error?.catalog_pages || 0 });
  } finally { clearTimeout(timeout); }
})();

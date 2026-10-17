// Read-only requests run in the authenticated portal on explicit UI actions.
// Only bounded allowlisted data leaves it; never credentials or raw HTML.
(async () => {
  const args = __DDT_ARGUMENTS__;
  let session_token = false, prefix_fallback = false, metadata_fallback = false;
  let http_requests = 0, diagnostics_omitted = 0;
  const diagnostics = [];
  let server_metadata;
  const trace = entry => {
    // Keep the first context and the latest failure even in long catalogs.
    if (diagnostics.length === 24) { diagnostics.pop(); diagnostics_omitted++; }
    diagnostics.push(entry);
  };
  const reply = data => {
    const target = new URL('https://polarion.vitesco.io/polarion/__ddt_search_result');
    target.searchParams.set('request', args.request);
    const compatibility = () => metadata_fallback ? { metadata_fallback: true } : {};
    const serialize = () => JSON.stringify({ session_token, prefix_fallback, ...compatibility(), diagnostics, diagnostics_omitted, http_requests, server_metadata, ...data });
    const oversized = value => value.length > 20000 && encodeURIComponent(value).replace(/%[0-9A-F]{2}/g, 'x').length > 60000;
    let payload = serialize();
    // Stay below the native bridge's UTF-8 limit. Return a useful error
    // instead of timing out. Diagnostics must not displace previously valid
    // search results or snapshots: reduce diagnostic detail first.
    if (oversized(payload) && diagnostics.length > 1) {
      diagnostics_omitted += diagnostics.length - 1;
      diagnostics.splice(0, diagnostics.length - 1); payload = serialize();
    }
    if (oversized(payload) && diagnostics.length) {
      diagnostics_omitted += diagnostics.length; diagnostics.length = 0; payload = serialize();
    }
    if (oversized(payload)) {
      // At the exact old size boundary even the diagnostic counters may not
      // fit. Preserve the legacy reply instead of breaking a successful read.
      const legacy = JSON.stringify({ session_token, prefix_fallback, ...compatibility(), ...data });
      payload = !oversized(legacy) ? legacy : JSON.stringify({ code: 'response_limit', session_token, diagnostics, diagnostics_omitted, http_requests });
    }
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
  const route = url => {
    const p = url.pathname.split('/').slice(4);
    if (p.length === 2 && p[0] === 'all' && p[1] === 'documents') return 'all_documents';
    if (p.length === 1 && ['metadata', 'user', 'projects'].includes(p[0])) return p[0];
    if (p[0] !== 'projects' || !p[1]) return 'unknown';
    if (p.length === 2) return 'project';
    if (p[2] === 'workitems') return p.length === 3 ? 'workitems' : p.length === 4 ? 'workitem' : p.length === 5 && p[4] === 'linkedworkitems' ? 'links' : 'unknown';
    if (p.length === 3 && ['documents', 'spaces'].includes(p[2])) return p[2];
    if (p[2] !== 'spaces' || !p[3]) return 'unknown';
    if (p.length === 5 && ['documents', 'pages'].includes(p[4])) return 'space_' + p[4];
    if (p.length === 6 && ['documents', 'pages'].includes(p[4])) return p[4] === 'documents' ? 'document' : 'page';
    return p.length === 7 && p[4] === 'documents' && p[6] === 'parts' ? 'parts' : 'unknown';
  };
  const resourceTypes = ['metadata', 'users', 'projects', 'workitems', 'documents', 'pages', 'spaces', 'document_parts', 'linkedworkitems'];
  const safeFields = ['id','name','title','type','status','moduleName','moduleFolder','created','updated','description','assignee','module','workItem','sourceWorkItem','level','revision','role','nextPart','previousPart','@all'];
  const safeRelationships = ['assignee','module','workItem','sourceWorkItem','nextPart','previousPart'];
  const technicalType = value => {
    const id = value && typeof value === 'object' ? value.id : value;
    return typeof id === 'string' && /^[A-Za-z][A-Za-z0-9_.-]{0,79}$/.test(id) ? id : '';
  };
  const responseFacts = data => {
    const rows = (Array.isArray(data.data) ? data.data : data.data ? [data.data] : []).slice(0, 10);
    const included = Array.isArray(data.included) ? data.included.slice(0, 10) : [];
    const fields = new Set(), relationships = new Set(), types = new Set();
    let module_links = 0, workitem_links = 0;
    for (const row of [...rows, ...included]) {
      if (!resourceTypes.includes(row?.type)) continue;
      for (const key of Object.keys(row.attributes || {})) if (safeFields.includes(key)) fields.add(row.type + '.' + key);
      for (const key of Object.keys(row.relationships || {})) if (safeRelationships.includes(key)) relationships.add(row.type + '.' + key);
      if (['workitems', 'documents', 'document_parts'].includes(row.type)) {
        const type = technicalType(row.attributes?.type); if (type) types.add(type);
      }
    }
    for (const row of rows) {
      if (row?.relationships?.module?.data?.type === 'documents') module_links++;
      if (row?.relationships?.workItem?.data?.type === 'workitems') workitem_links++;
    }
    return { shape: Array.isArray(data.data) ? 'list' : data.data ? 'single' : 'missing', sample_rows: rows.length,
      returned_fields: [...fields].sort().slice(0, 32), relationship_fields: [...relationships].sort().slice(0, 32),
      item_types: [...types].sort().slice(0, 12), module_links, workitem_links,
      included_workitems: included.filter(row => row?.type === 'workitems' && !hasErrors(row)).length };
  };
  // Error bodies are used only to derive fixed categories. No message, URL,
  // header value, query text or response content is sent to the desktop log.
  const errorInfo = async response => {
    if (!response.headers.get('content-type')?.includes('json')) return {};
    let body;
    try {
      if (response.body?.getReader) {
        const reader = response.body.getReader(); let size = 0, chunks = [];
        try {
          for (;;) {
            const { done, value } = await reader.read(); if (done) break;
            size += value.byteLength; if (size > 8192) return { reason: 'error_body_limit' };
            chunks.push(value);
          }
          const bytes = new Uint8Array(size); let offset = 0;
          for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
          body = JSON.parse(new TextDecoder().decode(bytes));
        } finally { await reader.cancel().catch(() => {}); }
      } else body = await response.json(); // Test adapters; real Fetch uses the bounded stream above.
    } catch { return {}; }
    const errors = Array.isArray(body?.errors) ? body.errors.slice(0, 3) : [];
    const words = errors.map(e => String(e.title || '').slice(0, 500) + ' ' + String(e.detail || '').slice(0, 1500)).join(' ').toLowerCase();
    const source = errors.find(e => e.source?.parameter)?.source?.parameter || '';
    const parameter = source === 'query' ? 'query' : /^fields\[/.test(source) ? 'fields' : /^page\[/.test(source) ? 'page' : ['sort','include','revision'].includes(source) ? source : source ? 'other' : '';
    const reason = /(?:leading|first character|start[^.]{0,30})[^.]{0,70}(?:wildcard|\*|\?)|wildcard[^.]{0,70}(?:leading|first|start)/.test(words) ? 'leading_wildcard'
      : /(?:unknown|invalid|unsupported|unrecognized)[^.]{0,60}(?:field)|field[^.]{0,60}(?:unknown|invalid|unsupported|not found)/.test(words) ? 'unknown_field'
      : /(?:query|lucene|parse|syntax)/.test(words) ? 'query_syntax'
      : /(?:unknown|invalid|unsupported)[^.]{0,60}parameter/.test(words) ? 'unsupported_parameter'
      : /(?:permission|forbidden|not allowed|access denied)/.test(words) ? 'permission_denied'
      : /(?:unauthoriz|authentication|sign.in)/.test(words) ? 'authentication_required'
      : /(?:not found|does not exist|no such)/.test(words) ? 'not_found' : 'unclassified';
    return { reason, parameter };
  };
  const snapshot = (item, summary = false) => {
    const a = item.attributes || {}, kind = item.type === 'documents' ? 'document' : 'workitem';
    const fields = {};
    for (const key of ['created', 'updated', 'plannedStart', 'plannedEnd', 'dueDate', 'startDate', 'endDate', 'actualStart', 'actualEnd', 'resolvedOn', 'resolution', 'priority', 'initialEstimate', 'remainingEstimate', 'timeSpent', 'moduleFolder', 'moduleName', 'outlineNumber']) {
      if (a[key] != null) fields[key] = enumValue(a[key]);
    }
    if (item.revision) fields.revision = text(item.revision, 120);
    if (metadata_fallback && kind === 'document') fields.compatibilityNote = 'This server rejected optional metadata fields. The document was read with title only; unavailable fields are not inferred.';
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
    const request = async (url, step = 'primary', bounded = false) => {
      const parsed = new URL(url);
      if (parsed.origin !== location.origin || !parsed.pathname.startsWith('/polarion/rest/v1/')) throw { code: 'invalid_response' };
      const started = Date.now(), query = parsed.searchParams.get('query') || '';
      const entry = { step, route: route(parsed), code: 'network_error', http: 0, page: Number(parsed.searchParams.get('page[number]') || 1), query: Boolean(query), id_star: /\bid:\*/.test(query), leading_wildcard: /\(\*|AND \*/.test(query), media: '', reason: '', parameter: '', resource_type: '', count: 0, more: false, allow: [] };
      entry.query_kind = !query ? 'none' : /^module\.id:/.test(query) ? 'module_name' : /^project\.id:/.test(query) ? 'project_scope' : /(?:id|title):/.test(query) ? 'work_item_search' : 'other';
      entry.requested_fields = [];
      for (const [key, value] of parsed.searchParams) {
        const resource = /^fields\[([a-z_]+)\]$/.exec(key)?.[1];
        if (resourceTypes.includes(resource)) for (const field of value.split(',')) if (safeFields.includes(field)) entry.requested_fields.push(resource + '.' + field);
      }
      entry.requested_fields = [...new Set(entry.requested_fields)].sort().slice(0, 32);
      const controller = new AbortController(), cancel = () => controller.abort();
      abort.signal.addEventListener('abort', cancel, { once: true });
      if (abort.signal.aborted) controller.abort();
      const timer = bounded ? setTimeout(cancel, 5000) : null;
      http_requests++;
      try {
        const response = await fetch(parsed.href, { ...options, signal: controller.signal });
        const http = entry.http = response.status, media = response.headers.get('content-type') || '';
        entry.media = /json/i.test(media) ? 'json' : /html/i.test(media) ? 'html' : /text\//i.test(media) ? 'text' : media ? 'other' : '';
        entry.allow = String(response.headers.get('allow') || '').split(',').map(m => m.trim().toUpperCase()).filter(m => ['GET','HEAD','OPTIONS','POST','PATCH','PUT','DELETE'].includes(m)).slice(0, 7);
        if (response.type === 'opaqueredirect') throw { http, code: 'auth_required' };
        if (!response.ok) {
          Object.assign(entry, await errorInfo(response));
          if (http === 405) entry.reason = 'method_not_allowed';
          throw { http, code: http === 401 ? 'auth_required' : http === 403 ? 'forbidden' : [404,405,501,503].includes(http) ? 'api_unavailable' : http === 400 ? 'query_rejected' : 'http_error' };
        }
        if (entry.media !== 'json') throw { http, code: 'not_json' };
        const data = await response.json();
        const resource = Array.isArray(data.data) ? data.data[0] : data.data;
        entry.resource_type = resourceTypes.includes(resource?.type) ? resource.type : '';
        entry.count = Array.isArray(data.data) ? data.data.length : data.data ? 1 : 0;
        entry.more = Boolean(data.links?.next);
        Object.assign(entry, responseFacts(data));
        if (hasErrors(data)) { entry.reason = 'partial_errors'; throw { http, code: 'invalid_response' }; }
        entry.code = 'ok';
        return data;
      } catch (error) {
        entry.code = error?.code || (error?.name === 'AbortError' ? 'timeout' : 'network_error');
        throw { code: entry.code, http: entry.http };
      } finally {
        clearTimeout(timer); abort.signal.removeEventListener('abort', cancel);
        entry.duration_ms = Math.min(600000, Math.max(0, Date.now() - started)); trace(entry);
      }
    };
    if (args.kind === 'diagnostics') {
      const started = Date.now(), confirmed = new Set(); let sampleId = '';
      for (const probe of args.probes || []) {
        let probeUrl = probe.url;
        if (probe.step === 'workitem_sample' && sampleId) {
          const url = new URL(probe.url), [project, item] = sampleId.split('/');
          url.pathname = '/polarion/rest/v1/projects/' + encodeURIComponent(project) + '/workitems/' + encodeURIComponent(item); probeUrl = url.href;
        }
        if (Date.now() - started >= 25000 || abort.signal.aborted || (probe.requires && !confirmed.has(probe.requires))) {
          const parsed = new URL(probeUrl);
          trace({ step: probe.step, route: route(parsed), code: 'not_run', reason: probe.requires && !confirmed.has(probe.requires) ? 'prerequisite_missing' : 'budget', duration_ms: 0 }); continue;
        }
        if (probe.step === 'workitem_sample' && !sampleId) {
          trace({ step: probe.step, route: 'workitem', code: 'not_run', reason: 'sample_missing', duration_ms: 0 }); continue;
        }
        try {
          const data = await request(probeUrl, probe.step, true), last = diagnostics[diagnostics.length - 1];
          const expected = ['document_exact', 'document_parts', 'document_parts_expanded', 'page_exact', 'metadata', 'current_user', 'project_access'].includes(probe.step)
            ? ({ document_exact:'documents', document_parts:'document_parts', document_parts_expanded:'document_parts', page_exact:'pages', metadata:'metadata', current_user:'users', project_access:'projects' })[probe.step]
            : ['documents_catalog','documents_global','space_documents'].includes(probe.step) ? 'documents' : probe.step === 'space_pages' ? 'pages' : probe.step === 'spaces' ? 'spaces' : 'workitems';
          const single = ['document_exact', 'page_exact', 'metadata', 'current_user', 'project_access', 'workitem_sample'].includes(probe.step);
          const rows = Array.isArray(data.data) ? data.data : data.data ? [data.data] : null;
          if (!rows || single === Array.isArray(data.data) || rows.some(item => item.type !== expected || hasErrors(item))) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
          if (['workitem_modules', 'document_name_query'].includes(probe.step) && rows.some(row => !validId(row.id, 2) || !row.id.startsWith(args.project + '/') ||
            !row.relationships?.module?.data || row.relationships.module.data.type !== 'documents' || !validId(row.relationships.module.data.id, 3))) {
            last.code = 'invalid_response'; last.reason = 'relationship_missing'; continue;
          }
          if (['document_exact', 'page_exact'].includes(probe.step)) {
            if (data.data?.id !== args.resourceId) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
            if (probe.step === 'document_exact') confirmed.add(probe.step);
          }
          if (probe.step === 'workitem_sample' && data.data?.id !== sampleId) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
          if (probe.step === 'document_parts_expanded') {
            if (rows.length > 10 || rows.some(row => !row.id?.startsWith(args.resourceId + '/'))) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
            confirmed.add(probe.step);
            sampleId = rows.map(row => row.attributes?.type === 'workitem' ? row.relationships?.workItem?.data : null)
              .find(ref => ref?.type === 'workitems' && validId(ref.id, 2) && ref.id.startsWith(args.project + '/'))?.id || '';
          }
          if (probe.step === 'metadata') {
            const a = data.data?.attributes || {};
            server_metadata = {
              version: /^\d{1,4}(?:\.\d{1,8}){1,3}$/.test(String(a.version || '')) ? String(a.version) : '',
              build: /^\d{8}-\d{4}(?:-[A-Za-z0-9]{1,24}){0,3}$/.test(String(a.build || '')) ? String(a.build) : '',
              max_page_size: Number.isSafeInteger(a.apiProperties?.maxPageSize) && a.apiProperties.maxPageSize >= 0 && a.apiProperties.maxPageSize <= 1000000 ? a.apiProperties.maxPageSize : 0,
            };
          }
        } catch { /* Each failed GET is reported independently; never try a write. */ }
      }
      return reply({ code: 'ok' }); // Completed checks, NOT a claim that every endpoint is supported.
    }
    if (args.kind === 'documents') {
      // Collection GETs are absent on some older Polarion servers, although
      // exact Document reads and Work Item -> module relationships work.
      // All fallbacks remain project-scoped GETs in this same browser session.
      const key = '__ddtDocumentSearchV2_' + args.project, token = headers['X-Polarion-REST-Token'];
      const queryKey = String(args.query || '').trim().toLowerCase(), cached = window[key];
      const catalog_cached = Boolean(token && cached?.token === token && Date.now() - cached.time < 120000 &&
        (cached.source !== 'work_item_modules' || cached.query === queryKey));
      let items = catalog_cached ? cached.items : [], catalog_source = catalog_cached ? cached.source : '', catalog_pages = 0;
      if (!catalog_cached) {
        delete window[key];
        const sources = [{ source: 'project_documents', url: args.url, step: 'catalog_page' },
          { source: 'all_documents', url: args.documentGlobalUrl, step: 'document_global' },
          { source: 'work_item_modules', url: args.documentModulesUrl, step: 'document_modules' }].filter(s => s.url);
        for (let index = 0; index < sources.length; index++) {
          const source = sources[index], modules = source.source === 'work_item_modules';
          const seenRows = new Set(), documents = new Map();
          let unavailable = false;
          for (let page = 1; ; page++) {
            if (page > 200) throw { code: 'catalog_limit' };
            const url = new URL(source.url); url.searchParams.set('page[number]', String(page));
            let data;
            try { data = await request(url.href, source.step); }
            catch (error) {
              // Do not retry authentication/permission errors, transient server
              // failures, invalid responses or a later failed catalog page.
              if (page === 1 && [404, 405, 501].includes(error.http) && index < sources.length - 1) { unavailable = true; break; }
              throw error;
            }
            catalog_pages++;
            if (!Array.isArray(data.data) || data.data.length > 100) throw { code: 'invalid_response' };
            let added = 0;
            for (const row of data.data) {
              if (hasErrors(row) || row.type !== (modules ? 'workitems' : 'documents') || !validId(row.id, modules ? 2 : 3) ||
                (source.source !== 'all_documents' && !row.id.startsWith(args.project + '/'))) throw { code: 'invalid_response' };
              if (seenRows.has(row.id)) continue;
              seenRows.add(row.id); added++;
              let doc = row;
              if (modules) {
                const relationship = row.relationships?.module;
                // A name query returned matching Work Items but the requested
                // relationship was omitted: don't claim there are no documents.
                if (!relationship || hasErrors(relationship) || !relationship.data) throw { code: 'invalid_response' };
                doc = relationship.data;
                if (doc.type !== 'documents' || !validId(doc.id, 3)) throw { code: 'invalid_response' };
              }
              if (!doc.id.startsWith(args.project + '/')) continue;
              const a = doc.attributes || {};
              documents.set(doc.id, { id: doc.id, title: text(a.title || a.moduleName || doc.id.split('/').pop()), prefix: '', item_type: enumValue(a.type) });
            }
            if (!data.links?.next) break;
            // Many consecutive pages can legitimately refer to the same
            // document. Detect loops using Work Item IDs, not Document IDs.
            if (!added) throw { code: 'invalid_response' };
          }
          if (unavailable) continue;
          catalog_source = source.source; items = [...documents.values()].sort((a, b) => a.id.localeCompare(b.id)); break;
        }
        if (token) window[key] = { token, time: Date.now(), items, source: catalog_source, query: queryKey };
      }
      const words = queryKey.split(/\s+/).filter(Boolean);
      const matches = items.filter(item => words.every(word => (item.id + ' ' + item.title).toLowerCase().includes(word)));
      const start = ((args.page || 1) - 1) * 25;
      return reply({ code: 'ok', http: 200, catalog_source, catalog_cached, catalog_pages, received: items.length, matched: matches.length,
        items: matches.slice(start, start + 25).map(({ id, title, item_type }) => ({ id, title, item_type })), more: matches.length > start + 25 });
    }
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
          const data = await request(url.href, 'catalog_page'); catalog_pages++;
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
      if (args.readFallback) { metadata_fallback = true; data = await request(args.fallbackUrl, 'metadata_fallback'); }
      else { prefix_fallback = true; data = await request(args.fallbackUrl, 'prefix_fallback'); }
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

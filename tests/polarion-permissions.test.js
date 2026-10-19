const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src-tauri/src/repository/polarion-permissions.js'), 'utf8');
const WSDL = 'http://schemas.xmlsoap.org/wsdl/', XSD = 'http://www.w3.org/2001/XMLSchema', SOAP = 'http://schemas.xmlsoap.org/soap/envelope/';
// Tiny fixture-only DOM adapter. Production uses the webview's native XML
// DOMParser. No parser or dependency is added to the application.
class XmlNode {
  constructor(name, attrs = {}, namespaces = {}) {
    this.nodeType = 1; this.localName = name.split(':').pop(); this.attrs = attrs; this.childNodes = [];
    this.ns = { ...namespaces };
    for (const [key, value] of Object.entries(attrs)) if (key === 'xmlns') this.ns[''] = value; else if (key.startsWith('xmlns:')) this.ns[key.slice(6)] = value;
    this.namespaceURI = this.ns[name.includes(':') ? name.split(':')[0] : ''] || '';
  }
  getAttribute(name) { return this.attrs[name] || ''; }
  get textContent() { return this.childNodes.map(n => typeof n === 'string' ? n : n.textContent).join(''); }
  getElementsByTagNameNS(ns, name) {
    return this.childNodes.filter(n => typeof n !== 'string').flatMap(n => [
      ...((ns === '*' || n.namespaceURI === ns) && (name === '*' || n.localName === name) ? [n] : []), ...n.getElementsByTagNameNS(ns, name),
    ]);
  }
}
const decode = s => s.replace(/&(lt|gt|amp|quot|apos);/g, (_, v) => ({ lt:'<',gt:'>',amp:'&',quot:'"',apos:"'" }[v]));
class FixtureDOMParser {
  parseFromString(xml) {
    const root = new XmlNode('#document'), stack = [root];
    for (const token of xml.match(/<[^>]*>|[^<]+/g) || []) {
      if (token.startsWith('<?')) continue;
      if (token.startsWith('</')) { stack.pop(); continue; }
      if (token.startsWith('<')) {
        const name = token.match(/^<([\w:.-]+)/)?.[1];
        if (!name) throw Error('Bad fixture XML');
        const attrs = Object.fromEntries([...token.matchAll(/([\w:.-]+)="([^"]*)"/g)].map(m => [m[1], decode(m[2])]));
        const node = new XmlNode(name, attrs, stack.at(-1).ns); stack.at(-1).childNodes.push(node);
        if (!token.endsWith('/>')) stack.push(node);
      } else stack.at(-1).childNodes.push(decode(token));
    }
    root.documentElement = root.childNodes.find(n => typeof n !== 'string'); return root;
  }
}
const signatures = {
  SessionWebService: { hasSubject: [], logInWithToken: ['mechanism','username','token'] },
  TrackerWebService: { getModuleByLocation: ['projectId','location'], createWorkItemInModule: ['moduleURI','parentWorkItemURI','workItem'], updateModule: ['module'] },
  SecurityWebService: { canModifyInstance: ['uri'], canModifyKey: ['uri','key'], hasCurrentUserPermission: ['permission','projectId'], canCreateInstances: ['prototype','projectId'] },
};
function wsdl(service, changes = {}) {
  const methods = { ...signatures[service], ...changes };
  return `<w:definitions xmlns:w="${WSDL}" xmlns:x="${XSD}"><w:types><x:schema targetNamespace="http://ws.polarion.com/${service}-impl" elementFormDefault="qualified">` +
    Object.entries(methods).map(([name, params]) => `<x:element name="${name}"><x:complexType><x:sequence>${params.map(p => `<x:element name="${p}" type="x:string"/>`).join('')}</x:sequence></x:complexType></x:element>`).join('') +
    `</x:schema></w:types><w:portType>${Object.keys(methods).map(n => `<w:operation name="${n}"/>`).join('')}</w:portType></w:definitions>`;
}
const envelope = content => `<s:Envelope xmlns:s="${SOAP}"><s:Body>${content}</s:Body></s:Envelope>`;
const resultXml = (service, method, value) => envelope(`<p:${method}Response xmlns:p="http://ws.polarion.com/${service}-impl"><p:${method}Return>${value}</p:${method}Return></p:${method}Response>`);
const moduleXml = (name = 'D', project = 'P') => `<moduleName>${name}</moduleName><moduleFolder>Structure Information</moduleFolder><project><id>${project}</id></project><uri>subterra:data-service:objects:/PRIVATE_DOCUMENT_URI</uri><unresolvable>false</unresolvable>`;
async function run(options = {}) {
  const requests = []; let reply;
  const location = { origin: options.origin || 'https://polarion.vitesco.io', set href(url) { reply = JSON.parse(new URL(url).searchParams.get('data')); } };
  const context = { URL, AbortController, TextDecoder, setTimeout, clearTimeout, DOMParser: FixtureDOMParser, location,
    window: { getRestApiToken() { throw Error('Never treat a REST token as a SOAP credential'); } },
    fetch: async (url, args) => {
      requests.push({ url, ...args }); const service = new URL(url).pathname.split('/').pop();
      const method = args.body?.match(/<p:([A-Za-z]+)/)?.[1];
      assert.ok(['hasSubject','getModuleByLocation','canModifyInstance','canModifyKey','hasCurrentUserPermission',undefined].includes(method));
      const response = options.response?.({ url, method, service, args }) || {};
      const data = response.data ?? (method ? resultXml(service, method, method === 'getModuleByLocation' ? moduleXml() : 'true') : wsdl(service));
      const bytes = new TextEncoder().encode(data); let read = false;
      return { status: response.status || 200, ok: (response.status || 200) === 200, type: response.type || 'basic',
        headers: { get: name => response.headers?.[name] || (name === 'content-type' ? 'text/xml' : '') },
        body: { getReader: () => ({ read: async () => read ? { done: true } : (read = true, { value: bytes, done: false }), cancel: async () => {}, releaseLock() {} }) } };
    } };
  await vm.runInNewContext(source.replace('__DDT_ARGUMENTS__', JSON.stringify({ resourceId: 'P/Structure Information/D', request: '1' })), context);
  return { reply, requests };
}
test('permission diagnostics query the actual document, field and project permissions without any write/login transaction', async () => {
  const { reply, requests } = await run();
  assert.equal(reply.code, 'ok'); assert.equal(requests.length, 8);
  assert.equal(reply.permission_checks.find(c => c.step === 'document_edit').allowed, true);
  assert.equal(reply.permission_checks.find(c => c.step === 'document_text').allowed, true);
  assert.match(requests[4].body, /<p:location>Structure%20Information\/D<\/p:location>/);
  assert.match(requests[6].body, /<p:key>homePageContent<\/p:key>/);
  assert.ok(requests.every(r => new URL(r.url).origin === 'https://polarion.vitesco.io' && r.credentials === 'same-origin' && r.redirect === 'manual'));
  assert.ok(requests.every(r => !r.headers['X-Polarion-REST-Token']));
  assert.ok(reply.permission_checks.find(c => c.step === 'tracker_wsdl').operations.includes('createWorkItemInModule'));
  assert.ok(requests.every(r => !/<p:(?:create|update|delete|logIn|beginTransaction)/.test(r.body || '')));
  assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_DOCUMENT_URI|Structure Information|<|"token":|Bearer /i);
});
test('a false permission is denied; SOAP sign-in failure or HTTP refusal remains unknown rather than denied', async () => {
  for (const value of ['false','0']) {
    const { reply } = await run({ response: ({ service, method }) => method === 'canModifyInstance' ? { data: resultXml(service, method, value) } : null });
    assert.equal(reply.permission_checks.find(c => c.step === 'document_edit').allowed, false);
  }
  for (const response of [() => ({ status: 403 }), ({ method }) => method === 'hasSubject' ? { data: envelope('<s:Fault><faultstring>Not authorized. PRIVATE_SECRET</faultstring></s:Fault>'), status: 500 } : null]) {
    const { reply } = await run({ response });
    assert.equal(reply.permission_checks.find(c => c.step === 'document_edit').allowed, undefined);
    assert.doesNotMatch(JSON.stringify(reply), /PRIVATE_SECRET/);
  }
});
test('an unauthenticated SOAP session is reported explicitly and never initiates login or document mutation', async () => {
  const { reply, requests } = await run({ response: ({ service, method }) => method === 'hasSubject' ? { data: resultXml(service, method, 'false') } : null });
  assert.equal(requests.length, 4); assert.equal(reply.permission_checks.find(c => c.step === 'soap_session').allowed, false);
  assert.equal(reply.permission_checks.find(c => c.step === 'document_edit').code, 'not_run');
});
test('wrong document name/project and unresolved identities never produce an edit permission verdict', async () => {
  for (const value of [moduleXml('OTHER'), moduleXml('D', 'OtherProject'), moduleXml().replace('false','true')]) {
    const { reply, requests } = await run({ response: ({ service, method }) => method === 'getModuleByLocation' ? { data: resultXml(service, method, value) } : null });
    assert.equal(reply.permission_checks.find(c => c.step === 'document_resolve').code, 'document_identity_unverified');
    assert.equal(reply.permission_checks.find(c => c.step === 'document_edit').allowed, undefined); assert.equal(requests.length, 5);
  }
});
test('unsupported method signatures, invalid boolean responses and foreign origins are handled conservatively', async () => {
  const mismatch = await run({ response: ({ method, service }) => !method && service === 'SecurityWebService' ? { data: wsdl(service, { canModifyInstance: ['uri','unexpected'] }) } : null });
  assert.equal(mismatch.reply.permission_checks.find(c => c.step === 'document_edit').code, 'unsupported_method');
  for (const value of ['maybe', '<unexpected>true</unexpected>']) {
    const bad = await run({ response: ({ method, service }) => method === 'canModifyKey' ? { data: resultXml(service, method, value) } : null });
    assert.equal(bad.reply.permission_checks.find(c => c.step === 'document_text').code, 'invalid_response');
  }
  const external = await run({ origin: 'https://sso.example.test' }); assert.equal(external.requests.length, 0);
  assert.equal(external.reply.permission_checks[0].code, 'wrong_origin');
});
test('XML entities, oversized/chunked replies and login redirects cannot leak raw content or trigger follow-up writes', async () => {
  for (const response of [() => ({ data: '<!DOCTYPE x [<!ENTITY secret SYSTEM="https://unsafe.example">]><x/>' }),
    () => ({ headers: { 'content-length': '2000001' } }), () => ({ data: 'x'.repeat(2_000_001) }), () => ({ type: 'opaqueredirect' })]) {
    const { reply, requests } = await run({ response });
    assert.ok(reply.permission_checks.some(c => ['invalid_response','response_limit','auth_required'].includes(c.code)));
    assert.ok(requests.every(r => r.method === 'GET'), 'no SOAP calls are sent without an accepted session schema');
    assert.doesNotMatch(JSON.stringify(reply), /unsafe.example|<!DOCTYPE/);
  }
});

// diri.sh/mcp: a read-only MCP server over these docs (streamable HTTP, stateless JSON responses).
// It only reads the static files the build already publishes: docs/search.json and docs/<page>.md.
import { rank } from '../docs-search.js';

const VERSIONS = ['2025-06-18', '2025-03-26', '2024-11-05'];
const SITE = 'https://diri.sh';
const CORS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'POST, GET, OPTIONS',
  'Access-Control-Allow-Headers': 'Content-Type, Accept, Mcp-Protocol-Version, Mcp-Session-Id',
};

const TOOLS = [
  {
    name: 'search_docs',
    description: 'Search the diri documentation (install, sessions, worktrees, notes, MCP server, dirijor CLI, remote hosts, troubleshooting). Returns matching pages and sections with URLs. Read a hit with read_doc.',
    inputSchema: { type: 'object', properties: { query: { type: 'string', minLength: 1 }, limit: { type: 'number', minimum: 1, maximum: 20, default: 8 } }, required: ['query'] },
  },
  {
    name: 'read_doc',
    description: 'Read one diri documentation page as Markdown. page is a slug from list_docs (e.g. "mcp", "cli", "remote-hosts") or a diri.sh/docs URL; "index" is the overview.',
    inputSchema: { type: 'object', properties: { page: { type: 'string', minLength: 1 } }, required: ['page'] },
  },
  {
    name: 'list_docs',
    description: 'List every diri documentation page with its slug, section and one-line description.',
    inputSchema: { type: 'object', properties: {} },
  },
];

const json = (body, status = 200, headers = {}) => new Response(body === null ? null : JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', ...CORS, ...headers } });
const ok = (id, result) => ({ jsonrpc: '2.0', id, result });
const fail = (id, code, message) => ({ jsonrpc: '2.0', id, error: { code, message } });
const text = (value, isError = false) => ({ content: [{ type: 'text', text: typeof value === 'string' ? value : JSON.stringify(value, null, 2) }], isError });

async function asset(env, request, path) {
  const response = await env.ASSETS.fetch(new URL(path, request.url));
  if (!response.ok) return null;
  return response.text();
}

const slugOf = url => url === '/docs/' ? 'index' : url.replace(/^\/docs\/|\/$/g, '');

async function call(name, args, env, request) {
  const index = JSON.parse(await asset(env, request, '/docs/search.json') || '[]');
  if (name === 'list_docs') {
    return text(index.filter(entry => !entry.p).map(entry => ({ page: slugOf(entry.u), title: entry.t, section: entry.g, description: entry.x, url: SITE + entry.u })));
  }
  if (name === 'search_docs') {
    const query = String(args.query || '').trim();
    if (!query) return text('query is required', true);
    const limit = Math.min(Math.max(Number(args.limit) || 8, 1), 20);
    const hits = rank(index, query, limit).map(entry => ({ title: entry.t, page: entry.p || entry.t, read: slugOf(entry.u.split('#')[0]), url: SITE + entry.u, excerpt: entry.x }));
    return text(hits.length ? hits : `No docs match "${query}". Try list_docs.`);
  }
  if (name === 'read_doc') {
    const page = String(args.page || '').trim().replace(/^https?:\/\/[^/]+/, '').replace(/^\/?docs\/?/, '').replace(/\.md$/, '').replace(/[#?].*$/, '').replace(/\/$/, '') || 'index';
    if (!/^[a-z0-9-]+$/.test(page)) return text(`Unknown page "${args.page}". Use list_docs.`, true);
    const markdown = await asset(env, request, `/docs/${page}.md`);
    return markdown ? text(markdown) : text(`Unknown page "${args.page}". Use list_docs.`, true);
  }
  return null;
}

async function handle(message, env, request) {
  if (!message || typeof message !== 'object' || message.jsonrpc !== '2.0' || typeof message.method !== 'string') return fail(message?.id ?? null, -32600, 'Invalid Request');
  const { id, method, params = {} } = message;
  const notification = id === undefined;
  switch (method) {
    case 'initialize': {
      const version = VERSIONS.includes(params.protocolVersion) ? params.protocolVersion : VERSIONS[0];
      return ok(id, {
        protocolVersion: version,
        capabilities: { tools: {} },
        serverInfo: { name: 'diri-docs', version: '1.0.0' },
        instructions: 'Read-only access to the diri documentation at https://diri.sh/docs/. Use search_docs to find a topic, then read_doc for the full page. This server cannot control diri; the dirijor MCP server inside the app does that.',
      });
    }
    case 'ping': return notification ? null : ok(id, {});
    case 'tools/list': return ok(id, { tools: TOOLS });
    case 'tools/call': {
      const result = await call(params.name, params.arguments || {}, env, request);
      return result ? ok(id, result) : fail(id, -32602, `Unknown tool: ${params.name}`);
    }
    default: return notification ? null : fail(id, -32601, `Method not found: ${method}`);
  }
}

export async function onRequest({ request, env }) {
  if (request.method === 'OPTIONS') return new Response(null, { status: 204, headers: CORS });
  if (request.method === 'GET') {
    // No server-initiated stream. Browsers get a pointer instead of a bare 405.
    if ((request.headers.get('Accept') || '').includes('text/event-stream')) return new Response(null, { status: 405, headers: { Allow: 'POST', ...CORS } });
    return json({ name: 'diri-docs', transport: 'streamable-http', docs: `${SITE}/docs/mcp/#docs-mcp`, tools: TOOLS.map(tool => tool.name) });
  }
  if (request.method !== 'POST') return new Response(null, { status: 405, headers: { Allow: 'POST, GET, OPTIONS', ...CORS } });
  let body;
  try { body = await request.json(); } catch { return json(fail(null, -32700, 'Parse error'), 400); }
  if (Array.isArray(body)) {
    const replies = (await Promise.all(body.map(message => handle(message, env, request)))).filter(Boolean);
    return replies.length ? json(replies) : json(null, 202);
  }
  const reply = await handle(body, env, request);
  return reply ? json(reply) : json(null, 202);
}

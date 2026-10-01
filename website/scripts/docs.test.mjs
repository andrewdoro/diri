// The docs MCP server (functions/mcp.js) against the generated docs, with no network.
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { onRequest } from '../functions/mcp.js';
import { rank } from '../docs-search.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const env = { ASSETS: { fetch: async url => {
  try { return new Response(await readFile(resolve(root, '.' + new URL(url).pathname))); }
  catch { return new Response('Not found', { status: 404 }); }
} } };
const rpc = async (body, init = {}) => onRequest({ env, request: new Request('https://diri.sh/mcp', { method: 'POST', body: JSON.stringify(body), headers: { 'Content-Type': 'application/json' }, ...init }) });
const call = async (name, args) => (await (await rpc({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } })).json()).result;

test('initialize negotiates a supported protocol version', async () => {
  const reply = await (await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26' } })).json();
  assert.equal(reply.result.protocolVersion, '2025-03-26');
  assert.equal(reply.result.serverInfo.name, 'diri-docs');
  const unknown = await (await rpc({ jsonrpc: '2.0', id: 2, method: 'initialize', params: { protocolVersion: '1999-01-01' } })).json();
  assert.equal(unknown.result.protocolVersion, '2025-06-18');
});

test('notifications get 202 and no body', async () => {
  const response = await rpc({ jsonrpc: '2.0', method: 'notifications/initialized' });
  assert.equal(response.status, 202);
});

test('tools/list exposes the three read-only tools', async () => {
  const reply = await (await rpc({ jsonrpc: '2.0', id: 1, method: 'tools/list' })).json();
  assert.deepEqual(reply.result.tools.map(tool => tool.name), ['search_docs', 'read_doc', 'list_docs']);
});

test('search finds the MCP tool reference and read_doc returns Markdown', async () => {
  const hits = JSON.parse((await call('search_docs', { query: 'spawn_agents' })).content[0].text);
  assert.ok(hits.some(hit => hit.read === 'mcp-tools'), 'spawn_agents is documented');
  const page = await call('read_doc', { page: 'https://diri.sh/docs/mcp/#docs-mcp' });
  assert.equal(page.isError, false);
  assert.match(page.content[0].text, /^# MCP server/);
});

test('read_doc rejects paths outside the docs', async () => {
  for (const page of ['../functions/mcp', '..%2Fsecret', 'nope']) assert.equal((await call('read_doc', { page })).isError, true);
});

test('list_docs lists every page once', async () => {
  const pages = JSON.parse((await call('list_docs', {})).content[0].text);
  assert.ok(pages.length >= 10);
  assert.equal(new Set(pages.map(page => page.page)).size, pages.length);
});

test('unknown methods and malformed bodies are JSON-RPC errors', async () => {
  assert.equal((await (await rpc({ jsonrpc: '2.0', id: 1, method: 'resources/list' })).json()).error.code, -32601);
  const bad = await onRequest({ env, request: new Request('https://diri.sh/mcp', { method: 'POST', body: '{' }) });
  assert.equal((await bad.json()).error.code, -32700);
});

test('ranking prefers title matches and requires every word', () => {
  const index = [{ t: 'Worktrees and review', u: '/a/', x: 'branches' }, { t: 'Sessions', u: '/b/', x: 'worktrees mentioned here' }];
  assert.deepEqual(rank(index, 'worktrees').map(e => e.u), ['/a/', '/b/']);
  assert.deepEqual(rank(index, 'worktrees branches').map(e => e.u), ['/a/']);
});

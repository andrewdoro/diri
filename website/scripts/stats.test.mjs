import { test } from 'node:test';
import assert from 'node:assert/strict';

globalThis.sessionStorage = { getItem: () => null, setItem: () => {} };
const { commitsSince, compact } = await import('../stats.js');

test('compacts star counts', () => {
  assert.equal(compact(338), '338');
  assert.equal(compact(1000), '1k');
  assert.equal(compact(1234), '1.2k');
  assert.equal(compact(25400), '25k');
});

test('counts commits from the last page link', async () => {
  globalThis.fetch = async url => {
    assert.match(String(url), /per_page=1&since=2026-09-01T00%3A00%3A00\.000Z/);
    return { ok: true, headers: new Map([['Link', '<https://api.github.com/x?per_page=1&page=2>; rel="next", <https://api.github.com/x?per_page=1&page=357>; rel="last"']]), json: async () => [{}] };
  };
  assert.equal(await commitsSince(30, Date.parse('2026-10-01T00:00:00Z')), 357);
});

test('falls back to the page length without a Link header', async () => {
  globalThis.fetch = async () => ({ ok: true, headers: new Map(), json: async () => [{}] });
  assert.equal(await commitsSince(30), 1);
});

test('rejects on API errors so the page shows nothing', async () => {
  globalThis.fetch = async () => ({ ok: false, status: 403, headers: new Map() });
  await assert.rejects(commitsSince(30));
});

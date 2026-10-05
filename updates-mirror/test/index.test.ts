// Run with `node --test` (Node 24 strips the types; no dependencies).

import assert from "node:assert/strict";
import { test } from "node:test";

import { type Deps, handle } from "../src/index.ts";

const ORIGIN = "https://updates.diri.sh";
const ZIP = "/releases/download/v0.9.2/diri-0.9.2-universal.zip";

function fakeDeps(upstream: (url: string) => Response | Promise<Response>) {
  const fetched: string[] = [];
  const store = new Map<string, Response>();
  const pending: Promise<unknown>[] = [];
  const deps: Deps = {
    fetch: async (url) => {
      fetched.push(url);
      return upstream(url);
    },
    cache: {
      match: async (key: RequestInfo | URL) => {
        const hit = store.get(new Request(key).url);
        return hit?.clone();
      },
      put: async (key: RequestInfo | URL, response: Response) => {
        // Buffer like the real cache does, so the clone is fully consumed.
        const body = await response.arrayBuffer();
        store.set(new Request(key).url, new Response(body, response));
      },
    },
    waitUntil: (promise) => {
      pending.push(promise);
    },
  };
  return { deps, fetched, store, settle: () => Promise.all(pending) };
}

const get = (path: string, method = "GET") => new Request(`${ORIGIN}${path}`, { method });

test("the feed is the latest GitHub release's appcast, unmodified", async () => {
  const feed = '{"feed_version":1,"releases":[]}';
  const { deps, fetched } = fakeDeps(() => new Response(feed, { status: 200 }));
  const response = await handle(get("/appcast.json"), deps);
  assert.equal(response.status, 200);
  assert.equal(await response.text(), feed);
  assert.deepEqual(fetched, [
    "https://github.com/cristicretu/diri/releases/latest/download/appcast.json",
  ]);
  assert.match(response.headers.get("cache-control") ?? "", /max-age=120\b/);
});

test("an archive is the same tag and file on GitHub, byte for byte", async () => {
  const bytes = new Uint8Array([0x50, 0x4b, 3, 4, 0xff, 0x00, 0x7f]);
  const { deps, fetched } = fakeDeps(
    () => new Response(bytes, { status: 200, headers: { "content-length": "7" } }),
  );
  const response = await handle(get(ZIP), deps);
  assert.equal(response.status, 200);
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), bytes);
  assert.equal(response.headers.get("content-length"), "7");
  assert.deepEqual(fetched, [
    "https://github.com/cristicretu/diri/releases/download/v0.9.2/diri-0.9.2-universal.zip",
  ]);
});

test("nothing outside the feed and diri archives is proxied", async () => {
  const { deps, fetched } = fakeDeps(() => new Response("x"));
  for (const path of [
    "/",
    "/releases/download/v0.9.2/diri-0.9.2.dmg",
    "/releases/download/v0.9.2/SHA256SUMS",
    "/releases/download/v0.9.2/../../other/repo.zip",
    "/releases/download/v0.9.2/%2e%2e/diri-x.zip",
    "/releases/download/latest/diri-0.9.2-universal.zip",
    "/releases/download/v0.9.2/sub/diri-0.9.2.zip",
    "/cristicretu/diri/releases/download/v0.9.2/diri-0.9.2-universal.zip",
    "/appcast.json/",
  ]) {
    const response = await handle(get(path), deps);
    assert.equal(response.status, 404, path);
  }
  assert.deepEqual(fetched, [], "no upstream request for a refused path");
});

test("only GET and HEAD", async () => {
  const { deps, fetched } = fakeDeps(() => new Response("x"));
  const response = await handle(get("/appcast.json", "POST"), deps);
  assert.equal(response.status, 405);
  assert.deepEqual(fetched, []);
});

test("upstream failures are reported, never cached", async () => {
  for (const [upstream, status] of [
    [() => new Response("gone", { status: 404 }), 404],
    [() => new Response("oops", { status: 503 }), 502],
    [
      () => {
        throw new TypeError("network");
      },
      502,
    ],
  ] as const) {
    const { deps, store, settle } = fakeDeps(upstream);
    const response = await handle(get(ZIP), deps);
    await settle();
    assert.equal(response.status, status);
    assert.equal(store.size, 0);
  }
});

test("a cached copy is served without asking GitHub, whatever the query string", async () => {
  let calls = 0;
  const { deps, fetched, settle } = fakeDeps(() => new Response(`feed ${++calls}`));
  assert.equal(await (await handle(get("/appcast.json"), deps)).text(), "feed 1");
  await settle();
  assert.equal(await (await handle(get("/appcast.json?bust=1"), deps)).text(), "feed 1");
  assert.equal(fetched.length, 1);
});

test("HEAD answers headers only", async () => {
  const { deps } = fakeDeps(() => new Response("abc", { headers: { "content-length": "3" } }));
  const response = await handle(get(ZIP, "HEAD"), deps);
  assert.equal(response.status, 200);
  assert.equal(response.body, null);
});

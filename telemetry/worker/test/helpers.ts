import { SELF } from "cloudflare:test";

export const TOKEN = "test-admin-token-0123456789";
export const BASE = "https://telemetry.test";

/** A fresh install per test keeps tests independent of shared D1 state. */
export function newInstall(): string {
  const hex = crypto.randomUUID().replace(/-/g, "");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20, 32)}`;
}

/** The Rust recorder's support id, reimplemented for fixtures. */
export function supportId(install: string): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  const bits = BigInt(`0x${install.replace(/-/g, "").slice(0, 10)}`);
  let code = "";
  for (let i = 7; i >= 0; i--) code += alphabet[Number((bits >> BigInt(i * 5)) & 31n)];
  return `D-${code}`;
}

export function header(install: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    v: 1,
    type: "batch",
    install,
    support_id: supportId(install),
    name: "julia",
    app_version: "0.9.0",
    build: "abc123",
    channel: "stable",
    os: "macos",
    os_version: "27.0",
    arch: "aarch64",
    sent_at: 1790581979447,
    lines: 0,
    ...extra,
  };
}

/** A record in the recorder's exact key order. */
export function rec(
  t: number,
  k: string,
  s: string,
  f: Record<string, unknown> = {},
  p = "engine",
  seq = t % 1000,
): string {
  return JSON.stringify({ t, seq, p, pid: 812, k, s, f });
}

export async function gzip(text: string): Promise<Uint8Array> {
  const stream = new Blob([text]).stream().pipeThrough(new CompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

export async function ingest(
  install: string,
  lines: string[],
  opts: { header?: Record<string, unknown> | string; body?: Uint8Array; installHeader?: string } = {},
): Promise<Response> {
  const head = typeof opts.header === "string" ? opts.header : JSON.stringify(opts.header ?? header(install, { lines: lines.length }));
  const body = opts.body ?? (await gzip(`${[head, ...lines].join("\n")}\n`));
  return SELF.fetch(`${BASE}/v1/ingest`, {
    method: "POST",
    headers: {
      "content-type": "application/x-ndjson",
      "content-encoding": "gzip",
      "x-diri-install": opts.installHeader ?? install,
    },
    body,
  });
}

export async function admin(path: string, token = TOKEN): Promise<Response> {
  return SELF.fetch(`${BASE}${path}`, { headers: { authorization: `Bearer ${token}` } });
}

export async function adminJson<T = any>(path: string): Promise<T> {
  const res = await admin(path);
  if (res.status !== 200) throw new Error(`${path} -> ${res.status} ${await res.text()}`);
  return (await res.json()) as T;
}

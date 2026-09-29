// POST /v1/ingest: one gzip NDJSON batch from an Engine's uploader.
//
// The contract (diri/TELEMETRY.md, "Upload") is that 400/413/422 are
// permanent (the client drops the batch) and 429/5xx are retried. So a
// batch is rejected whole only when it can never succeed: an unreadable
// body, a bad header line, or a size cap. One malformed record is counted
// and skipped, never a reason to lose the other 49,999.
//
// Cost shape: one R2 PUT (the original gzip bytes, stored untouched) and one
// D1 read + one D1 batch of at most four statements per request. Records are
// scanned with an anchored regex over the recorder's fixed key order; only
// error/incident lines are JSON.parse'd, which keeps a 4 MiB batch inside a
// Worker's CPU budget.

import { recordWrite, refuseWrite, usage } from "./budget";
import { type Env, error, json, rateLimitPerHour } from "./env";

export const MAX_COMPRESSED_BYTES = 5 * 1024 * 1024;
export const MAX_RAW_BYTES = 64 * 1024 * 1024;
export const MAX_LINES = 50_000;
/** Incident rows indexed per batch; the rest stay in R2 and are counted. */
export const MAX_INCIDENTS_PER_BATCH = 200;
/** Distinct (session, conv) pairs upserted per batch. */
export const MAX_SESSIONS_PER_BATCH = 200;
export const MAX_INCIDENT_FIELDS_BYTES = 2048;
const MAX_SIGNATURE_CHARS = 200;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const SUPPORT_ID = /^D-[0-9A-HJKMNP-TV-Z]{8}$/;
const ID = /^[A-Za-z0-9_.:-]{1,96}$/;
// The recorder always writes t, seq, p, pid, k, s, f in this order.
const RECORD_PREFIX =
  /^\{"t":(\d{1,16}),"seq":(\d{1,16}),"p":"([a-z]{1,16})","pid":(\d{1,10}),"k":"([^"\\]{1,128})","s":"([a-z]{1,16})"/;
const SESSION_FIELD = /"session":"([A-Za-z0-9_.:-]{1,96})"/;
const CONV_FIELD = /"conv":"([A-Za-z0-9_.:-]{1,96})"/;
const AGENT_FIELD = /"agent":"([A-Za-z0-9_.:-]{1,96})"/;

/** The validated first line of a batch. */
export interface BatchHeader {
  install: string;
  support_id: string;
  name: string | null;
  app_version: string | null;
  build: string | null;
  channel: string | null;
  os: string | null;
  os_version: string | null;
  arch: string | null;
  sent_at: number;
  lines: number | null;
}

class Reject extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    detail: string,
  ) {
    super(detail);
  }
}

function boundedString(value: unknown, field: string, max: number, required = false): string | null {
  if (value === undefined || value === null) {
    if (required) throw new Reject(422, "bad_header", `${field} is required`);
    return null;
  }
  if (typeof value !== "string" || value.length > max || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Reject(422, "bad_header", `${field} must be a string of at most ${max} characters`);
  }
  return value;
}

export function parseHeader(line: string): BatchHeader {
  let raw: unknown;
  try {
    raw = JSON.parse(line);
  } catch {
    throw new Reject(400, "bad_header", "first line is not JSON");
  }
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Reject(400, "bad_header", "first line is not an object");
  }
  const h = raw as Record<string, unknown>;
  if (h.v !== 1) throw new Reject(422, "unsupported_version", "v must be 1");
  if (h.type !== "batch") throw new Reject(422, "bad_header", "type must be batch");
  const install = typeof h.install === "string" ? h.install.toLowerCase() : "";
  if (!UUID.test(install)) throw new Reject(422, "bad_header", "install must be a UUID");
  const supportId = boundedString(h.support_id, "support_id", 10, true)!;
  if (!SUPPORT_ID.test(supportId)) throw new Reject(422, "bad_header", "support_id is malformed");
  const sentAt = h.sent_at;
  if (typeof sentAt !== "number" || !Number.isSafeInteger(sentAt) || sentAt <= 0) {
    throw new Reject(422, "bad_header", "sent_at must be epoch milliseconds");
  }
  const lines = h.lines;
  if (lines !== undefined && (typeof lines !== "number" || !Number.isSafeInteger(lines) || lines < 0)) {
    throw new Reject(422, "bad_header", "lines must be a non-negative integer");
  }
  return {
    install,
    support_id: supportId,
    name: boundedString(h.name, "name", 64),
    app_version: boundedString(h.app_version, "app_version", 64),
    build: boundedString(h.build, "build", 64),
    channel: boundedString(h.channel, "channel", 32),
    os: boundedString(h.os, "os", 32),
    os_version: boundedString(h.os_version, "os_version", 64),
    arch: boundedString(h.arch, "arch", 32),
    sent_at: sentAt,
    lines: (lines as number | undefined) ?? null,
  };
}

export interface IncidentRow {
  t: number;
  seq: number | null;
  proc: string | null;
  pid: number | null;
  kind: string;
  sev: string;
  session: string | null;
  conv: string | null;
  agent: string | null;
  code: string | null;
  signature: string;
  fields: string;
}

interface SessionSpan {
  session: string;
  conv: string;
  agent: string | null;
  first_t: number;
  last_t: number;
}

/** Everything the index needs from one batch's records. */
export class BatchScan {
  lines = 0;
  badLines = 0;
  rawBytes = 0;
  tMin: number | null = null;
  tMax: number | null = null;
  procs = new Set<string>();
  incidents: IncidentRow[] = [];
  incidentsDropped = 0;
  sessions = new Map<string, SessionSpan>();
  sessionsDropped = 0;

  record(line: string): void {
    if (line.length === 0) return;
    this.lines += 1;
    if (this.lines > MAX_LINES) {
      throw new Reject(413, "too_many_lines", `more than ${MAX_LINES} records`);
    }
    if (line.charCodeAt(0) !== 0x7b || line.charCodeAt(line.length - 1) !== 0x7d) {
      this.badLines += 1;
      return;
    }
    let t: number;
    let seq: number | null;
    let proc: string | null;
    let pid: number | null;
    let kind: string;
    let sev: string;
    let parsed: Record<string, unknown> | null = null;
    const prefix = RECORD_PREFIX.exec(line);
    if (prefix) {
      t = Number(prefix[1]);
      seq = Number(prefix[2]);
      proc = prefix[3];
      pid = Number(prefix[4]);
      kind = prefix[5];
      sev = prefix[6];
    } else {
      // Not the recorder's canonical order; take the slow path once.
      try {
        const value: unknown = JSON.parse(line);
        if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error();
        parsed = value as Record<string, unknown>;
      } catch {
        this.badLines += 1;
        return;
      }
      if (typeof parsed.t !== "number" || typeof parsed.k !== "string" || typeof parsed.s !== "string") {
        this.badLines += 1;
        return;
      }
      t = parsed.t;
      seq = typeof parsed.seq === "number" ? parsed.seq : null;
      proc = typeof parsed.p === "string" ? parsed.p.slice(0, 16) : null;
      pid = typeof parsed.pid === "number" ? parsed.pid : null;
      kind = parsed.k.slice(0, 128);
      sev = parsed.s.slice(0, 16);
    }

    if (this.tMin === null || t < this.tMin) this.tMin = t;
    if (this.tMax === null || t > this.tMax) this.tMax = t;
    if (proc && this.procs.size < 8) this.procs.add(proc);

    // JSON escapes every quote inside a string value, so `"session":"` can
    // only match a real key, never scrubbed text that mentions one.
    // indexOf first: most records (health, metrics, frames) name no session.
    const session = line.includes('"session":"') ? (SESSION_FIELD.exec(line)?.[1] ?? null) : null;
    const conv = session && line.includes('"conv":"') ? (CONV_FIELD.exec(line)?.[1] ?? null) : null;
    const agent = session && line.includes('"agent":"') ? (AGENT_FIELD.exec(line)?.[1] ?? null) : null;
    if (session) this.session(session, conv ?? "", agent, t);

    if (sev === "error" || sev === "incident") {
      if (this.incidents.length >= MAX_INCIDENTS_PER_BATCH) {
        this.incidentsDropped += 1;
        return;
      }
      if (!parsed) {
        try {
          parsed = JSON.parse(line) as Record<string, unknown>;
        } catch {
          this.badLines += 1;
          return;
        }
      }
      const f = isObject(parsed.f) ? parsed.f : {};
      const code = scalarString(f.code);
      this.incidents.push({
        t,
        seq,
        proc,
        pid,
        kind,
        sev,
        session,
        conv,
        agent,
        code,
        signature: signatureOf(kind, f, code),
        fields: capFields(f),
      });
    }
  }

  private session(session: string, conv: string, agent: string | null, t: number): void {
    const key = `${session}\u0000${conv}`;
    const span = this.sessions.get(key);
    if (span) {
      if (t < span.first_t) span.first_t = t;
      if (t > span.last_t) span.last_t = t;
      if (agent && !span.agent) span.agent = agent;
      return;
    }
    if (this.sessions.size >= MAX_SESSIONS_PER_BATCH) {
      this.sessionsDropped += 1;
      return;
    }
    this.sessions.set(key, { session, conv, agent, first_t: t, last_t: t });
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function scalarString(value: unknown): string | null {
  if (typeof value === "string") return value.slice(0, 96);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return null;
}

/**
 * The grouping key for `top`: a panic groups by its first non-runtime frame,
 * a crash report by the crashed frame, everything else by kind and code.
 */
export function signatureOf(kind: string, f: Record<string, unknown>, code: string | null): string {
  let signature: string | null = null;
  if (kind === "panic") {
    signature = typeof f.signature === "string" && f.signature ? f.signature : null;
  } else if (kind === "crash.report") {
    for (const key of ["signature", "crashed_frame", "frame"]) {
      if (typeof f[key] === "string" && f[key]) {
        signature = f[key] as string;
        break;
      }
    }
    if (!signature && Array.isArray(f.frames) && typeof f.frames[0] === "string") {
      signature = f.frames[0];
    }
  }
  if (signature) return `${kind}:${signature}`.slice(0, MAX_SIGNATURE_CHARS);
  return (code ? `${kind}:${code}` : kind).slice(0, MAX_SIGNATURE_CHARS);
}

/** The record's fields as JSON, at most 2 KiB: long values are trimmed first, then keys dropped. */
export function capFields(f: Record<string, unknown>): string {
  const whole = JSON.stringify(f);
  if (whole.length <= MAX_INCIDENT_FIELDS_BYTES) return whole;
  const out: Record<string, unknown> = {};
  let size = 2;
  for (const [key, value] of Object.entries(f)) {
    let v = value;
    if (typeof v === "string" && v.length > 256) v = `${v.slice(0, 256)}…`;
    else if (Array.isArray(v) && v.length > 8) v = [...v.slice(0, 8), `…${v.length - 8} more`];
    else if (isObject(v) && JSON.stringify(v).length > 512) v = "…object trimmed";
    const piece = JSON.stringify(key).length + JSON.stringify(v).length + 2;
    if (size + piece > MAX_INCIDENT_FIELDS_BYTES - 24) {
      out["…"] = "trimmed";
      break;
    }
    out[key] = v;
    size += piece;
  }
  return JSON.stringify(out);
}

async function readCapped(body: ReadableStream<Uint8Array>, cap: number): Promise<Uint8Array> {
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > cap) {
      await reader.cancel();
      throw new Reject(413, "body_too_large", `compressed body exceeds ${cap} bytes`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

/** Streams the gzip body through the scanner without holding the raw text. */
export async function scanGzip(gz: Uint8Array, maxRaw = MAX_RAW_BYTES): Promise<{ header: BatchHeader; scan: BatchScan }> {
  const scan = new BatchScan();
  let header: BatchHeader | null = null;
  const stream = new Blob([gz]).stream().pipeThrough(new DecompressionStream("gzip"));
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  const take = (line: string) => {
    if (header === null) {
      header = parseHeader(line);
    } else {
      scan.record(line);
    }
  };
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      scan.rawBytes += value.byteLength;
      if (scan.rawBytes > maxRaw) {
        throw new Reject(413, "raw_too_large", `decompressed body exceeds ${maxRaw} bytes`);
      }
      pending += decoder.decode(value, { stream: true });
      let start = 0;
      for (let nl = pending.indexOf("\n", start); nl !== -1; nl = pending.indexOf("\n", start)) {
        take(pending.slice(start, nl));
        start = nl + 1;
      }
      pending = pending.slice(start);
    }
  } catch (err) {
    await reader.cancel().catch(() => {});
    if (err instanceof Reject) throw err;
    throw new Reject(400, "bad_gzip", "body is not valid gzip");
  }
  pending += decoder.decode();
  if (pending.length > 0) take(pending);
  if (header === null) throw new Reject(400, "empty_batch", "no header line");
  return { header, scan };
}

async function gzip(raw: Uint8Array): Promise<Uint8Array> {
  const stream = new Blob([raw]).stream().pipeThrough(new CompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

function randomSuffix(): string {
  const bytes = new Uint8Array(4);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function objectKey(install: string, sentAt: number, suffix = randomSuffix()): string {
  const day = new Date(sentAt).toISOString().slice(0, 10);
  return `v1/${install}/${day}/${sentAt}-${suffix}.ndjson.gz`;
}

export async function handleIngest(request: Request, env: Env, now = Date.now()): Promise<Response> {
  if (request.method !== "POST") return error(405, "method_not_allowed");
  const encoding = request.headers.get("content-encoding")?.toLowerCase();
  if (encoding && encoding !== "gzip") return error(400, "bad_encoding", "Content-Encoding must be gzip");
  const declared = Number(request.headers.get("content-length") ?? 0);
  if (declared > MAX_COMPRESSED_BYTES) return error(413, "body_too_large");
  const claimed = request.headers.get("x-diri-install")?.toLowerCase() ?? "";
  if (!UUID.test(claimed)) return error(400, "bad_install", "X-Diri-Install must be the install UUID");
  if (!request.body) return error(400, "empty_batch");

  // Before any decompression, R2 write or D1 write: the cheapest check that
  // turns a looping client away. One indexed COUNT over at most an hour of
  // that install's batches (<= the limit, so a bounded number of rows read).
  const limit = rateLimitPerHour(env);
  const recent = await env.DB.prepare(
    "SELECT COUNT(*) AS n FROM (SELECT 1 FROM batches WHERE install = ?1 AND received_at > ?2 LIMIT ?3)",
  )
    .bind(claimed, now - 60 * 60 * 1000, limit)
    .first<{ n: number }>();
  if ((recent?.n ?? 0) >= limit) {
    return error(429, "rate_limited", `at most ${limit} batches per hour`);
  }

  let gz: Uint8Array;
  let header: BatchHeader;
  let scan: BatchScan;
  try {
    gz = await readCapped(request.body, MAX_COMPRESSED_BYTES);
    if (gz[0] === 0x7b) {
      // A proxy in front of us already undid Content-Encoding. Store gzip
      // anyway so every R2 object has the same shape.
      gz = await gzip(gz);
    }
    ({ header, scan } = await scanGzip(gz));
  } catch (err) {
    if (err instanceof Reject) return error(err.status, err.code, err.message);
    throw err;
  }
  if (header.install !== claimed) {
    return error(422, "install_mismatch", "header install differs from X-Diri-Install");
  }

  // Spend guard, after validation so only real batches count against it.
  const refused = refuseWrite(await usage(env, now), gz.byteLength);
  if (refused) return error(429, "budget_exhausted", refused);

  const key = objectKey(header.install, header.sent_at);
  await env.BATCHES.put(key, gz, {
    httpMetadata: { contentType: "application/x-ndjson", contentEncoding: "gzip" },
    customMetadata: {
      install: header.install,
      support_id: header.support_id,
      sent_at: String(header.sent_at),
      lines: String(scan.lines),
      app_version: header.app_version ?? "",
    },
  });

  const procs = [...scan.procs].sort().join(",");
  const incidents = scan.incidents.map((i) => ({ ...i, install: header.install, app_version: header.app_version }));
  const sessions = [...scan.sessions.values()];
  const statements: D1PreparedStatement[] = [
    ...recordWrite(env, now, gz.byteLength),
    env.DB.prepare(
      `INSERT INTO installs (install, support_id, name, app_version, build, channel, os, os_version, arch, first_seen, last_seen)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
       ON CONFLICT (install) DO UPDATE SET
         support_id = excluded.support_id, name = excluded.name, app_version = excluded.app_version,
         build = excluded.build, channel = excluded.channel, os = excluded.os,
         os_version = excluded.os_version, arch = excluded.arch,
         last_seen = max(installs.last_seen, excluded.last_seen)`,
    ).bind(
      header.install,
      header.support_id,
      header.name,
      header.app_version,
      header.build,
      header.channel,
      header.os,
      header.os_version,
      header.arch,
      now,
    ),
    env.DB.prepare(
      `INSERT INTO batches (install, r2_key, received_at, sent_at, t_min, t_max, procs, lines, bad_lines, bytes, raw_bytes, app_version)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)`,
    ).bind(
      header.install,
      key,
      now,
      header.sent_at,
      scan.tMin,
      scan.tMax,
      procs,
      scan.lines,
      scan.badLines,
      gz.byteLength,
      scan.rawBytes,
      header.app_version,
    ),
  ];
  // One statement per table however many rows: json_each expands a single
  // bound JSON array, which keeps us far below D1's per-invocation query cap.
  if (incidents.length > 0) {
    statements.push(
      env.DB.prepare(
        `INSERT INTO incidents (install, t, seq, proc, pid, kind, sev, session, conv, agent, code, signature, app_version, fields)
         SELECT j.value ->> '$.install', j.value ->> '$.t', j.value ->> '$.seq', j.value ->> '$.proc',
                j.value ->> '$.pid', j.value ->> '$.kind', j.value ->> '$.sev', j.value ->> '$.session',
                j.value ->> '$.conv', j.value ->> '$.agent', j.value ->> '$.code', j.value ->> '$.signature',
                j.value ->> '$.app_version', j.value ->> '$.fields'
         FROM json_each(?1) AS j`,
      ).bind(JSON.stringify(incidents)),
    );
  }
  if (sessions.length > 0) {
    statements.push(
      env.DB.prepare(
        `INSERT INTO sessions (install, session, conv, agent, first_t, last_t)
         SELECT ?1, j.value ->> '$.session', j.value ->> '$.conv', j.value ->> '$.agent',
                j.value ->> '$.first_t', j.value ->> '$.last_t'
         FROM json_each(?2) AS j WHERE true
         ON CONFLICT (install, session, conv) DO UPDATE SET
           agent = coalesce(sessions.agent, excluded.agent),
           first_t = min(sessions.first_t, excluded.first_t),
           last_t = max(sessions.last_t, excluded.last_t)`,
      ).bind(header.install, JSON.stringify(sessions)),
    );
  }
  await env.DB.batch(statements);

  return json(
    {
      accepted: scan.lines,
      bad_lines: scan.badLines,
      incidents: incidents.length,
      incidents_unindexed: scan.incidentsDropped,
      sessions: sessions.length,
      key,
    },
    202,
  );
}


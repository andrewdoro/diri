# Diri telemetry Worker

Receives the batches Diri's Engine uploads (`diri/crates/diri-telemetry/src/upload.rs`), keeps them for 30 days, and answers the investigation queries `diri-debug` makes. The contract is `diri/TELEMETRY.md` (sections Record format, Upload, Worker).

It is a Cloudflare Worker with two storage bindings:

- **R2** (`BATCHES`) holds every batch exactly as uploaded (gzip NDJSON) at `v1/<install>/<yyyy-mm-dd>/<sent_at>-<rand>.ndjson.gz`. This is the only complete copy of the records.
- **D1** (`DB`) is the index: `installs`, `batches` (time range, processes, R2 key), `incidents` (every `error`/`incident` record with a grouping signature and up to 2 KiB of its fields) and `sessions` (session id, agent, conversation, first/last seen). Schema: `migrations/0001_init.sql`.

```
Engine uploader ──POST /v1/ingest (gzip NDJSON)──▶ Worker ──PUT original bytes──▶ R2
                                                     └──one D1 batch (≤4 statements)──▶ D1
diri-debug ──GET /v1/admin/* (Bearer ADMIN_TOKEN)──▶ Worker ──▶ D1 index, R2 bodies
cron 03:17 UTC ──▶ delete D1 rows + their R2 objects older than 30 days
```

## Ingest

`POST /v1/ingest`, headers `Content-Encoding: gzip`, `X-Diri-Install: <uuid>`.

| Check | Response |
|---|---|
| `X-Diri-Install` missing or not a UUID, body not gzip, header line not JSON | 400 |
| body over 5 MiB compressed, over 64 MiB decompressed (the stream stops at the cap), or over 50,000 records | 413 |
| header `v != 1`, `type != "batch"`, bad `install`/`support_id`/`sent_at`, a string field too long or with control characters, header install differs from `X-Diri-Install` | 422 |
| more than `RATE_LIMIT_PER_HOUR` (120) batches from this install in the last hour | 429 |
| accepted | 202 `{accepted, bad_lines, incidents, incidents_unindexed, sessions, key}` |

400/413/422 are permanent: the client drops the batch. 429 and 5xx are retried next cycle. A record line that is not a JSON object is counted in `bad_lines` and skipped; it never rejects the batch. Only the header line can do that.

Per request the Worker makes one D1 read (the rate limit), one R2 PUT, and one D1 `batch()` of at most four statements. Incidents and sessions go in as one `INSERT … SELECT FROM json_each(?)` each, however many rows, so a batch never approaches D1's per-invocation query limit. At most 200 incident rows and 200 session spans are indexed per batch; anything past that is still in R2 and is counted in the response.

Records are scanned with an anchored regex over the recorder's fixed key order (`t, seq, p, pid, k, s, f`). Only `error`/`incident` lines and lines in another key order are `JSON.parse`d.

**Rate limiting uses D1, not the Rate Limiting binding.** The check is `COUNT(*)` over this install's batch rows from the last hour, using the `(install, received_at)` index and capped by `LIMIT`, so it reads at most 120 rows and writes nothing (the batch row it counts is written anyway). The Workers Rate Limiting binding is per-colo, eventually consistent and only supports 10 s or 60 s periods, so it cannot express an hourly budget. The healthy uploader sends about 6 batches per hour, plus one per minute while incidents are happening.

## Admin API

Every route needs `Authorization: Bearer <ADMIN_TOKEN>`, compared in constant time. If the secret is unset or shorter than 16 characters, every request gets 401. All times are epoch milliseconds.

| Route | Returns |
|---|---|
| `GET /v1/admin/installs?q=` | installs matching a name substring, Support ID (`D-…`, prefix ok) or install UUID prefix; no `q` lists the most recent |
| `GET /v1/admin/installs/<uuid>` | install row, batch totals, incident counts by severity, session count, versions seen |
| `GET /v1/admin/incidents?install=&kind=&sev=&version=&session=&conv=&signature=&since=&until=&limit=` | incident rows with the install's `name` and `support_id`, newest first; `kind` accepts `*`/`?` globs |
| `GET /v1/admin/incidents/summary?…same filters` | groups by signature: `count`, `installs` affected, `first_t`, `last_t`, `versions` |
| `GET /v1/admin/batches?install=&since=&until=` | batches whose record time range overlaps the window, oldest first |
| `GET /v1/admin/batch?key=<r2 key>` | the stored gzip body, streamed as `application/gzip` (the caller gunzips) |
| `GET /v1/admin/sessions?install=` | `(session, conv)` spans with agent, newest first |
| `GET /v1/admin/find?id=<session id or conversation uuid>` | matching spans joined with the owning install's name and Support ID |

Incident signatures: a `panic` groups by `panic:<f.signature>` (first non-runtime frame), a `crash.report` by `crash.report:<f.signature | f.crashed_frame | f.frame | f.frames[0]>`, and everything else by `<kind>:<f.code>`, or by `<kind>` alone when there is no code.

## Retention

Nothing is kept longer than 30 days (`RETENTION_DAYS`).

1. **R2 lifecycle rule (primary).** Objects under `v1/` expire 30 days after upload. Lifecycle deletions are free and also catch objects whose D1 row was never written, for example when the Worker died between the PUT and the D1 batch.
2. **Daily cron (`17 3 * * *`).** Walks `batches` for rows older than the window, deletes their R2 objects in chunks of 1,000 (R2 deletes are free; listing the bucket is not), then deletes old `batches`, `incidents`, `sessions` and silent `installs` rows. Each run handles at most 50,000 batches; anything left is picked up the next day.

## Deploy

Run everything from this directory. The owner deploys; CI only runs the tests.

```sh
cd telemetry/worker
pnpm install
pnpm exec wrangler login

# 1. Storage.
pnpm exec wrangler d1 create diri-telemetry        # paste database_id into wrangler.jsonc
pnpm exec wrangler r2 bucket create diri-telemetry
pnpm exec wrangler r2 bucket lifecycle add diri-telemetry expire-30d v1/ --expire-days 30 --force

# 2. Schema.
pnpm exec wrangler d1 migrations apply diri-telemetry --remote

# 3. Admin secret. Keep a copy in your password manager; diri-debug needs it.
openssl rand -base64 32 | tr -d '\n' | pnpm exec wrangler secret put ADMIN_TOKEN

# 4. Check, then ship.
pnpm run check        # tsc, tests, wrangler deploy --dry-run
pnpm exec wrangler deploy
```

**Hostname.** `wrangler deploy` prints `https://diri-telemetry.<account>.workers.dev`, which works immediately. To use `https://telemetry.diri.sh` instead, the `diri.sh` zone must be on the same Cloudflare account. It already is if the website runs on Pages (see `website/CLOUDFLARE.md`). Uncomment the `routes` entry in `wrangler.jsonc`, deploy again, and optionally set `"workers_dev": false`.

**Smoke test.**

```sh
curl -s https://telemetry.diri.sh/healthz                     # {"ok":true}
curl -s -H "Authorization: Bearer $DIRI_TELEMETRY_ADMIN_TOKEN" \
  'https://telemetry.diri.sh/v1/admin/installs?limit=5'
```

**Schema changes** go in a new `migrations/000N_*.sql`. Apply it with `pnpm run migrate` before deploying code that uses it.

## Point the app at it

The uploader resolves its endpoint in `diri/crates/diri-telemetry/src/upload.rs::endpoint()`:

- **Release builds.** Either set `pub const DEFAULT_ENDPOINT: Option<&str> = Some("https://telemetry.diri.sh");` in `upload.rs`, or export `DIRI_TELEMETRY_ENDPOINT=https://telemetry.diri.sh` when building the release (`option_env!` reads it at compile time and it wins over the const).
- **Run-time override.** Any build honours `DIRI_TELEMETRY_ENDPOINT` in the Engine's environment. `off` disables uploading. Debug builds upload only when this is set.

The endpoint is the origin only; the client appends `/v1/ingest`.

## Cost on the free tiers

The free-plan limits used below (check the current pricing pages before relying on them):

- Workers: 100,000 requests per day, 10 ms CPU per request.
- R2: 10 GB-month of storage, 1M Class A operations (PUTs) and 10M Class B operations (GETs) per month, free egress.
- D1: 5M rows read and 100,000 rows written per day, 5 GB of storage.

Assumptions for one install with Diri open 8 hours a day:

- 6 uploads per hour gives about 48 batches per day.
- Health and metrics from 3 or 4 processes plus events come to roughly 5,000 records, about 1.5 MB raw or about 150 KB gzipped per day.

| Resource | Per install per day | Free limit | Installs it covers |
|---|---|---|---|
| Worker requests | 48 | 100,000/day | ~2,000 |
| R2 PUT (Class A) | 48 | ~33,000/day | ~690 |
| R2 storage, 30 days | ~4.5 MB | 10 GB | ~2,200 |
| D1 rows written (≈5/batch incl. indexes) | ~240 | 100,000/day | **~415** |
| D1 rows read (rate-limit count ≤ 30/batch at this rate) | ~1,500 | 5M/day | ~3,300 |

So the free tier carries about **400 daily-active installs**, and D1 row writes run out first. Admin queries and the daily sweep cost a few thousand reads and are negligible.

Past that, Workers Paid is $5/month. It includes 10M requests, 50M D1 rows written and 25B D1 rows read per month, and 30 s of CPU per request. R2 beyond its free tier costs $4.50 per million PUTs and $0.015 per GB-month, which is still cents at a few thousand installs.

**CPU.** A routine 10-minute batch (about 100 KB raw) takes well under 1 ms to scan. A full 4 MiB backlog batch (about 32k records) took about 15–25 ms on an M-series Mac in Node, about 5 ms of that in gzip. That is over the free plan's 10 ms, so the free plan may kill a large backlog upload (error 1102). A killed upload returns 5xx and the client retries it on every cycle, and a batch that can never succeed that way blocks the uploads queued behind it. Either use Workers Paid, or lower `BATCH_RAW_BYTES` in `upload.rs` to 1 MiB (about 5 ms) to stay safely on the free plan.

## Develop

```sh
pnpm install
pnpm test          # vitest inside workerd (@cloudflare/vitest-pool-workers), local D1 + R2
pnpm run typecheck
pnpm dev           # wrangler dev with local D1/R2 under .wrangler/
```

Local `wrangler dev` needs the migrations applied locally once: `pnpm exec wrangler d1 migrations apply diri-telemetry --local`. For the admin API it also needs a `.dev.vars` file (git-ignored) containing `ADMIN_TOKEN=…`.

All npm packages here are dev-only tooling (`devDependencies`). The deployed Worker bundles nothing from npm. `scripts/check-licenses.py` enforces that.

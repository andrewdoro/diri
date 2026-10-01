# Diri telemetry Worker

Receives the batches Diri's Engine uploads (`diri/crates/diri-telemetry/src/upload.rs`), keeps them for 30 days, and answers the investigation queries `diri-debug` makes. The contract is `diri/TELEMETRY.md` (sections Record format, Upload, Worker).

It is a Cloudflare Worker with two storage bindings:

- **R2** (`BATCHES`) holds every batch exactly as uploaded (gzip NDJSON) at `v1/<install>/<yyyy-mm-dd>/<sent_at>-<rand>.ndjson.gz`. This is the only complete copy of the records.
- **D1** (`DB`) is the index: `installs`, `batches` (time range, processes, R2 key), `incidents` (every `error`/`incident` record with a grouping signature and up to 2 KiB of its fields), `sessions` (session id, agent, conversation, first/last seen) and `milestones` (the activation funnel: one row per install and `activation.*` step). Schema: `migrations/`.

```
Engine uploader ──POST /v1/ingest (gzip NDJSON)──▶ Worker ──PUT original bytes──▶ R2
                                                     └──one D1 batch (≤5 statements)──▶ D1
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

Accepted requests reserve global admission and R2 budget in D1, check the per-install limit, then make one R2 PUT and one D1 index `batch()` of at most five statements. Incidents, sessions and milestones go in as one `INSERT … SELECT FROM json_each(?)` each, however many rows, so a batch never approaches D1's per-invocation query limit. At most 200 incident rows and 200 session spans are indexed per batch; anything past that is still in R2 and is counted in the response. Milestones (`activation.*` records, once per install and step on the client) add at most six rows per install over its lifetime, with `INSERT OR IGNORE` so a re-sent batch never moves a step's time.

Records are scanned with an anchored regex over the recorder's fixed key order (`t, seq, p, pid, k, s, f`). Only `error`/`incident` lines and lines in another key order are `JSON.parse`d.

**Admission has independent limits.** The required `INGEST_RATE_LIMITER`
binding keys on Cloudflare's `CF-Connecting-IP`, never a client-supplied
install UUID or `X-Forwarded-For`. It allows 30 attempts per minute per location
before any D1/body work; IPs are not persisted in D1 or R2. The binding is
[per-location and eventually consistent](https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/),
so it is backed by an atomic, global D1 reservation: at most 3,600 attempts per
UTC hour. `GLOBAL_RATE_LIMIT_PER_HOUR` may lower that cap, including zero to
pause admission. Failed/invalid bodies consume admission too. Missing source
identity returns 400; a missing limiter binding returns 503. The existing
per-install 120/hour limit remains an additional fairness check, not proof of
identity. Upload labels and events are still untrusted client claims.

R2 write/byte and read budgets are reserved atomically before the respective
operation. A failed R2 operation or later index write keeps the reservation,
so retries can exhaust the budget early but cannot leave unaccounted usage.
The global/source limits are independent of these monthly storage budgets.


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
| `GET /v1/admin/funnel?since=&until=&version=` | the activation funnel (`src/funnel.ts`): installs whose `first_launch` record time is in the window (default the last 7 days; `version` filters by the version that first launched) form the cohort, split into `new` and `preexisting` (used Diri before activation tracking). Each has `installs` and per step `installs, of_cohort, of_previous, median_s` (seconds from first launch) and, for `agent_ready`, `sources`. Reads at most 60,000 milestone rows (`truncated` says so) |

Incident signatures: a `panic` groups by `panic:<f.signature>` (first non-runtime frame), a `crash.report` by `crash.report:<f.signature | f.crashed_frame | f.frame | f.frames[0]>`, and everything else by `<kind>:<f.code>`, or by `<kind>` alone when there is no code.

## Retention

The retention window is 30 days (`RETENTION_DAYS`); the daily sweep uses server
receipt times, so deletion occurs at the next sweep after expiry. R2 lifecycle
expiration is an independent backstop.

1. **R2 lifecycle rule (primary).** Objects under `v1/` expire 30 days after upload. Lifecycle deletions are free and also catch objects whose D1 row was never written, for example when the Worker died between the PUT and the D1 batch.
2. **Daily cron (`17 3 * * *`).** Walks `batches` for rows older than the window, deletes their R2 objects in chunks of 1,000 (R2 deletes are free; listing the bucket is not), then deletes old `batches`, `incidents`, `sessions`, `milestones` and silent `installs` rows. Milestones expire like everything else, so a funnel can only look back 30 days: the cohort's `first_launch` rows are gone after that. Each run handles at most 50,000 batches; anything left is picked up the next day.

## Deploy

Apply migration `0004_activation.sql` (the `milestones` table) before deploying
the revision that indexes activation records: `pnpm run migrate`, then
`pnpm exec wrangler deploy`. Batches ingested before it carry no indexed
milestones (they stay in R2 only), so the funnel starts with the first upload
after the deploy.

Apply migration `0003_server_retention.sql` before deploying this revision.
It intentionally expires the existing incident/session index at the next sweep
because old rows have no reliable server receipt time. Existing R2 batches and
their index remain available until normal expiry. Old Worker writes during a
rolling deployment get receipt time zero and also expire conservatively.
Ensure `INGEST_RATE_LIMITER` is included when deploying; choose a namespace ID
unique to this limiter within the account. Verify the R2 lifecycle rule remains
active. CI only validates and bundles; it does not migrate or deploy production.

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

Routine uploads run every ten minutes; incidents can upload within a minute.
Admission and R2 reservations add D1 writes, including rejected attempts at the
global admission stage. Capacity depends on batch frequency, incident/session
index rows and their indexes, retention, and admin traffic. Measure D1 rows read
and written in the deployed workload before setting an install capacity target.
The global admission cap is an abuse bound, not a guarantee that the D1 free
quota can sustain that many accepted batches.

The client caps each batch at 1 MiB raw (`BATCH_RAW_BYTES`). Keep CPU and D1
quota failures visible when validating production traffic.

## Spend guard

- **Workers and D1:** stay on the Workers **Free** plan. Over its limits, requests fail instead of billing. Never upgrade to Workers Paid for this Worker; that is what turns D1 overage into money.
- **R2** is the only resource that bills past its free tier once enabled, and Cloudflare has no spending cap for it. So the Worker enforces one itself (`src/budget.ts`): it atomically reserves its R2 writes, reads and stored bytes in D1 (`budget` table) and refuses work at **90% of the free tier**: 900k writes/month, 9M reads/month, 9 GB stored. Over a cap, ingest answers `429 budget_exhausted` and the app keeps the data locally and retries later. `BUDGET_MONTHLY_PUTS`, `BUDGET_MONTHLY_GETS` and `BUDGET_STORED_BYTES` can lower the caps, never raise them.
- **Backstops:** the R2 lifecycle rule deletes objects after 30 days even if the daily sweep fails (the stored-bytes counter assumes it). Add a Cloudflare **billing notification** (Notifications → Add → Usage Based Billing) as a last alarm.
- `diri-debug budget` (or `GET /v1/admin/budget`) shows current usage against the caps.

## Develop

```sh
pnpm install
pnpm test          # vitest inside workerd (@cloudflare/vitest-pool-workers), local D1 + R2
pnpm run typecheck
pnpm dev           # wrangler dev with local D1/R2 under .wrangler/
```

Local `wrangler dev` needs the migrations applied locally once: `pnpm exec wrangler d1 migrations apply diri-telemetry --local`. For the admin API it also needs a `.dev.vars` file (git-ignored) containing `ADMIN_TOKEN=…`.

All npm packages here are dev-only tooling (`devDependencies`). The deployed Worker bundles nothing from npm. `scripts/check-licenses.py` enforces that.

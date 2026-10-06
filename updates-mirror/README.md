# diri update mirror

A Cloudflare Worker at `https://updates.diri.sh` that serves the macOS update
feed and release archives for people whose network cannot reach GitHub
reliably (DNS poisoning, blocked or crawling `github.com` /
`release-assets.githubusercontent.com`, TLS interception).

It proxies exactly two things from the public GitHub release, unmodified, and
caches them at the edge:

| Mirror path | GitHub source | Edge TTL |
| --- | --- | --- |
| `/appcast.json` | `releases/latest/download/appcast.json` | 2 min |
| `/releases/download/<tag>/diri-*.zip` | `releases/download/<tag>/<file>` | 30 days |

Every other path is a 404 without an upstream request, so it is not an open
proxy.

## Trust model

The app does not trust this Worker more than it trusts GitHub, and in one
respect less: release URLs in the feed must still name `github.com`, and the
app derives the mirror's archive URL from them itself. Whatever the mirror
serves is held to the feed's size and SHA-256 and then to the Developer ID +
notarization pin and the promised version, exactly like a GitHub download
(`diri/crates/diri-updater/src/mirror.rs`). A compromised mirror can withhold
updates, not ship one.

## The app side

`diri-updater` asks GitHub first. It asks the mirror too when GitHub fails on
its route (`dns`, `connect`, `timeout`, `tls`, a dropped connection, 403/429,
5xx), or when the feed has not arrived within 3 s. Once the feed has come from
the mirror, the archive is fetched there first. `update.check` and
`update.download` telemetry carry `source` (`github` | `mirror`),
`github_error` and `mirror_error`.

`DIRI_UPDATE_MIRROR=<host[:port]>` points an install at another mirror;
`DIRI_UPDATE_MIRROR=off` disables it (as does `DIRI_UPDATE_FEED`).

## Setup (once)

The `diri.sh` zone is already on Cloudflare (it serves `telemetry.diri.sh`).

```sh
cd updates-mirror
npx --yes wrangler@4.124.0 login     # if not already
npx --yes wrangler@4.124.0 deploy    # creates the Worker and the updates.diri.sh custom domain
curl -sSf https://updates.diri.sh/healthz
curl -sSf https://updates.diri.sh/appcast.json | head -c 300
```

The custom domain creates its own DNS record and certificate; nothing else
needs configuring. No secrets, bindings or buckets.

## Tests

```sh
node --test test/*.test.ts   # Node 24, no install
```

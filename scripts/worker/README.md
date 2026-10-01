# Distribution mirror

A Cloudflare Worker that serves oven's downloads from one CDN hostname instead
of `github.com` / `raw.githubusercontent.com` / `api.github.com`. GitHub stays
the only source of truth; nothing is uploaded or kept in sync here.

| Path | Upstream | Edge TTL |
| --- | --- | --- |
| `/dl/<tag>/<asset>` | `github.com/guuzaa/oven/releases/download/<tag>/<asset>` | 1 year (tagged paths are immutable) |
| `/tags/<tag>` | `api.github.com/repos/guuzaa/oven/releases/tags/<tag>` | 60 s |
| `/install.sh`, `/install.ps1` | `raw.githubusercontent.com/guuzaa/oven/<branch>/scripts/` | 60 s |
| `/latest` | `api.github.com/repos/guuzaa/oven/releases/latest` | 60 s |
| `/_health` | — | no-store |

GitHub answers an asset URL with a 302 to a signed `release-assets` URL that
expires in minutes. The worker follows that redirect and stores only the final
200 body, keyed by the public URL (`https://<hostname>/dl/...`). A zone purge of
that URL drops the copy. Redirects and non-200 responses are not stored.
Redirect targets are limited to GitHub hostnames.

Anything else is a 404. The upstream repository is baked into the source, so
the worker cannot be used as an open proxy. Republishing the same tag does not
refresh an asset already cached for a year; purge that exact `/dl/...` URL if
you replace one.

Both installers verify each download against the digest GitHub records for that
asset in the release JSON (`/latest` or `/tags/<tag>`), so there is nothing to
compute or publish here. Because that JSON is cached for 60 s while the binary is
cached for a year, a re-published tag stops being installable rather than
silently reinstalling the cached binary — purge the `/dl/<tag>/<asset>` URL to
recover.

## Setup

1. Put your hostname in `wrangler.toml`'s `routes`. The zone must already be
   hosted on Cloudflare for `custom_domain` to be created.
2. `npx wrangler@latest login`
3. `npx wrangler@latest secret put GITHUB_TOKEN` — optional but recommended: a
   fine-grained PAT with public-repository read access lifts `/latest` and
   `/tags/<tag>` from 60 to 5000 requests per hour. With the 60 s cache they are
   hit a few times an hour.
4. `npx wrangler@latest deploy`
5. `DEFAULT_MIRROR` in `scripts/install.sh` and `scripts/install.ps1` already
   points at the zone. Setting `OVEN_MIRROR` (or `$env:OVEN_MIRROR`) to an empty
   string makes an install skip the mirror and go straight to GitHub.
6. Optional: set the `CDN_URL` repository variable and the `CF_ZONE_ID` /
   `CF_PURGE_TOKEN` secrets so every release purges `/latest` and the installers
   instead of waiting out the 60 s TTL.

## Checking

```console
$ curl -fsSL -D - -o /dev/null https://<hostname>/latest | grep -i x-oven-cache
$ curl -fsSL -D - -o /dev/null https://<hostname>/dl/v0.0.9/oven-v0.0.9-x86_64-unknown-linux-musl.tar.gz | grep -iE 'x-oven-cache|content-length'
$ curl -fsSL https://<hostname>/tags/v0.0.9 | head -c 120
```

The first asset request is `x-oven-cache: MISS`, the second is `HIT`, and
`content-length` matches the GitHub release asset.

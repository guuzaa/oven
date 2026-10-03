# Distribution mirror

A Cloudflare Worker that serves oven's downloads and public site from one CDN
hostname instead of `github.com` / `raw.githubusercontent.com` /
`api.github.com`. GitHub stays the only source of truth; nothing is uploaded or
kept in sync here.

| Path | Upstream | Edge TTL |
| --- | --- | --- |
| `/dl/<tag>/<asset>` | `github.com/guuzaa/oven/releases/download/<tag>/<asset>` | 1 year (tagged paths are immutable) |
| `/tags/<tag>` | `api.github.com/repos/guuzaa/oven/releases/tags/<tag>` | 60 s |
| `/install`, `/install.sh`, `/install.ps1` | `raw.githubusercontent.com/guuzaa/oven/<branch>/scripts/` | 60 s |
| `/latest` | `api.github.com/repos/guuzaa/oven/releases/latest` | 60 s |
| `/changelog` | `raw.githubusercontent.com/guuzaa/oven/<branch>/CHANGELOG.md` | 60 s |
| `/_health` | — | no-store |
| anything else | `public/index.html` (see [Site](#site)) | static assets |

## Site

`[assets]` in `wrangler.toml` publishes `public/` as Workers static assets:
hand-written `index.html`, `app.css`, `app.js` and `icon.svg`, no build step and
no npm dependencies — the site is still deployed by `npx wrangler@latest deploy`.

A path that is neither an asset nor a route above reaches the worker, which
answers with `index.html` through the `ASSETS` binding (`binding = "ASSETS"`), so
a hard refresh of `/downloads` or `/releases` works. `app.js` then switches
between the `home`, `downloads` and `releases` views by `location.pathname` and
intercepts `[data-link]` clicks with `history.pushState`. The `/downloads` page
reads `/latest` from this worker and links each asset through
`/dl/<tag>/<asset>`, so the list is always the current release. The `/releases`
page parses `/changelog` in the browser — one card per `## [x.y.z] - date`
heading, grouped by its `###` sections — and falls back to
`raw.githubusercontent.com` (open CORS) when the route is missing, as in a plain
static preview.

Install snippets rewrite `https://oven.paulden.site` to `location.origin`, so a
copy-paste from `wrangler dev` or a renamed zone shows the host in use.

`/install` is the single entrypoint: it serves `install.ps1` to a `User-Agent`
carrying `PowerShell/` (which is what `irm` sends) and `install.sh` to anything
else. Both variants are stored under the canonical `/install.sh` and
`/install.ps1` keys and the response carries `Vary: User-Agent`, so no cache can
hand the shell script to a Windows client, and purging the two canonical paths
also drops `/install`.

GitHub answers an asset URL with a 302 to a signed `release-assets` URL that
expires in minutes. The worker follows that redirect and stores only the final
200 body, keyed by the public URL (`https://<hostname>/dl/...`). A zone purge of
that URL drops the copy. Redirects and non-200 responses are not stored.
Redirect targets are limited to GitHub hostnames.

The worker is not an open proxy: the upstream repository is baked into the
source, and a path matching none of the regexes above never reaches an upstream
at all — it is answered by the site. Republishing the same tag does not refresh
an asset already cached for a year; purge that exact `/dl/...` URL if you
replace one.

Both installers verify each download against the digest GitHub records for that
asset in the release JSON (`/latest` or `/tags/<tag>`), so there is nothing to
compute or publish here. Because that JSON is cached for 60 s while the binary is
cached for a year, a re-published tag stops being installable rather than
silently reinstalling the cached binary — purge the `/dl/<tag>/<asset>` URL to
recover.

## Setup

1. Put your hostname in `wrangler.toml`'s `routes`. The zone must already be
   hosted on Cloudflare for `custom_domain` to be created. `public/index.html`
   ships with `https://oven.paulden.site` as the fallback mirror, so update it
   when renaming the zone.
2. `npx wrangler@latest login`
3. `npx wrangler@latest secret put GITHUB_TOKEN` — optional but recommended: a
   fine-grained PAT with public-repository read access lifts `/latest` and
   `/tags/<tag>` from 60 to 5000 requests per hour. With the 60 s cache they are
   hit a few times an hour.
4. `npx wrangler@latest deploy`
5. `DEFAULT_MIRROR` in `scripts/install.sh` and `scripts/install.ps1` already
   points at the zone. Setting `OVEN_MIRROR` (or `$env:OVEN_MIRROR`) to an empty
   string makes an install skip the mirror and go straight to GitHub.

Releases do not purge anything. `/latest` and the installers can lag up to their
60 s TTL after a release, so an install in that window gets the previous version
and a later run gets the new one; the digests always match the tag they resolve.

## Checking

```console
$ curl -fsSL -D - -o /dev/null https://<hostname>/latest | grep -i x-oven-cache
$ curl -fsSL -D - -o /dev/null https://<hostname>/dl/v0.0.9/oven-v0.0.9-x86_64-unknown-linux-musl.tar.gz | grep -iE 'x-oven-cache|content-length'
$ curl -fsSL https://<hostname>/tags/v0.0.9 | head -c 120
$ curl -fsSL https://<hostname>/downloads | grep -c app.js
$ curl -fsSL -o /dev/null -w '%{http_code}\n' https://<hostname>/icon.svg
```

The first asset request is `x-oven-cache: MISS`, the second is `HIT`, and
`content-length` matches the GitHub release asset.

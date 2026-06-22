# Registry Health Baselines

This file records small, reproducible registry-health snapshots. It is a
summary log, not a raw artifact store: live doctor JSON is intentionally
not committed because site responses, random absent usernames, rate
limits, and operator egress vary between runs.

## 2026-06-19 Direct Top-50 Snapshot

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Default embedded registry with the normal WMN merge enabled.
- Popularity-ranked entries only; `--top 50` selected 40 ranked sites.
- Direct local egress, no proxy pool, no browser backend, no operator
  sessions.
- `--max-retries 0` so rate limits and protection surfaces remain visible
  instead of being retried away.

Summary:

- Total: 40 sites.
- Healthy: 27.
- Unhealthy: 13.

Healthy high-value checks included TikTok, Pinterest, Patreon, GitHub,
GitLab, BitBucket, YouTube, Wikipedia, Snapchat, Twitch, Telegram,
Medium, Tumblr, SoundCloud, Vimeo, Flickr, Behance, Dribbble, WordPress,
Blogger, Bandcamp, MixCloud, last.fm, Docker Hub, Keybase, HackerOne,
and dev.to.

Unhealthy entries:

| Site | Observed issue | Initial bucket |
| --- | --- | --- |
| Instagram | known-present users reported `NotFound` | access/browser research |
| Twitter | known-present users reported `Uncertain` | access/browser research |
| X | known-present users reported `NotFound` | access/browser research |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| VK | known-present user reported `Uncertain` | access or endpoint research |
| Weibo | random absent username reported `Found` | fixed: profile API is session-gated; embedded cookies removed and unauthenticated scans now return `session_required` |
| DeviantArt | known-present users reported `Uncertain` | access or endpoint research |
| Ko-Fi | known-present users hit `cloudflare_challenge`; random absent also reported `Found` | mixed protection + signature |
| StackOverflow | random absent username reported `Found` | fixed: switched to StackExchange API exact username evidence |
| npm | known-present user reported `Uncertain` | access or endpoint research |
| pypi | random absent username reported `Found` | fixed: removed `200 == Found`; strict profile username marker now keeps JS challenge responses uncertain |
| Replit | random absent username reported `Found` | fixed: profile route is session-gated; unauthenticated scans now return `session_required` instead of `Found` |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- Do not convert Instagram, X/Twitter, or similar social login-wall
  surfaces into `Found`/`NotFound` fixes from this snapshot alone.
- Reddit is expected to require the named `reddit` session path; this
  direct run confirms the unauthenticated path stays honest.
- The direct-run false-positive registry candidates from this snapshot
  are now closed: StackOverflow uses exact API evidence, PyPI keeps JS
  challenges uncertain, and Replit/Weibo are explicit session-gated
  probes.
- The first access-policy candidates are protection-heavy sites with
  repeatable Cloudflare evidence, starting with CodePen and Ko-Fi only
  after a targeted mock/live check proves the registry metadata change
  does not mask a broken signature.

## 2026-06-19 Direct Top-50 Snapshot After False-Positive Fixes

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same scope as the earlier 2026-06-19 direct snapshot.
- Run after the StackOverflow, PyPI, Replit, and Weibo registry repairs
  landed.
- Direct local egress, no proxy pool, no browser backend, no operator
  sessions.

Summary:

- Total: 40 sites.
- Healthy: 28.
- Unhealthy: 12.

Healthy checks included YouTube, Wikipedia, TikTok, GitHub, Pinterest,
Snapchat, Twitch, Telegram, Medium, Tumblr, SoundCloud, Vimeo, Flickr,
Behance, Dribbble, WordPress, Blogger, Bandcamp, MixCloud, last.fm,
Patreon, GitLab, BitBucket, StackOverflow, Docker Hub, Keybase,
HackerOne, and dev.to.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | known-present users reported `NotFound` | access/browser research |
| Twitter | known-present users reported `Uncertain` | access/browser research |
| X | known-present users reported `NotFound` | access/browser research |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| VK | known-present user reported `Uncertain` | access or endpoint research |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | access or endpoint research |
| Ko-Fi | known-present users hit `cloudflare_challenge`; random absent still reported `Found` | remaining false-positive signature candidate |
| npm | known-present user reported `Uncertain` | access or endpoint research |
| pypi | known-present users reported `Uncertain` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- StackOverflow moved from the false-positive queue to healthy after the
  StackExchange API exact-username evidence change.
- PyPI, Replit, and Weibo no longer produce unauthenticated false
  `Found` results in this direct run. They remain visible as
  protection/session cases, which is the expected conservative behavior.
- Ko-Fi is the only remaining direct-run false-positive candidate in
  this top set. Treat it as the next focused registry investigation:
  first prove whether a stable exact marker or API-backed endpoint
  exists; if not, prefer protection/session gating over a permissive
  `Found` rule.
- CodePen still shows repeatable Cloudflare challenge behavior for
  known-present users, but no absent-user false positive in this run.

## 2026-06-19 Direct Top-50 Snapshot After X/VK Service Fixes

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same scope as the earlier 2026-06-19 direct snapshots.
- Run after the X username-availability signal fix, the VK canonical
  profile marker fix, Ko-Fi's status-only Found removal, and
  DeviantArt's CloudFront/bot-protected classification.
- Direct local egress, no proxy pool, no browser backend, no operator
  sessions.

Summary:

- Total: 40 sites.
- Healthy: 30.
- Unhealthy: 10.

Newly healthy since the previous direct snapshot:

- X: the username-availability endpoint now keys off the JSON
  `reason` marker instead of treating HTTP 200 as both Found and
  NotFound.
- VK: the profile probe now requires the exact canonical profile marker
  and no longer conflicts with the old redirect rule.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | known-present users reported `NotFound` | access/browser research |
| Twitter | known-present users reported `Uncertain` | access/browser research |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | CloudFront/browser research |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| npm | known-present user reported `Uncertain` | access or endpoint research |
| pypi | known-present users reported `Uncertain` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- Ko-Fi is no longer a direct-run false-positive candidate after the
  status-only Found rule was removed. It remains a protected profile
  surface until a stable exact marker can be reached through a browser
  or a supported public endpoint exists.
- DeviantArt's current direct failure is an edge-protection issue, not a
  signature issue observed from this egress.
- The next high-value registry work should focus on npm endpoint
  semantics and browser/protection routing for CodePen, Ko-Fi, and
  DeviantArt.

## 2026-06-19 Direct Top-50 Snapshot After npm API Fix

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same scope as the earlier 2026-06-19 direct snapshots.
- Run after moving npm from the Cloudflare-protected web profile to the
  public registry search API with exact maintainer username evidence.
- Direct local egress, no proxy pool, no browser backend, no operator
  sessions.

Summary:

- Total: 40 sites.
- Healthy: 31.
- Unhealthy: 9.

Newly healthy since the previous direct snapshot:

- npm: the probe now uses
  `registry.npmjs.org/-/v1/search?text=maintainer:{username}` and
  requires exact `"username":"{username}"` evidence. Empty search
  results are classified by `"total":0`.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | known-present users reported `NotFound` | access/browser research |
| Twitter | known-present users reported `Uncertain` | access/browser research |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | CloudFront/browser research |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| pypi | known-present users reported `Uncertain` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- npm's new signal detects public maintainer evidence, not private npm
  accounts with no published package ownership.
- The remaining direct-run failures are now mostly access/session or
  browser-protection cases rather than obvious top-set raw signature
  bugs.

## 2026-06-21 Direct Top-50 Snapshot After Instagram Session API Model

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same direct local-egress scope as the 2026-06-19 snapshots.
- Run after moving canonical Instagram from the generic HTML profile
  shell to `api/v1/users/web_profile_info` with `X-IG-App-ID`, exact
  `/data/user/username` JSON evidence, and an explicit
  `access.session = instagram` requirement.
- No proxy pool, no browser backend, no operator sessions.

Summary:

- Total: 40 sites.
- Healthy: 30.
- Unhealthy: 10.

Changed since the previous direct snapshot:

- Instagram no longer reports known-present users as `NotFound` from
  the generic HTML shell. The canonical probe is now an operator-session
  API path: without an `instagram` session it returns
  `Uncertain(session_required)`, and with a session it can produce exact
  username evidence from `/data/user/username`.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Twitter | known-present users reported `Uncertain` | access/API research |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | CloudFront/browser research |
| last.fm | known-present user intermittently reported `Uncertain` | transient direct-run flake |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| pypi | known-present users reported `Uncertain` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- Instagram is no longer a canonical HTML-shell probe in the direct top
  set. The separate `Instagram (Imginn)` and `Instagram_archives`
  registry entries remain independent aliases with their own health.
- `last.fm` flapped during this local run; targeted reruns alternated
  between healthy and `Uncertain`, so treat it as a separate stability
  candidate rather than part of the Instagram change.
- The remaining direct-run failures are access/session/protection
  problems or services without a stable public exact-username endpoint
  from this egress.

## 2026-06-21 Direct Top-50 Snapshot After X/Twitter API Alias Cleanup

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same direct local-egress scope as the earlier 2026-06-21 snapshot.
- Run after moving the legacy `Twitter` registry entry from the
  `x.com/{username}` HTML profile shell to the X username-availability
  API.
- `X` and `Twitter` are both raw API probes in this snapshot; neither
  is routed as `bot-protected`.
- No proxy pool, no browser backend, no operator sessions.

Summary:

- Total: 40 sites.
- Healthy: 32.
- Unhealthy: 8.

Changed since the previous direct snapshot:

- `Twitter` now reports healthy from the API instead of `Uncertain`
  from the HTML shell.
- `X` remains healthy on the same public API model.
- `last.fm` was healthy in this run; the previous transient flake
  remains noted as a stability candidate, not a registry signature fix.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | CloudFront/browser research |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| pypi | known-present users reported `Uncertain` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- The legacy `Twitter` name remains available for compatibility, but it
  no longer duplicates the browser-only profile-shell behavior.
- The harmless `suggest=0` query parameter keeps `Twitter` and `X` as
  distinct registry URLs so the default+WMN merge does not drop the WMN
  `X` entry.
- Remaining unhealthy entries are now concentrated in explicit
  session-required paths, protected profile surfaces, or PyPI's client
  challenge.

## 2026-06-22 Direct Top-50 Snapshot After PyPI Client-Challenge Classification

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same direct local-egress scope as the 2026-06-21 snapshots.
- Run after promoting PyPI's generic browser challenge to structured
  `protection: ["client-challenge"]` and
  `Uncertain(client_challenge)`.
- No proxy pool, no browser backend, no operator sessions.

Summary:

- Total: 40 sites.
- Healthy: 31.
- Unhealthy: 9.

Changed since the previous direct snapshot:

- PyPI now reports `Uncertain(client_challenge)` instead of an
  unclassified `Uncertain` for both known-present and random usernames.
- `last.fm` flapped back to `Uncertain` in this run; previous targeted
  and top-set runs showed it can alternate between healthy and
  unhealthy from the same direct egress.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users reported `Uncertain` | CloudFront/browser research |
| last.fm | known-present user intermittently reported `Uncertain` | transient direct-run flake |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| pypi | known-present users reported `Uncertain(client_challenge)` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- This snapshot is a diagnostics improvement, not a recall increase:
  PyPI remains unhealthy without a browser/session path, but its failure
  mode is now machine-readable.
- CodePen and Ko-Fi still present Cloudflare challenges across profile
  and feed-style endpoints from this egress.
- DeviantArt profile, oEmbed, and RSS-style endpoints still return
  CloudFront blocks from this egress.

## 2026-06-22 Direct Top-50 Snapshot After CloudFront Challenge Classification

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --top 50 --no-progress --no-cache \
  --max-retries 0 --timeout 8 --concurrency 12 \
  --format json
```

Scope:

- Same direct local-egress scope as the earlier 2026-06-22 snapshot.
- Run after classifying HTTP 403 CloudFront edge blocks as
  `Uncertain(cloudfront_challenge)`.
- No proxy pool, no browser backend, no operator sessions.

Summary:

- Total: 40 sites.
- Healthy: 32.
- Unhealthy: 8.

Changed since the previous direct snapshot:

- DeviantArt now reports `Uncertain(cloudfront_challenge)` instead of an
  unclassified `Uncertain` from its CloudFront edge block.
- `last.fm` returned to healthy in this run, matching its observed
  flapping behavior across direct local-egress snapshots.

Unhealthy entries:

| Site | Observed issue | Current bucket |
| --- | --- | --- |
| Instagram | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Reddit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| Weibo | `Uncertain(session_required)` without operator credentials | expected session-gated |
| DeviantArt | known-present users hit `cloudfront_challenge` | CloudFront/browser research |
| Ko-Fi | known-present users hit `cloudflare_challenge` | Cloudflare/browser research |
| pypi | known-present users reported `Uncertain(client_challenge)` behind the client challenge | fixed false-positive class; access/protection research remains |
| Replit | `Uncertain(session_required)` without operator credentials | expected session-gated |
| CodePen | known-present users hit `cloudflare_challenge` | protection metadata candidate |

Triage notes:

- This is also a diagnostics improvement, not a recall increase:
  DeviantArt remains blocked from this egress, but downstream consumers
  can now distinguish CloudFront blocks from generic uncertainty.
- The remaining non-session protected surfaces are now explicitly
  classified as Cloudflare, CloudFront, or generic client challenge.

## 2026-06-22 CodePen Endpoint Research

Command samples:

```bash
curl -sS -L --max-time 12 -D - https://codepen.io/good88gorg
curl -sS -L --max-time 12 -D - https://codepen.io/RayyanDonut
curl -sS -L --max-time 12 -D - https://codepen.io/good88gorg.json
curl -sS -L --max-time 12 -D - https://codepen.io/api/users/good88gorg
curl -sS -L --max-time 12 -D - https://codepen.io/good88gorg/public/feed
curl -sS -L --max-time 12 -D - 'https://codepen.io/api/oembed?url=https://codepen.io/good88gorg'
```

Observed result:

- Known-present users, a synthetic missing username, `.json`, `api/users`,
  public feed, public pens, and oEmbed probes all returned the same
  `HTTP 403` Cloudflare challenge from this direct local egress.
- CodePen's own API documentation says there is no public REST or GraphQL
  data API for CodePen profiles. The documented API-like surface is
  oEmbed/prefill oriented, not a username lookup endpoint.

Decision:

- Do not add an unofficial third-party API dependency.
- Do not infer `Found` from HTTP `200` if CodePen becomes reachable
  through a generic shell or challenge page.
- Keep CodePen tagged as Cloudflare/browser-protected and require profile
  metadata before producing `Found`.

## 2026-06-19 Persisted Scan Protection Telemetry

Command:

```bash
cargo run -q -p adler-cli -- \
  --doctor --suggest-protection \
  --scans-dir "$HOME/.cache/adler/scans"
```

Scope:

- 26 local persisted scans.
- Telemetry-only: no live registry health probe is issued by this
  command.
- Default threshold: at least 60% escalation evidence over at least 3
  scans.

Summary:

- 79 sites met the threshold for `protection: ["cloudflare"]`.
- Highest-confidence repeated candidates were sites with 100% escalation
  evidence across 20 or 21 observations: AvidCommunity, Fur Affinity,
  Huntingnet, Nairaland Forum, Smule, Steamid, Steamid (by id),
  Steamidfinder, Steamidfinder (by id), Storycorps, Sythe,
  TalkDrugabuse, TechSpot, furaffinity, and steamdb.info.

Triage notes:

- Do not bulk-apply all suggestions. Treat the output as a queue.
- Prefer one small PR per failure class or site family.
- Before adding `protection: ["cloudflare"]`, check whether the site is
  already disabled, duplicated under another name, or better served by a
  stable API/feed/profile-card endpoint.
- For high-volume suggestions, add a registry guard or doctor fixture
  when the metadata change is likely to affect routing behavior.

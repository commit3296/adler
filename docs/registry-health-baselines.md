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
| Weibo | random absent username reported `Found` | too-permissive signature |
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
- The next registry-fix candidate is the remaining too-permissive
  signature: Weibo.
- The first access-policy candidates are protection-heavy sites with
  repeatable Cloudflare evidence, starting with CodePen and Ko-Fi only
  after a targeted mock/live check proves the registry metadata change
  does not mask a broken signature.

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

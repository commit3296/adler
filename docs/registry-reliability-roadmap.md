# Registry Reliability v2 roadmap

This roadmap picks up after the v0.15 identity-graph work and the TikTok
oEmbed restoration. The goal is to make site coverage easier to keep
healthy without weakening Adler's honest-verdict model.

## Principles

- Prefer stable API, oEmbed, ActivityPub, RSS, profile-card, or metadata
  endpoints over hydrated web pages.
- Keep `Uncertain(reason)` honest when a site blocks reliable detection.
- Do not turn login walls, CAPTCHA, generic shell pages, or JS hydration
  placeholders into `Found`.
- Keep registry fixes data-driven where possible: `sites.json`,
  signatures, access policies, and doctor metadata before custom code.
- Add contract or doctor fixtures before changing confidence, evidence,
  avatar, or clustering semantics.
- Treat site-specific browser research as opt-in and bounded by budgets.

## Track 1: Site Health Inventory

- [x] Commit a direct top-50 doctor baseline summary:
  [registry-health-baselines.md](registry-health-baselines.md).
- [ ] Produce a current doctor baseline from datacenter, residential, and
  optional browser-backed runs.
- [x] Classify the first direct-run failures by dominant reason:
  `cloudflare_challenge`, `rate_limited`, `session_required`,
  `geo_unavailable`, `browser_budget`, stale `known_present`, and
  signal drift.
- [ ] Split failures into registry-only fixes, access-policy fixes,
  endpoint-research candidates, and intentionally parked sites.
- [ ] Add a short per-release health summary to changelog or release notes
  when registry reliability meaningfully changes.

## Track 2: Automated Triage

- [x] Record a persisted-scan `--suggest-protection` summary and keep the
  output as a review queue instead of bulk-applying it:
  [registry-health-baselines.md](registry-health-baselines.md).
- [ ] Make nightly doctor output easier to prioritize by grouping repeated
  failures by reason, tag, popularity, and protection metadata.
- [ ] Keep auto-opened registry-health PRs small: one failure class or one
  site family per PR.
- [ ] Prefer suggested patches with evidence snippets over broad generated
  changes.
- [ ] Add regression fixtures for repaired high-value sites so future
  registry edits cannot silently undo the fix.

## Track 3: Stable Endpoint Research

TikTok current status:

- [x] Restore TikTok through its public oEmbed endpoint instead of the
  hydrated profile shell.
- [x] Require deterministic regression coverage for the TikTok oEmbed
  shape: live `Found` response, `400` missing-user response, exact
  username evidence, and `--doctor` health behavior.
- [x] Keep TikTok in the smoke set when refreshing registry health
  baselines, because the endpoint is public but still externally owned.
  Use deterministic fixtures for CI-grade acceptance; live smoke is an
  operator check because the endpoint is externally owned.

Research order after TikTok:

1. **Pinterest** — keep the current public endpoint approach healthy.
   - [x] Use public oEmbed instead of the canonical JS shell.
   - [x] Require exact username evidence from the returned `author_url`.
   - [x] Cover found, 404 missing-user, and doctor behavior with
     deterministic fixtures.
   - [x] Document fallback behavior if the endpoint starts rate-limiting
     or hiding unavailable profiles: keep `Uncertain` honest, prefer a
     different stable metadata endpoint, and do not fall back to the
     canonical JS shell unless it is explicitly browser/access modeled.
2. **Reddit** — validate the authenticated session path and app-only OAuth
   guidance against current API behavior; do not imply unauthenticated
   absence when Reddit blocks profile visibility.
   - [x] Keep Reddit behind the named `reddit` operator session.
   - [x] Use authenticated OAuth JSON rather than the unauthenticated
     profile shell.
   - [x] Require exact username evidence from `/data/name`.
   - [x] Cover session-backed doctor behavior with deterministic fixtures.
   - [ ] Run a live OAuth smoke when operator credentials are available.
3. **Patreon** — keep the current status-only profile probe honest while
   looking for a stronger metadata path.
   - [x] Verify the current public profile route still distinguishes
     known-present and synthetic missing users through HTTP status
     (`200`/`404`).
   - [x] Document that the plain HTML body is not a stable exact-username
     source: known-present profiles can redirect into generic Patreon
     shells such as `/profile/creators?u=...` or `/cw/...`.
   - [x] Cover the status-only doctor behavior with a deterministic
     fixture.
   - [x] Add a registry guard that prevents accidental `body_username` or
     `json_username` evidence from being inferred from the generic HTML
     shell.
   - [ ] Revisit only when a stable public metadata endpoint, explicit
     browser-backed signal, or operator-session path can produce
     username-confirming evidence without leaking secrets.
4. **Instagram** — use the `web_profile_info` JSON endpoint with the
   required `X-IG-App-ID` header, explicit operator session, and exact
   username evidence.
   - [x] Replace the generic HTML profile-shell signature with the API
     endpoint.
   - [x] Require exact `/data/user/username` JSON evidence.
   - [x] Treat API HTTP 404 as NotFound.
   - [x] Keep the canonical probe on the raw HTTP session API path
     rather than routing it as `bot-protected`.
5. **X / Twitter** — no false NotFound, no
   CAPTCHA solving, no fragile hydrated-page scraping as a default signal.
6. **Threads** — revisit only if a stable public profile endpoint appears
   or an operator-session path can be modeled without leaking secrets.

## Track 4: Access Policy Cleanup

- [x] Classify parked login-wall social entries with explicit
  `protection: ["user-auth"]` while keeping them disabled and excluded
  from scans.
- [ ] Audit high-volume `Uncertain` sites for missing `access` metadata:
  geo, IP type, browser requirement, TLS impersonation, or session need.
- [ ] Convert repeated datacenter-only failures into explicit access
  policies where residential/mobile/browser paths are known to work.
- [ ] Keep session names and proxy details out of evidence metadata,
  persisted artifacts, reports, and docs examples.
- [ ] Add docs examples only for generic TOML shapes, never operator
  secrets.

## Track 5: Contract Discipline

- [ ] Every new evidence kind, confidence reason, cluster reason, report
  field, MCP DTO field, or persisted-scan wire change gets a contract
  test before release.
- [ ] Site-specific fixes that can emit new evidence should update CLI,
  Web, MCP, and report snapshots where applicable.
- [ ] Browser-only success should improve confidence only when it produces
  a clear verdict and supporting evidence.
- [ ] Username-only, avatar-hash-only, and template-only URL presence must
  remain insufficient for hard identity merges.

## Track 6: Metrics To Watch

- Found / NotFound / Uncertain distribution by scan source.
- Top repeated `UncertainReason` values.
- Known-present failures by site popularity and tag.
- Sites repaired by doctor suggestions versus manual endpoint research.
- Contract snapshot churn caused by registry changes.

## First PR Slice

- [x] Generate and commit a fresh doctor baseline artifact or summary.
- [x] Pick one target from the baseline queue and write a short
  investigation note with response examples.
- [x] Repair only that target if a stable signal exists.
- [x] Add targeted tests or fixtures proving the new signal does not
  create false `Found` results for random usernames.

## Second PR Slice

- [x] Repair PyPI's false-positive signature by removing `200 == Found`
  and requiring a strict profile username marker.
- [x] Mark PyPI's raw user profile path as bot-protected/protection-other
  so browser/access research is explicit.
- [x] Add targeted tests proving PyPI's client-challenge shell does not
  create false `Found` results for random usernames.
- [x] Repair Replit's false-positive signature by requiring the named
  `replit` session and exact username evidence instead of `200 == Found`.
- [x] Add targeted tests proving a missing Replit session skips the
  network probe and returns `session_required`.
- [x] Repair Weibo's false-positive signature by requiring the named
  `weibo` session, removing embedded cookies, and removing `200 == Found`.
- [x] Add targeted tests proving Weibo has no embedded Cookie header and
  stays session-gated in both embedded registry sources.

## Third PR Slice

- [x] Refresh the direct top-set doctor baseline after the
  StackOverflow, PyPI, Replit, and Weibo false-positive repairs.
- [x] Investigate Ko-Fi as the remaining direct-run false-positive
  candidate from the refreshed top-set baseline.
- [x] Remove Ko-Fi's status-only Found rule so Cloudflare challenge
  shells stay `Uncertain` instead of producing false positives.
- [x] Repair X's username-availability API signature by removing the
  conflicting HTTP 200 Found/NotFound status rules.
- [x] Repair VK's profile signature by replacing the redirect conflict
  with exact canonical profile evidence.
- [x] Classify DeviantArt as CloudFront/bot-protected based on the live
  direct-run edge block.
- [x] Refresh the direct top-set doctor baseline after the X/VK/Ko-Fi
  service fixes.
- [x] Move npm to the public registry search API with exact maintainer
  username evidence.
- [x] Refresh the direct top-set doctor baseline after the npm API fix.
- [x] Move canonical Instagram to a session-gated `web_profile_info`
  path with exact `/data/user/username` evidence and `X-IG-App-ID`.
- [x] Refresh the direct top-set doctor baseline after the Instagram
  session API model.
- [ ] Keep CodePen as a protection-metadata candidate unless targeted
  research finds a stable exact evidence endpoint.

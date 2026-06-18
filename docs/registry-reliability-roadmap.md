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

- [ ] Produce a current doctor baseline from datacenter, residential, and
  optional browser-backed runs.
- [ ] Classify failures by dominant reason:
  `cloudflare_challenge`, `rate_limited`, `session_required`,
  `geo_unavailable`, `browser_budget`, stale `known_present`, and
  signal drift.
- [ ] Split failures into registry-only fixes, access-policy fixes,
  endpoint-research candidates, and intentionally parked sites.
- [ ] Add a short per-release health summary to changelog or release notes
  when registry reliability meaningfully changes.

## Track 2: Automated Triage

- [ ] Make nightly doctor output easier to prioritize by grouping repeated
  failures by reason, tag, popularity, and protection metadata.
- [ ] Keep auto-opened registry-health PRs small: one failure class or one
  site family per PR.
- [ ] Prefer suggested patches with evidence snippets over broad generated
  changes.
- [ ] Add regression fixtures for repaired high-value sites so future
  registry edits cannot silently undo the fix.

## Track 3: Stable Endpoint Research

Research order after TikTok:

1. **Pinterest** — keep the current public endpoint approach healthy and
   document fallback behavior if the endpoint starts rate-limiting or
   hiding unavailable profiles.
2. **Reddit** — validate the authenticated session path and app-only OAuth
   guidance against current API behavior; do not imply unauthenticated
   absence when Reddit blocks profile visibility.
3. **Patreon** — investigate whether a stable profile metadata endpoint or
   browser-backed signal can distinguish real profiles from generic walls.
4. **Instagram** — keep parked/bot-protected by default unless a stable,
   responsible, non-CAPTCHA signal is available through an operator-owned
   session or explicit browser path.
5. **X / Twitter** — same policy as Instagram: no false NotFound, no
   CAPTCHA solving, no fragile hydrated-page scraping as a default signal.
6. **Threads** — revisit only if a stable public profile endpoint appears
   or an operator-session path can be modeled without leaking secrets.

## Track 4: Access Policy Cleanup

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

- [ ] Generate and commit a fresh doctor baseline artifact or summary.
- [ ] Pick one target from the research order and write a short
  investigation note with response examples.
- [ ] Repair only that target if a stable signal exists.
- [ ] Add targeted tests or fixtures proving the new signal does not
  create false `Found` results for random usernames.

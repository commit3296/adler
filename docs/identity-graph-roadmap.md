# Identity Graph roadmap

Long-term checklist for turning Adler from a username availability
scanner into an investigation layer: evidence, confidence, account
correlation, timelines, watchlists, and reports.

This file is intentionally written as a working checklist. Keep completed
platform work visible so future implementation does not lose the context
that already exists.

## Product target

- [x] Adler records not only whether a profile was found, but why that
  result is trustworthy.
- [x] Every positive result can expose normalized evidence collected from
  the profile page, API endpoint, registry rule, or authenticated access
  path.
- [x] Every result can carry a confidence score and human-readable
  reasons.
- [x] Multiple profile results can be grouped into probable identities
  when they share strong evidence.
- [x] Historical scans can become timelines, watchlists, and
  investigation reports.
- [x] CLI, Web, and MCP surfaces can expose the same underlying evidence
  without inventing separate product logic.

## Already shipped foundation

- [x] Rust workspace split into focused crates:
  `adler-core`, `adler-cli`, `adler-server`, and `adler-mcp`.
- [x] Data-driven registry with enabled sites, disabled sites, tags,
  detection signals, protection metadata, and inheritable engine
  signatures.
- [x] Multi-signal profile detection via status, body, redirect, and
  registry-authored rules.
- [x] Access engine with local HTTP, impersonation transport, proxy/egress
  selection, FlareSolverr, local browser, Browserbase, and operator
  sessions.
- [x] Doctor tooling for registry maintenance:
  `--fix`, `--suggest-known-present`, `--suggest-extract`,
  `--suggest-protection`, JSON/NDJSON output, and atomic apply.
- [x] Nightly doctor workflow that tracks repeated failures and opens
  registry-health PRs.
- [x] Web UI with live scan streaming, batch scans, scan history,
  compare-with-previous, per-scan egress selection, and access telemetry.
- [x] MCP server with scan, batch, doctor, site listing, scan history,
  resources, and prompts for agents.
- [x] Disabled/parked site diagnostics are surfaced consistently across
  CLI, Web/API, and MCP instead of being hidden as missing matches.
- [x] Reddit authenticated access path is modeled as an operator session
  and can be derived from app-only OAuth credentials.

## Phase 0: Architecture specification

- [x] Capture the long-term direction in this roadmap.
- [x] Add a short architectural sketch showing the intended data flow:
  registry -> probe -> outcome -> evidence -> confidence -> identity
  cluster -> timeline/report -> CLI/Web/MCP.
- [x] Decide which terms become stable public API names:
  `ProfileEvidence`, `ObservedProfile`, `ConfidenceScore`,
  `IdentityCluster`, `InvestigationReport`.
- [x] Define which fields are experimental and can change before the
  next semver-relevant release.
- [x] Link follow-up GitHub issues or milestones from this checklist once
  they exist.

See [identity-graph-architecture.md](identity-graph-architecture.md)
for the data flow, public API terms, stability policy, and follow-up
issue links.

## Phase 1: Evidence model

- [x] Add a normalized evidence model in `adler-core`.
- [x] Keep evidence collection separate from the low-level site check
  path so `Client::check` does not absorb product-level logic.
- [x] Model normalized evidence kinds for current enrichment fields:
  display name, profile title, meta description, avatar, bio text,
  external links, location, created/joined date, and generic extracted
  fields.
- [x] Include source metadata for each evidence item:
  site, URL, and extraction origin.
- [x] Extend evidence source metadata with transport/access path and
  timestamp. ([#75](https://github.com/commit3296/adler/issues/75))
- [x] Add explicit username-match evidence once the detection pipeline can
  distinguish username confirmation from generic positive site signals.
- [x] Add serde-compatible structs that can be reused by CLI JSON, Web
  API, persisted scan history, and MCP.
- [x] Add focused unit tests for evidence serialization and backwards
  compatible defaults.

## Phase 2: Confidence engine

- [x] Add a rule-based `adler-core` confidence module.
- [x] Produce a numeric score and a label such as
  `low`, `medium`, `high`, or `verified`.
- [x] Return machine-readable and human-readable confidence reasons.
- [x] Account for current positive signals:
  signal evidence, extracted profile metadata, authenticated access,
  browser/impersonating transport, and successful escalation.
  ([#76](https://github.com/commit3296/adler/issues/76))
- [x] Account for current negative or weakening signals:
  weak status-only detection, blocked transport, missing session,
  geo-unavailable access, CAPTCHA/rate-limit/browser-budget limits.
- [x] Add exact username-match scoring once that signal is modeled
  explicitly.
- [x] Add repeated historical consistency scoring once that signal is
  modeled explicitly.
- [x] Add tests for representative confidence cases before surfacing the
  score in UI.

## Phase 3: Versioned scan artifact

- [x] Introduce or extend a versioned persisted scan schema.
- [x] Store result-level evidence and confidence without breaking older
  scan history.
- [x] Preserve access telemetry and session-required states in
  the same artifact.
- [x] Preserve disabled/parked filter context alongside scan artifacts.
- [x] Add migration or tolerant-read behavior for older scan records.
- [x] Add serialization tests for persisted scan history.

## Phase 4: CLI, Web, and MCP surfaces

- [x] Expose evidence and confidence in CLI JSON/NDJSON output.
- [x] Keep human CLI output compact:
  found/missing/session-required/disabled plus confidence label.
- [x] Add Web UI confidence badges and a result detail panel for evidence.
- [x] Add Web API fields without breaking legacy clients that expect the
  current response shape.
- [x] Return evidence and confidence from MCP scan tools so agents can
  reason over Adler results without scraping presentation text.
- [x] Update MCP prompts to instruct agents to cite evidence and
  limitations.

## Phase 5: Identity clustering

- [x] Add `IdentityCluster` and `ClusterReason` models in `adler-core`.
  ([#77](https://github.com/commit3296/adler/issues/77))
- [x] Build deterministic correlation rules before considering any ML:
  shared external link, avatar URL equality, avatar perceptual hash,
  display name, bio phrase, and location.
- [x] Add historical co-occurrence as a read-time support signal for
  already-linked profiles.
- [x] Add avatar hashing once image fetch, cache, privacy, and hashing
  policy are designed.
- [x] Assign cluster confidence separately from per-profile confidence.
- [x] Make uncertain clusters explicit instead of silently merging weakly
  related accounts.
- [x] Add persisted scan, Web API, MCP, and compact Web output for
  cluster candidates.
- [x] Add tests that prevent over-merging based on username-only matches.

## Phase 6: Timeline and watchlists

- [x] Add a deterministic scan-to-scan diff model for added, removed,
  verdict-changed, and evidence-changed results.
- [x] Expose scan-to-scan diffs through the Web API.
- [x] Expose scan-to-scan diffs through MCP tools and resources.
- [x] Add watchlist configuration for usernames, aliases, and optional
  site/tag scopes.
- [x] Support scheduled or repeated scans without coupling scheduling to
  the core engine.
- [x] Track first seen, last seen, disappeared, reappeared, and changed
  evidence.
- [x] Expose scan timelines through the Web API.
- [x] Show scan-to-scan diffs in Web UI.
- [x] Expose timeline summaries through MCP resources.
- [x] Expose watchlist summaries through MCP resources.
- [x] Keep operator privacy and local-storage expectations explicit.

## Phase 7: Investigation reports

- [x] Add a report model that consumes scan artifacts, evidence,
  confidence, clusters, and timeline events.
  ([#78](https://github.com/commit3296/adler/issues/78))
- [x] Generate Markdown reports first.
- [x] Include sections for summary, high-confidence accounts, uncertain
  accounts, evidence table, timeline, parked/disabled sites, and known
  limitations.
- [x] Add JSON report output for downstream tools.
- [x] Add self-contained HTML export after the report model stabilized.
- [x] Expose report exports through Web API and finished-scan Web controls.
- [x] Expose reports through MCP tool/resource surfaces.
- [x] Add snapshot tests for report rendering.

## Phase 8: Hardening and release readiness

- [x] Add compatibility tests for public JSON, Web API, and MCP output.
  ([#79](https://github.com/commit3296/adler/issues/79))
- [x] Add UI tests for confidence/evidence rendering.
- [x] Add performance checks for large scan artifacts.
- [x] Document privacy, retention, and responsible-use considerations.
- [x] Update `README.md` once the first user-visible slice ships.
- [x] Update `CHANGELOG.md` only when implementation lands in a release.

## Phase 9/10: Avatar hashing

- [x] Design privacy-safe avatar hashing before implementation:
  external image fetching must be opt-in, raw image bytes must never be
  persisted, and hashes must not include operator-specific access data.
- [x] Add a bounded avatar fetch/hash helper with response size,
  content-type, timeout, and redirect limits.
- [x] Add avatar perceptual hash as a supporting identity-cluster signal
  without replacing `SharedAvatarUrl`.
- [x] Prevent username-only and avatar-hash-only hard merges.
- [x] Surface the new signal through CLI, Web, MCP, and reports only
  after the core model and contract tests are stable.
- [x] Keep Web/MCP from fetching external avatar images in v1; they only
  read and render avatar hash evidence already present in artifacts.

## Post-v0.15: Next tracks

Identity graph foundation work is complete as of v0.15.0. New work
should build on it instead of extending this checklist indefinitely.

- [x] Restore TikTok detection through its public oEmbed endpoint and close
  the remaining hydration issue:
  [#12](https://github.com/commit3296/adler/issues/12).
- [x] Harden Pinterest through public oEmbed with exact username evidence
  from `author_url`.
- [x] Harden Reddit through an operator-session OAuth JSON path with exact
  username evidence from `/data/name`.
- [x] Draft a Registry Reliability v2 roadmap for site health, flaky
  detection triage, and automated registry maintenance:
  [registry-reliability-roadmap.md](registry-reliability-roadmap.md).
- [x] Decide which JS-heavy or bot-protected sites deserve targeted
  detection research after TikTok. Initial order: Pinterest, Reddit,
  Patreon, Instagram, X / Twitter, Threads.
- [ ] Keep future confidence/avatar/cluster changes behind contract tests
  and conservative merge rules.

## Suggested first implementation slice

- [x] Create the `ProfileEvidence` and `ConfidenceScore` types in
  `adler-core`.
- [x] Populate a small evidence set from existing extract/meta rules
  without changing detection behavior.
- [x] Add confidence calculation for current core outcomes:
  `Found`, `NotFound`, `Uncertain`, including `SessionRequired`.
- [x] Surface confidence in JSON/NDJSON first.
- [x] Add tests around serialization and score reasons.
- [x] Only then add Web/MCP presentation.

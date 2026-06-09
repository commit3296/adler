# Identity Graph roadmap

Long-term checklist for turning Adler from a username availability
scanner into an investigation layer: evidence, confidence, account
correlation, timelines, watchlists, and reports.

This file is intentionally written as a working checklist. Keep completed
platform work visible so future implementation does not lose the context
that already exists.

## Product target

- [ ] Adler records not only whether a profile was found, but why that
  result is trustworthy.
- [ ] Every positive result can expose normalized evidence collected from
  the profile page, API endpoint, registry rule, or authenticated access
  path.
- [ ] Every result can carry a confidence score and human-readable
  reasons.
- [ ] Multiple profile results can be grouped into probable identities
  when they share strong evidence.
- [ ] Historical scans can become timelines, watchlists, and
  investigation reports.
- [ ] CLI, Web, and MCP surfaces can expose the same underlying evidence
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
- [ ] Add a short architectural sketch showing the intended data flow:
  registry -> probe -> outcome -> evidence -> confidence -> identity
  cluster -> timeline/report -> CLI/Web/MCP.
- [ ] Decide which terms become stable public API names:
  `ProfileEvidence`, `ObservedProfile`, `ConfidenceScore`,
  `IdentityCluster`, `InvestigationReport`.
- [ ] Define which fields are experimental and can change before the
  next semver-relevant release.
- [ ] Link follow-up GitHub issues or milestones from this checklist once
  they exist.

## Phase 1: Evidence model

- [ ] Add a normalized evidence model in `adler-core`.
- [ ] Keep evidence collection separate from the low-level site check
  path so `Client::check` does not absorb product-level logic.
- [ ] Model evidence kinds such as:
  username match, display name, profile title, meta description, avatar,
  bio text, external links, location, created/joined date, and endpoint
  source.
- [ ] Include source metadata for each evidence item:
  site, URL, transport/access path, extraction rule, and timestamp.
- [ ] Add serde-compatible structs that can be reused by CLI JSON, Web
  API, persisted scan history, and MCP.
- [ ] Add focused unit tests for evidence serialization and backwards
  compatible defaults.

## Phase 2: Confidence engine

- [ ] Add a rule-based `adler-core` confidence module.
- [ ] Produce a numeric score and a label such as
  `low`, `medium`, `high`, or `verified`.
- [ ] Return machine-readable and human-readable confidence reasons.
- [ ] Account for positive signals:
  exact username match, strong body marker, extracted profile metadata,
  public endpoint match, authenticated endpoint match, and repeated
  historical consistency.
- [ ] Account for negative or weakening signals:
  weak status-only detection, known false-positive-prone site,
  catch-all profile response, blocked transport, missing body marker,
  and disabled/parked limitations.
- [ ] Add tests for representative confidence cases before surfacing the
  score in UI.

## Phase 3: Versioned scan artifact

- [ ] Introduce or extend a versioned persisted scan schema.
- [ ] Store result-level evidence and confidence without breaking older
  scan history.
- [ ] Preserve access telemetry and disabled/session-required states in
  the same artifact.
- [ ] Add migration or tolerant-read behavior for older scan records.
- [ ] Add snapshot tests for serialized scan history.

## Phase 4: CLI, Web, and MCP surfaces

- [ ] Expose evidence and confidence in CLI JSON/NDJSON output.
- [ ] Keep human CLI output compact:
  found/missing/session-required/disabled plus confidence label.
- [ ] Add Web UI confidence badges and a result detail panel for evidence.
- [ ] Add Web API fields without breaking legacy clients that expect the
  current response shape.
- [ ] Return evidence and confidence from MCP scan tools so agents can
  reason over Adler results without scraping presentation text.
- [ ] Update MCP prompts to instruct agents to cite evidence and
  limitations.

## Phase 5: Identity clustering

- [ ] Add `IdentityCluster` and `ClusterReason` models in `adler-core`.
- [ ] Build deterministic correlation rules before considering any ML:
  shared external link, avatar hash, display name, bio phrase, location,
  and historical co-occurrence.
- [ ] Assign cluster confidence separately from per-profile confidence.
- [ ] Make uncertain clusters explicit instead of silently merging weakly
  related accounts.
- [ ] Add Web and MCP output for cluster candidates.
- [ ] Add tests that prevent over-merging based on username-only matches.

## Phase 6: Timeline and watchlists

- [ ] Add watchlist configuration for usernames, aliases, and optional
  site/tag scopes.
- [ ] Support scheduled or repeated scans without coupling scheduling to
  the core engine.
- [ ] Track first seen, last seen, disappeared, reappeared, and changed
  evidence.
- [ ] Show scan-to-scan diffs in Web UI.
- [ ] Expose watchlist/timeline summaries through MCP resources.
- [ ] Keep operator privacy and local-storage expectations explicit.

## Phase 7: Investigation reports

- [ ] Add a report model that consumes scan artifacts, evidence,
  confidence, clusters, and timeline events.
- [ ] Generate Markdown reports first.
- [ ] Include sections for summary, high-confidence accounts, uncertain
  accounts, evidence table, timeline, parked/disabled sites, and known
  limitations.
- [ ] Add JSON report output for downstream tools.
- [ ] Consider HTML export after the report model stabilizes.
- [ ] Add snapshot tests for report rendering.

## Phase 8: Hardening and release readiness

- [ ] Add compatibility tests for public JSON, Web API, and MCP output.
- [ ] Add UI tests for confidence/evidence rendering.
- [ ] Add performance checks for large scan artifacts.
- [ ] Document privacy, retention, and responsible-use considerations.
- [ ] Update `README.md` once the first user-visible slice ships.
- [ ] Update `CHANGELOG.md` only when implementation lands in a release.

## Suggested first implementation slice

- [ ] Create the `ProfileEvidence` and `ConfidenceScore` types in
  `adler-core`.
- [ ] Populate a small evidence set from existing extract/meta rules
  without changing detection behavior.
- [ ] Add confidence calculation for current `Found`, `NotFound`,
  `Uncertain`, `Disabled`, and `SessionRequired` cases.
- [ ] Surface confidence in JSON/NDJSON first.
- [ ] Add tests around serialization and score reasons.
- [ ] Only then add Web/MCP presentation.


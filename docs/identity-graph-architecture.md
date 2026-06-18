# Identity graph architecture

Adler's identity graph work should stay layered. The low-level scanner
decides whether a username appears on a site; higher layers explain why
that result is trustworthy, group related profiles, and render
investigation artifacts without re-implementing scan logic in CLI, Web,
or MCP.

## Data flow

```text
registry
  -> probe
  -> CheckOutcome
  -> ProfileEvidence
  -> ConfidenceScore
  -> IdentityCluster
  -> timeline / InvestigationReport
  -> CLI / Web API / MCP
```

The registry remains the source of site behavior: URL templates,
detection signals, extractors, protection metadata, access constraints,
and disabled-site reasons. Probes apply those rules through the selected
transport and return `CheckOutcome` values. A `CheckOutcome` is the
durable per-site result: verdict, URL, human-readable signal evidence,
normalized profile evidence, confidence, transport/access telemetry, and
timing.

`ProfileEvidence` is the normalized product layer above extractor output
and registry-authored signals. It describes facts observed on a profile
or profile-like endpoint: display names, bios, avatars, external links,
locations, joined dates, profile titles, meta descriptions, and explicit
username-confirmation facts. Optional avatar perceptual hashes are stored
as derived evidence, not as raw image bytes. Evidence is shared by every
surface and should not contain presentation text.

`ObservedProfile` is the normalized aggregate for one found profile in a
scan artifact. It collects the site, URL, username, profile evidence,
confidence, and observation timestamp that identity clustering and
reports need without forcing those layers to parse presentation-oriented
outcome rows.

`ConfidenceScore` is a conservative per-result assessment. It explains
how trustworthy Adler's site-level verdict is; it is not identity proof.
The score may use detection strength, normalized evidence, access path,
historical consistency, and weakening signals such as catch-all pages or
blocked transports.

`IdentityCluster` is the stable account-grouping model. It consumes found
outcomes and evidence, emits explicit reasons, and keeps cluster
confidence separate from per-result confidence. The existing
`CorrelationReport` and `Cluster` types remain useful CLI-era triage
output; new Web, MCP, and report work uses `IdentityCluster` as the
long-term public API.

Timelines and reports are case-level views over persisted scan artifacts.
Timelines describe how profiles change over time. Investigation reports
combine scan summaries, evidence, confidence, clusters, timelines,
disabled/parked limitations, and privacy/retention notes into a
shareable artifact.

CLI, Web API, and MCP are adapters over the same core models. They may
format, filter, or paginate data, but they should not invent separate
confidence, evidence, clustering, or reporting semantics.

## Public names

These names are intended to be stable public API concepts:

- `ProfileEvidence`: normalized observed profile facts.
- `EvidenceSource`: metadata describing where an evidence item came from.
- `ObservedProfile`: normalized found-profile aggregate derived from a
  scan outcome.
- `ConfidenceScore`: explainable per-outcome confidence.
- `ConfidenceReason`: machine-readable confidence rationale.
- `IdentityCluster`: probable group of related profile outcomes.
- `ClusterReason`: machine-readable reason a cluster link exists.
- `InvestigationReport`: case-level report built from scans, evidence,
  confidence, clusters, timelines, and limitations.

Names that exist today but should be treated as transitional:

- `CorrelationReport`: current CLI-oriented correlation summary.
- `Cluster`: current correlation cluster row.

## Stability policy

Stable fields should be additive-first once exposed through CLI JSON,
Web API, persisted scan history, or MCP:

- existing field names should not be renamed without a compatibility
  layer;
- new fields should have serde defaults where old scan artifacts might
  omit them;
- enum additions should be tolerated by downstream presentation layers
  where possible;
- persisted scan readers should keep accepting older artifacts.

Experimental behavior may change before the next semver-relevant
release. It should be documented when introduced, should avoid becoming a
required input for older readers, and should graduate only after CLI,
Web, MCP, and persisted-artifact compatibility tests cover it.

Current experimental areas:

- detailed confidence signal weights and cluster scoring thresholds;
- expansion of avatar perceptual hashing beyond the current opt-in CLI
  fetch path;
- performance limits for very large scan histories.

## Post-v0.15 follow-up

The original identity graph follow-up issues (#75-#79) are complete as of
v0.15.0. Future work should treat the identity graph as shipped
foundation and focus on reliability and coverage around it:

- TikTok detection is restored through its public oEmbed endpoint, with
  deterministic fixtures covering the missing-user and exact-username
  evidence paths;
- use the Registry Reliability v2 roadmap for site health, flaky
  detection triage, and automated registry maintenance:
  [registry-reliability-roadmap.md](registry-reliability-roadmap.md);
- Pinterest and Reddit now have stable evidence-backed paths; continue
  hardening the remaining post-TikTok target set — Patreon, Instagram,
  X / Twitter, and Threads — without weakening privacy, rate-limit, or
  responsible-use constraints;
- keep confidence, avatar hashing, and cluster thresholds conservative
  until new contract tests justify changing them.

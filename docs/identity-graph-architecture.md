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

`ProfileEvidence` is the normalized product layer above extractor output.
It should describe facts observed on a profile or profile-like endpoint:
display names, bios, avatars, external links, locations, joined dates,
profile titles, meta descriptions, and future username-confirmation
facts. Evidence is shared by every surface and should not contain
presentation text.

`ObservedProfile` is the future normalized aggregate for one found
profile in a scan artifact. It should collect the site, URL, username,
verdict context, profile evidence, confidence, and access metadata that
identity clustering and reports need without forcing those layers to
parse presentation-oriented outcome rows.

`ConfidenceScore` is a conservative per-result assessment. It explains
how trustworthy Adler's site-level verdict is; it is not identity proof.
The score may use detection strength, normalized evidence, access path,
historical consistency, and weakening signals such as catch-all pages or
blocked transports.

`IdentityCluster` is the future stable account-grouping model. It should
consume found outcomes and evidence, emit explicit reasons, and keep
cluster confidence separate from per-result confidence. The existing
`CorrelationReport` and `Cluster` types are useful CLI-era triage output;
new Web, MCP, and report work should converge on `IdentityCluster`
instead of treating those older names as the long-term public API.

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

Experimental fields may change before the next semver-relevant release.
They should be documented as experimental when introduced, should avoid
being required by older readers, and should graduate only after CLI, Web,
MCP, and persisted-artifact compatibility tests cover them.

Current experimental areas:

- evidence source transport/access-path metadata;
- explicit username-match evidence;
- detailed confidence signal weights;
- identity-cluster reasons and cluster confidence;
- investigation report JSON schema;
- performance limits for very large scan histories.

## Follow-up issues

- [#75](https://github.com/commit3296/adler/issues/75):
  extend evidence source metadata.
- [#76](https://github.com/commit3296/adler/issues/76):
  refine confidence signal rules.
- [#77](https://github.com/commit3296/adler/issues/77):
  add stable `IdentityCluster` model.
- [#78](https://github.com/commit3296/adler/issues/78):
  generate investigation reports.
- [#79](https://github.com/commit3296/adler/issues/79):
  add compatibility and performance hardening.

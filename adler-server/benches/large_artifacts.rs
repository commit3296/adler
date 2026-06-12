//! Large persisted-scan artifact microbenchmarks.
//!
//! These benches exercise the identity-graph payloads that make scan
//! artifacts heavy: profile evidence, confidence reasons, identity
//! clusters, diffs, and timelines.
//!
//! Run with:
//! `cargo bench -p adler-server --bench large_artifacts`

#![allow(missing_docs)] // criterion macros expand to undocumented items

use std::collections::BTreeMap;
use std::hint::black_box;

use adler_core::{
    CheckOutcome, EvidenceAccessPath, MatchKind, ProfileEvidence, TransportTier, UncertainReason,
};
use adler_server::{FinishedScan, PersistedScan, ScanId, Summary, build_scan_timeline, diff_scans};
use criterion::{Criterion, criterion_group, criterion_main};

const LARGE_OUTCOMES: usize = 2_500;

fn large_outcomes(count: usize, generation: usize) -> Vec<CheckOutcome> {
    (0..count)
        .map(|idx| large_outcome(idx, generation))
        .collect()
}

fn large_outcome(idx: usize, generation: usize) -> CheckOutcome {
    let site = format!("LargeSite{idx:04}");
    let url = format!("https://large{idx:04}.example/alice");
    let mut kind = match idx % 20 {
        0 | 1 => MatchKind::Found,
        3 => MatchKind::Uncertain,
        _ => MatchKind::NotFound,
    };
    if generation > 0 && idx % 20 == 0 {
        kind = MatchKind::NotFound;
    } else if generation > 0 && idx % 20 == 2 {
        kind = MatchKind::Found;
    }

    let mut outcome = CheckOutcome {
        site: site.clone(),
        url: url.clone(),
        kind,
        reason: (kind == MatchKind::Uncertain).then_some(UncertainReason::RateLimited),
        elapsed_ms: 10 + (idx % 75) as u64,
        enrichment: BTreeMap::new(),
        evidence: Vec::new(),
        profile_evidence: Vec::new(),
        confidence: adler_core::ConfidenceScore::default(),
        transport: Some(if idx % 7 == 0 {
            TransportTier::Browser
        } else {
            TransportTier::Http
        }),
        escalations: u8::from(idx % 7 == 0),
    };

    match kind {
        MatchKind::Found => {
            let observed_at_ms = 1_781_192_451_000 + generation as u64 * 1_000 + idx as u64;
            let website = format!("https://identity-{:02}.example", idx % 25);
            let name = format!("Alice Group {:02}", idx % 50);
            let bio = if generation > 0 && idx % 20 == 1 {
                format!("updated profile generation {generation} for {idx}")
            } else {
                format!("stable profile generation 0 for {idx}")
            };
            for (field, value) in [
                ("website", website.as_str()),
                ("name", name.as_str()),
                ("bio", bio.as_str()),
            ] {
                outcome
                    .enrichment
                    .insert(field.to_owned(), value.to_owned());
                outcome
                    .profile_evidence
                    .push(ProfileEvidence::from_enrichment_with_source(
                        &site,
                        &url,
                        field,
                        value,
                        Some(observed_at_ms),
                        Some(EvidenceAccessPath::new(
                            outcome.transport.unwrap_or(TransportTier::Http),
                            outcome.escalations,
                            idx % 11 == 0,
                        )),
                    ));
            }
            outcome.evidence = vec![
                "HTTP 200 (status_found)".to_owned(),
                "body matched profile marker".to_owned(),
            ];
        }
        MatchKind::NotFound => {
            outcome.evidence = vec!["HTTP 404 (status_not_found)".to_owned()];
        }
        MatchKind::Uncertain => {}
    }
    outcome.refresh_confidence();
    outcome
}

fn persisted_scan(scan_id: &str, count: usize, generation: usize) -> PersistedScan {
    let outcomes = large_outcomes(count, generation);
    let finished = FinishedScan {
        summary: Summary::from_outcomes(&outcomes),
        identity_clusters: adler_core::build_identity_clusters("alice", &outcomes),
        elapsed_ms: 30_000 + generation as u64,
        outcomes,
    };
    PersistedScan::from_finished(
        ScanId::from(scan_id.to_owned()),
        "alice".to_owned(),
        count,
        1_781_192_451_000 + generation as u64 * 10_000,
        finished,
    )
}

fn timeline_scans() -> Vec<PersistedScan> {
    (0..12)
        .map(|generation| persisted_scan(&format!("timeline-{generation}"), 600, generation))
        .collect()
}

fn bench_large_artifacts(c: &mut Criterion) {
    let outcomes = large_outcomes(LARGE_OUTCOMES, 0);
    c.bench_function("PersistedScan::from_finished/2500", |b| {
        b.iter(|| {
            let outcomes = black_box(outcomes.clone());
            let finished = FinishedScan {
                summary: Summary::from_outcomes(&outcomes),
                identity_clusters: adler_core::build_identity_clusters("alice", &outcomes),
                elapsed_ms: 30_000,
                outcomes,
            };
            black_box(PersistedScan::from_finished(
                ScanId::from("large".to_owned()),
                "alice".to_owned(),
                LARGE_OUTCOMES,
                1_781_192_451_000,
                finished,
            ));
        });
    });

    let scan = persisted_scan("large", LARGE_OUTCOMES, 0);
    c.bench_function("persisted_scan_json_roundtrip/2500", |b| {
        b.iter(|| {
            let raw = serde_json::to_vec(black_box(&scan)).unwrap();
            let decoded: PersistedScan = serde_json::from_slice(&raw).unwrap();
            black_box(decoded);
        });
    });

    let previous = persisted_scan("large-old", LARGE_OUTCOMES, 0);
    let current = persisted_scan("large-new", LARGE_OUTCOMES, 1);
    c.bench_function("diff_scans/2500", |b| {
        b.iter(|| {
            black_box(diff_scans(black_box(&previous), black_box(&current)));
        });
    });

    let scans = timeline_scans();
    c.bench_function("build_scan_timeline/12x600", |b| {
        b.iter(|| {
            black_box(build_scan_timeline(black_box(&scans)));
        });
    });
}

criterion_group!(benches, bench_large_artifacts);
criterion_main!(benches);

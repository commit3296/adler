//! Cross-account correlation microbenchmark.
//!
//! `correlate()` walks every `Found` outcome and computes pairwise
//! similarity on the enrichment fields (name / bio overlap, avatar
//! URL match, etc.), then unions linked pairs into clusters. The
//! algorithm is O(n²) over found accounts — so a regression hurts
//! disproportionately on `--enrich --correlate` runs over a large
//! popular-target's hit list.
//!
//! Run with: `cargo bench --bench correlate`

#![allow(missing_docs)] // criterion macros expand to undocumented items

use std::collections::BTreeMap;

use adler_core::{CheckOutcome, MatchKind, correlate};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Build N realistic-looking `Found` outcomes with overlapping
/// profile fields. Three identity clusters (alice / bob / charlie)
/// share enrichment values across half the sites — enough overlap
/// that correlate has real work to do (not just a "no overlap"
/// short-circuit), but not so much that everything's in one cluster.
fn make_outcomes(count: usize) -> Vec<CheckOutcome> {
    let identities = [
        ("Alice Liddell", "alice@example.com", "alice.avatar.png"),
        ("Bob Builder", "bob@example.com", "bob.avatar.png"),
        ("Charlie Brown", "charlie@example.com", "charlie.avatar.png"),
    ];
    (0..count)
        .map(|i| {
            let id = identities[i % identities.len()];
            let mut enrichment = BTreeMap::new();
            enrichment.insert("name".into(), id.0.into());
            enrichment.insert("email".into(), id.1.into());
            enrichment.insert("avatar".into(), id.2.into());
            CheckOutcome {
                site: format!("Site{i}"),
                url: format!("https://site{i}.example/u"),
                kind: MatchKind::Found,
                reason: None,
                elapsed_ms: 100,
                enrichment,
                evidence: Vec::new(),
                transport: None,
                escalations: 0,
            }
        })
        .collect()
}

fn bench_correlate(c: &mut Criterion) {
    let mut group = c.benchmark_group("correlate");

    // Sweep through realistic Found-set sizes:
    //   10  — small targeted scan
    //   50  — a popular name on the bundled WMN tranche
    //  200  — registry-wide scan on a well-known handle
    for size in [10_usize, 50, 200] {
        let outcomes = make_outcomes(size);
        group.throughput(criterion::Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &outcomes, |b, outs| {
            b.iter(|| {
                let report = correlate(outs);
                black_box(report);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_correlate);
criterion_main!(benches);

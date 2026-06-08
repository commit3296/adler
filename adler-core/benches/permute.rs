//! Username permutation microbenchmark.
//!
//! `permute()` is a pure-CPU expansion of one username into many
//! variants — leet substitutions, digit suffixes, separator swaps. It
//! runs once per scan (each variant becomes its own scan pass), but a
//! regression here would multiply with `--permute aggressive` runs
//! that fan out to dozens of variants.
//!
//! Run with: `cargo bench --bench permute`

#![allow(missing_docs)] // criterion macros expand to undocumented items

use adler_core::{PermuteLevel, Username, permute};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_permute(c: &mut Criterion) {
    let mut group = c.benchmark_group("permute");

    // Three realistic usernames — short / medium / long — to cover the
    // typical OSINT input distribution. Permutation cost scales
    // with the base length × level.
    let usernames = [
        ("alice", Username::new("alice").unwrap()),
        ("john_doe", Username::new("john_doe").unwrap()),
        (
            "aVeryLongHandleWithMixedCase",
            Username::new("aVeryLongHandleWithMixedCase").unwrap(),
        ),
    ];

    for level in [PermuteLevel::Basic, PermuteLevel::Aggressive] {
        for (label, user) in &usernames {
            let id = format!("{label}/{level:?}");
            group.bench_with_input(
                BenchmarkId::from_parameter(id),
                &(level, user),
                |b, (lvl, u)| {
                    b.iter(|| {
                        let variants = permute(u, *lvl);
                        black_box(variants);
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_permute);
criterion_main!(benches);

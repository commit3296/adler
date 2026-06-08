//! Registry load + filter microbenchmarks.
//!
//! The embedded registry ships ~1900 site entries; `default_embedded()`
//! parses the bundled JSON on first call, and `filter()` runs on every
//! scan to narrow the site set. These are pure-CPU paths so a regression
//! here would compound across every probe — caught here before it ships.
//!
//! Run with: `cargo bench --bench registry`

#![allow(missing_docs)] // criterion macros expand to undocumented items

use adler_core::Registry;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_default_embedded(c: &mut Criterion) {
    c.bench_function("Registry::default_embedded", |b| {
        b.iter(|| {
            // Parse the embedded JSON, validate every site, build the
            // engine merge map. Drop result to force full cost on each iter.
            let r = Registry::default_embedded().expect("embedded registry loads");
            black_box(r);
        });
    });
}

fn bench_filter(c: &mut Criterion) {
    let registry = Registry::default_embedded().expect("embedded registry loads");
    let mut group = c.benchmark_group("Registry::filter");

    // No-op filter: includes every enabled site. Measures the bare
    // walk-and-clone cost without any per-site predicate work.
    group.bench_function("no_filter", |b| {
        b.iter(|| {
            let sites = registry.filter(&[], &[], &[], &[], false);
            black_box(sites);
        });
    });

    // Single-tag include filter, the most common CLI shape
    // (`--tag dev` etc.). Hits every site's tag-comparison path.
    let tag_dev = vec!["dev".to_owned()];
    group.bench_function("tag_dev", |b| {
        b.iter(|| {
            let sites = registry.filter(&[], &[], &tag_dev, &[], false);
            black_box(sites);
        });
    });

    // Combined include + exclude tag filter — exercises the both-set
    // branches and is realistic for `--tag social --exclude-tag bot-protected`.
    let tag_social = vec!["social".to_owned()];
    let exclude_bot = vec!["bot-protected".to_owned()];
    group.bench_function("tag_social_exclude_bot_protected", |b| {
        b.iter(|| {
            let sites = registry.filter(&[], &[], &tag_social, &exclude_bot, false);
            black_box(sites);
        });
    });

    // Name-based include filter — case-insensitive substring match, hot
    // for `--only github,gitlab` style usage.
    let only = vec!["git".to_owned(), "lab".to_owned()];
    group.bench_function("only_substring", |b| {
        b.iter(|| {
            let sites = registry.filter(&only, &[], &[], &[], false);
            black_box(sites);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_default_embedded, bench_filter);
criterion_main!(benches);

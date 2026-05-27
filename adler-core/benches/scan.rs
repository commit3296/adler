//! Microbenchmark for executor throughput against a local wiremock backend.
//!
//! This measures Adler's internal overhead per site, not raw network speed —
//! the wiremock server runs in-process. It exists to catch regressions
//! between Adler commits, not to validate the phase gate "5× faster than
//! Sherlock". That comparison needs real-network runs; see issue #8 for
//! the planned `scripts/bench-vs-sherlock.sh` harness.
//!
//! Run with: `cargo bench --bench scan`

#![allow(missing_docs)] // criterion macros expand to undocumented items

use std::num::NonZeroUsize;
use std::time::Duration;

use adler_core::{Client, ExecutorOptions, Signal, Site, UrlTemplate, Username, executor};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SITE_COUNT: usize = 50;

fn make_sites(server: &MockServer) -> Vec<Site> {
    (0..SITE_COUNT)
        .map(|i| Site {
            name: format!("S{i}"),
            url: UrlTemplate::new(format!("{}/{i}/{{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
        })
        .collect()
}

fn bench_executor(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let server = rt.block_on(async {
        let s = MockServer::start().await;
        for i in 0..SITE_COUNT {
            Mock::given(method("GET"))
                .and(path(format!("/{i}/alice")))
                .respond_with(ResponseTemplate::new(200))
                .mount(&s)
                .await;
        }
        s
    });
    let user = Username::new("alice").unwrap();
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .min_request_interval(Duration::ZERO)
        .build()
        .unwrap();
    let sites = make_sites(&server);

    let mut group = c.benchmark_group("executor::run");
    group.throughput(criterion::Throughput::Elements(SITE_COUNT as u64));
    for concurrency in [4_usize, 16, 64] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &n| {
                let opts = ExecutorOptions::default().concurrency(NonZeroUsize::new(n).unwrap());
                b.to_async(&rt)
                    .iter(|| executor::run(&client, &sites, &user, opts.clone()));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_executor);
criterion_main!(benches);

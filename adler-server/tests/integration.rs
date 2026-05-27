//! End-to-end integration tests for `adler-server`.
//!
//! Unlike the unit tests in `src/api.rs` (which drive the router via
//! `tower::ServiceExt::oneshot`), these tests bind a real TCP listener,
//! spawn the server on a tokio task, and exercise the HTTP API through
//! a real `reqwest` client. That covers the SSE response flow (which
//! oneshot doesn't model well) and the keep-alive / streaming bits
//! axum applies on top of plain handler return values.

use std::collections::BTreeMap;
use std::time::Duration;

use adler_core::{Client as CoreClient, HttpMethod, KnownPresent, Signal, Site, UrlTemplate};
use adler_server::AppState;
use futures::StreamExt;
use reqwest::Client;
use tempfile::TempDir;
use tokio::net::TcpListener;
use wiremock::matchers::{any, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct TestServer {
    base: String,
    _scans_dir: TempDir,
}

fn site(name: &str, base: &str, segment: &str) -> Site {
    Site {
        name: name.into(),
        url: UrlTemplate::new(format!("{base}/{segment}/{{username}}")).unwrap(),
        signals: vec![
            Signal::StatusFound { codes: vec![200] },
            Signal::StatusNotFound { codes: vec![404] },
        ],
        known_present: None::<KnownPresent>,
        known_absent: None,
        extract: Vec::new(),
        tags: Vec::new(),
        request_headers: BTreeMap::new(),
        regex_check: None,
        engine: None,
        strip_bad_char: None,
        request_method: HttpMethod::Get,
        request_body: None,
        protection: Vec::new(),
        disabled: false,
        source: None,
        popularity: None,
    }
}

/// Boot a server with the given sites on a random port and return a
/// handle that owns its scratch persistence directory.
async fn spawn_server(sites: Vec<Site>) -> TestServer {
    let scans_dir = TempDir::new().expect("temp dir");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = CoreClient::builder()
        .timeout(Duration::from_secs(2))
        .min_request_interval(Duration::ZERO)
        .build()
        .expect("client");
    let state = AppState::new(sites, client, 16).with_scans_dir(scans_dir.path().to_path_buf());
    let app = adler_server::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    // Best-effort wait so the listener is ready to accept.
    tokio::time::sleep(Duration::from_millis(20)).await;
    TestServer {
        base: format!("http://{addr}"),
        _scans_dir: scans_dir,
    }
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let server = spawn_server(Vec::new()).await;
    let resp = http()
        .get(format!("{}/api/health", server.base))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn full_scan_flow_via_sse_stream() {
    let mock = MockServer::start().await;
    Mock::given(any())
        .and(path("/a/torvalds"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;
    Mock::given(any())
        .and(path("/b/torvalds"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;
    let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
    let server = spawn_server(sites).await;
    let c = http();

    // Start scan.
    let resp = c
        .post(format!("{}/api/scan", server.base))
        .json(&serde_json::json!({ "username": "torvalds" }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200);
    let started: serde_json::Value = resp.json().await.expect("json");
    let scan_id = started["scan_id"].as_str().unwrap().to_owned();
    assert_eq!(started["site_count"], 2);

    // Consume the SSE stream: expect `start`, two `outcome`s, then `done`.
    let stream_resp = c
        .get(format!("{}/api/scan/{scan_id}/stream", server.base))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("sse send");
    assert_eq!(stream_resp.status(), 200);
    let ctype = stream_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.starts_with("text/event-stream"),
        "got content-type {ctype:?}"
    );

    let mut body = stream_resp.bytes_stream();
    let mut buf = Vec::<u8>::new();
    let mut events: Vec<String> = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .ok()
        .flatten()
    {
        let chunk = chunk.expect("chunk");
        buf.extend_from_slice(&chunk);
        // Parse out "event: <name>" lines.
        let s = String::from_utf8_lossy(&buf);
        events = s
            .lines()
            .filter_map(|l| l.strip_prefix("event: ").map(str::to_owned))
            .collect();
        if events.iter().any(|e| e == "done") {
            break;
        }
    }
    assert!(
        events.contains(&"start".to_owned()),
        "missing start: {events:?}"
    );
    assert!(
        events.iter().filter(|e| *e == "outcome").count() >= 2,
        "expected ≥2 outcome events, got: {events:?}",
    );
    assert!(
        events.contains(&"done".to_owned()),
        "missing done: {events:?}"
    );

    // Final aggregate via GET.
    let snap: serde_json::Value = c
        .get(format!("{}/api/scan/{scan_id}", server.base))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert_eq!(snap["status"], "finished");
    assert_eq!(snap["summary"]["found"], 1);
    assert_eq!(snap["summary"]["not_found"], 1);
    assert_eq!(snap["site_count"], 2);
}

#[tokio::test]
async fn scans_listing_includes_persisted_after_restart() {
    let mock = MockServer::start().await;
    Mock::given(any())
        .and(path("/a/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;
    let sites = vec![site("A", &mock.uri(), "a")];

    // First server: run a scan, let it finish, persist.
    let scans_dir = TempDir::new().unwrap();
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = CoreClient::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state =
            AppState::new(sites.clone(), client, 16).with_scans_dir(scans_dir.path().to_path_buf());
        let app = adler_server::router(state);
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let base = format!("http://{addr}");
        let c = http();
        let r: serde_json::Value = c
            .post(format!("{base}/api/scan"))
            .json(&serde_json::json!({ "username": "alice" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let scan_id = r["scan_id"].as_str().unwrap().to_owned();
        // Wait for finish.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let s: serde_json::Value = c
                .get(format!("{base}/api/scan/{scan_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if s["status"] == "finished" {
                break;
            }
        }
        server_task.abort();
        let _ = server_task.await;
    }

    // Second server: fresh state, same scans_dir. /api/scans should show the prior scan.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let client = CoreClient::builder().build().unwrap();
    let state = AppState::new(sites, client, 16).with_scans_dir(scans_dir.path().to_path_buf());
    let app = adler_server::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let c = http();
    let scans: serde_json::Value = c
        .get(format!("http://{addr}/api/scans"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = scans.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "expected the persisted scan to show up: {scans:#?}"
    );
    assert_eq!(arr[0]["username"], "alice");
    assert_eq!(arr[0]["status"], "finished");
    assert_eq!(arr[0]["summary"]["found"], 1);
}

#[tokio::test]
async fn retry_endpoint_re_probes_and_updates_state() {
    let mock = MockServer::start().await;
    // First call: NotFound. Subsequent: Found.
    Mock::given(any())
        .and(path("/a/alice"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    Mock::given(any())
        .and(path("/a/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;
    let sites = vec![site("A", &mock.uri(), "a")];
    let server = spawn_server(sites).await;
    let c = http();

    let started: serde_json::Value = c
        .post(format!("{}/api/scan", server.base))
        .json(&serde_json::json!({ "username": "alice" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let scan_id = started["scan_id"].as_str().unwrap().to_owned();

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let s: serde_json::Value = c
            .get(format!("{}/api/scan/{scan_id}", server.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if s["status"] == "finished" {
            assert_eq!(s["summary"]["not_found"], 1);
            break;
        }
    }

    let retried: serde_json::Value = c
        .post(format!("{}/api/scan/{scan_id}/retry", server.base))
        .json(&serde_json::json!({ "site": "A" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retried["outcome"]["kind"], "found");

    let after: serde_json::Value = c
        .get(format!("{}/api/scan/{scan_id}", server.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["summary"]["found"], 1);
}

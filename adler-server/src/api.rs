//! Axum router and request handlers.
//!
//! Routes:
//!
//! - `GET  /api/health`           — liveness probe (returns `{ "ok": true }`).
//! - `GET  /api/sites`            — site catalogue available to scans.
//! - `POST /api/scan`             — start a scan; returns a [`ScanId`].
//! - `GET  /api/scan/:id`         — final aggregate (or 404 / 202 in-progress).
//! - `GET  /api/scan/:id/stream`  — Server-Sent Events stream of outcomes.
//!
//! All endpoints emit JSON. Errors carry a stable `{ "error": "<code>",
//! "message": "<human>" }` shape so the `SolidJS` frontend can branch on
//! `error` without parsing free-text.

use axum::Router;
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod dto;
mod error;
mod filter;
mod handlers;

use self::handlers::{
    get_scan, health, list_access, list_scans, list_sites, refilter_scan, retry_site, start_scan,
    stream_scan,
};
use crate::state::AppState;

/// Build the axum router. Public so test harnesses can drive it
/// directly without going through [`crate::serve`].
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/sites", get(list_sites))
        .route("/api/access", get(list_access))
        .route("/api/scans", get(list_scans))
        .route("/api/scan", post(start_scan))
        .route("/api/scan/{id}", get(get_scan))
        .route("/api/scan/{id}/stream", get(stream_scan))
        .route("/api/scan/{id}/retry", post(retry_site))
        .route("/api/scan/{id}/refilter", post(refilter_scan))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use super::dto::StartScanRequest;
    use super::filter::filter_catalog;
    use crate::scan::{ScanHandle, ScanId};
    use adler_core::{Client, KnownPresent, Signal, Site, UrlTemplate};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;
    use wiremock::matchers::{any, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: adler_core::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: adler_core::AccessPolicy::default(),
        }
    }

    async fn test_app() -> (Router, MockServer) {
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        Mock::given(any())
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        (router(state), mock)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn list_sites_returns_summary() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["name"], "A");
        assert!(v[0]["url"].as_str().unwrap().contains("{username}"));
    }

    #[tokio::test]
    async fn list_access_empty_when_nothing_configured() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/access")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["egress"].as_array().unwrap().len(), 0);
        assert_eq!(v["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_access_surfaces_pool_and_sessions_without_secrets() {
        use adler_core::{EgressKind, EgressSpec, Session, SessionStore};
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a")];

        let pool = vec![
            EgressSpec {
                url: "http://corp-proxy.invalid:8080".into(),
                country: adler_core::CountryCode::new("de"),
                kind: EgressKind::Datacenter,
                name: Some("corp-de".into()),
            },
            EgressSpec {
                url: "socks5://user:hunter2@residential.invalid:1080".into(),
                country: adler_core::CountryCode::new("us"),
                kind: EgressKind::Residential,
                name: Some("us-residential".into()),
            },
        ];
        let mut sessions = SessionStore::new();
        let mut hdr = std::collections::BTreeMap::new();
        hdr.insert("Cookie".into(), "sessionid=secret-token-do-not-leak".into());
        sessions.insert("instagram", Session::from_headers(hdr));

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .sessions(sessions)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/access")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let raw = String::from_utf8(body.to_vec()).unwrap();
        // Negative assertions first — a regression here is the whole point
        // of the API design (no URLs, no header values reach the browser).
        assert!(
            !raw.contains("corp-proxy.invalid"),
            "proxy URLs must never leak into /api/access — got body: {raw}"
        );
        assert!(
            !raw.contains("residential.invalid"),
            "proxy URLs must never leak: {raw}"
        );
        assert!(
            !raw.contains("hunter2"),
            "proxy credentials must never leak: {raw}"
        );
        assert!(
            !raw.contains("secret-token-do-not-leak"),
            "session values must never leak: {raw}"
        );

        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let egress = v["egress"].as_array().unwrap();
        assert_eq!(egress.len(), 2);
        assert_eq!(egress[0]["name"], "corp-de");
        assert_eq!(egress[0]["country"], "de");
        assert_eq!(egress[0]["kind"], "datacenter");
        assert_eq!(egress[1]["name"], "us-residential");
        assert_eq!(egress[1]["country"], "us");
        assert_eq!(egress[1]["kind"], "residential");

        let sessions = v["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["name"], "instagram");
    }

    #[tokio::test]
    async fn start_scan_rejects_unknown_egress_name() {
        use adler_core::{EgressKind, EgressSpec};
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a")];
        let pool = vec![EgressSpec {
            url: "http://only-one.invalid:8080".into(),
            country: adler_core::CountryCode::new("de"),
            kind: EgressKind::Datacenter,
            name: Some("only-one".into()),
        }];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .build()
            .unwrap();
        let app = router(AppState::new(sites, client, 16));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","egress_names":["does-not-exist"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unknown_egress");
        assert!(
            v["message"].as_str().unwrap().contains("does-not-exist"),
            "message should name the bad egress, got {}",
            v["message"]
        );
    }

    #[tokio::test]
    async fn start_scan_accepts_known_egress_name() {
        use adler_core::{EgressKind, EgressSpec};
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        let sites = vec![site("A", &mock.uri(), "a")];
        let pool = vec![EgressSpec {
            url: "http://corp-de.invalid:8080".into(),
            country: adler_core::CountryCode::new("de"),
            kind: EgressKind::Datacenter,
            name: Some("corp-de".into()),
        }];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .build()
            .unwrap();
        let app = router(AppState::new(sites, client, 16));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","egress_names":["corp-de"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Known egress name; the scan is accepted (the actual probe
        // outcome happens off-task and is checked elsewhere — this
        // assertion just covers the validation boundary).
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["scan_id"].is_string());
    }

    #[tokio::test]
    async fn start_scan_rejects_invalid_username() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":" bad "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "invalid_username");
    }

    #[tokio::test]
    async fn start_then_poll_finishes_with_expected_counts() {
        let (app, _mock) = test_app().await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();
        assert_eq!(v["site_count"], 2);

        // Poll until finished, max ~5s.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{scan_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            let body = to_bytes(r.into_body(), 16384).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if v["status"] == "finished" {
                assert_eq!(v["summary"]["found"], 1);
                assert_eq!(v["summary"]["not_found"], 1);
                assert_eq!(v["outcomes"].as_array().unwrap().len(), 2);
                return;
            }
        }
        panic!("scan did not finish within 5s");
    }

    #[tokio::test]
    async fn get_scan_404s_on_unknown_id() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "scan_not_found");
    }

    fn tagged_site(name: &str, base: &str, segment: &str, tags: &[&str]) -> Site {
        let mut s = site(name, base, segment);
        s.tags = tags.iter().map(|t| (*t).to_owned()).collect();
        s
    }

    #[test]
    fn filter_catalog_honours_only_exclude() {
        let sites = vec![
            site("GitHub", "http://x", "gh"),
            site("GitLab", "http://x", "gl"),
            site("Bitbucket", "http://x", "bb"),
        ];
        let only = StartScanRequest {
            only: vec!["git".into()],
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &only)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["GitHub", "GitLab"]);

        let exclude = StartScanRequest {
            exclude: vec!["lab".into()],
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &exclude)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["GitHub", "Bitbucket"]);
    }

    #[test]
    fn filter_catalog_honours_tags_and_nsfw() {
        let sites = vec![
            tagged_site("A", "http://x", "a", &["social"]),
            tagged_site("B", "http://x", "b", &["dev"]),
            tagged_site("C", "http://x", "c", &["social", "nsfw"]),
            tagged_site("D", "http://x", "d", &[]),
        ];
        let only_social = StartScanRequest {
            tag: vec!["social".into()],
            ..Default::default()
        };
        // C has `nsfw` so default `nsfw=false` excludes it.
        let names: Vec<_> = filter_catalog(&sites, &only_social)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A"]);

        let with_nsfw = StartScanRequest {
            tag: vec!["social".into()],
            nsfw: true,
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &with_nsfw)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A", "C"]);

        let exclude_dev = StartScanRequest {
            exclude_tag: vec!["dev".into()],
            ..Default::default()
        };
        // dev excluded → A, C (still no nsfw), D remain.
        let names: Vec<_> = filter_catalog(&sites, &exclude_dev)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A", "D"]);
    }

    #[test]
    fn filter_catalog_top_sorts_by_popularity() {
        let mut a = site("A", "http://x", "a");
        a.popularity = Some(3);
        let mut b = site("B", "http://x", "b");
        b.popularity = Some(1);
        let mut c = site("C", "http://x", "c");
        c.popularity = Some(2);
        let d = site("D", "http://x", "d"); // no rank
        let sites = vec![a, b, c, d];
        let req = StartScanRequest {
            top: Some(2),
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &req)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["B", "C"]);
    }

    #[tokio::test]
    async fn start_scan_with_tag_filter_only_runs_matching_sites() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        Mock::given(any())
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let sites = vec![
            tagged_site("A", &mock.uri(), "a", &["social"]),
            tagged_site("B", &mock.uri(), "b", &["dev"]),
        ];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice","tag":["social"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["site_count"], 1);
    }

    #[tokio::test]
    async fn empty_filter_returns_bad_request() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","only":["definitely-not-a-site"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "empty_site_filter");
    }

    #[tokio::test]
    async fn retry_flips_outcome_when_response_changes() {
        // First call returns 404 (one-shot via `up_to_n_times`); second
        // and later calls hit the longer-lived 200 mock that follows it.
        let mock = MockServer::start().await;
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
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);

        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();

        // Wait for completion with NotFound for site A.
        let mut finished = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{scan_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = to_bytes(r.into_body(), 8192).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if v["status"] == "finished" {
                assert_eq!(v["summary"]["not_found"], 1);
                finished = true;
                break;
            }
        }
        assert!(finished, "scan did not finish");

        // Retry — should now hit the 200 mock and flip to found.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{scan_id}/retry"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"A"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["outcome"]["site"], "A");
        assert_eq!(v["outcome"]["kind"], "found");

        // Persistent scan state reflects the new outcome.
        let r = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/scan/{scan_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 16384).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["summary"]["found"], 1);
        assert_eq!(v["summary"]["not_found"], 0);
    }

    #[tokio::test]
    async fn retry_404s_unknown_site_or_scan() {
        let (app, _mock) = test_app().await;
        // Unknown scan.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan/nope/retry")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"A"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);

        // Start a scan, then ask to retry a site that isn't in the catalog.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();
        let r = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{scan_id}/retry"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"NoSuch"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(r.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "site_not_in_catalog");
    }

    #[tokio::test]
    async fn list_scans_returns_newest_first() {
        let (app, _mock) = test_app().await;
        // Kick off two scans.
        for _ in 0..2 {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/scan")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(r#"{"username":"alice"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            // Yield so SystemTime moves forward between insertions.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(
            arr[0]["started_at_ms"].as_u64() >= arr[1]["started_at_ms"].as_u64(),
            "scans must be newest-first",
        );
    }

    #[tokio::test]
    async fn refilter_404s_unknown_scan() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan/does-not-exist/refilter")
                    .header("content-type", "application/json")
                    .body(Body::from(r"{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn refilter_rejects_finished_scan() {
        // Start a scan, wait for it to finish naturally (both sites
        // resolve from the mock instantly), then refilter — must
        // return 400 scan_finished.
        let (app, _mock) = test_app().await;
        let id = start_and_wait(&app, "alice").await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{id}/refilter"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"only":["A"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "scan_finished");
    }

    #[tokio::test]
    async fn refilter_rejects_empty_filter() {
        let (app, _mock) = test_app().await;
        let id = start_and_wait(&app, "alice").await;
        // Even with a finished predecessor, the empty-filter check
        // would fire before scan_finished — but here we get
        // scan_finished first. Use the live router with a custom
        // handle to actually exercise empty_site_filter. We instead
        // construct a fake running scan by inserting a never-ending
        // handle directly into AppState.
        let _ = id;
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let prev_id = ScanId::new();
        let handle = ScanHandle::new("bob", 2, 16);
        state.insert_scan(prev_id.clone(), handle).await;
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{prev_id}/refilter"))
                    .header("content-type", "application/json")
                    // `only=Z` matches no site in the catalog (`A`, `B`).
                    .body(Body::from(r#"{"only":["Z"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "empty_site_filter");
    }

    #[tokio::test]
    async fn refilter_carries_overlap_and_returns_fresh_id() {
        // Synthesize a "running" predecessor whose handle already has
        // an outcome recorded for site A. Refiltering to `only=A`
        // means the new scan should carry A over (1 outcome) and have
        // 0 sites left to probe.
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);

        let prev_id = ScanId::new();
        let handle = ScanHandle::new("bob", 2, 16);
        // Inject a Found outcome for site A so the refilter has
        // something concrete to carry over.
        handle
            .extend_outcomes(vec![adler_core::CheckOutcome {
                site: "A".to_owned(),
                url: "https://a.test/bob".to_owned(),
                kind: adler_core::MatchKind::Found,
                reason: None,
                elapsed_ms: 12,
                evidence: Vec::new(),
                enrichment: std::collections::BTreeMap::new(),
                transport: None,
                escalations: 0,
            }])
            .await;
        state.insert_scan(prev_id.clone(), handle).await;
        let app = router(state.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{prev_id}/refilter"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"only":["A"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["carried_outcomes"], 1);
        assert_eq!(v["site_count"], 1);
        assert_eq!(v["derived_from"].as_str().unwrap(), prev_id.as_str());
        let new_id = v["scan_id"].as_str().unwrap();
        assert_ne!(new_id, prev_id.as_str(), "new scan must have a fresh id");

        // The successor handle should hold the carried-over outcome
        // already, even before the spawn task gets a chance to run.
        let new_handle = state
            .get_scan(&ScanId::from(new_id.to_owned()))
            .await
            .expect("new handle registered");
        let snap = new_handle.outcomes_snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].site, "A");
    }

    /// Test helper: start a scan and wait for it to finish. Returns
    /// the scan id as a string.
    async fn start_and_wait(app: &Router, username: &str) -> String {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username": username}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["scan_id"].as_str().unwrap().to_owned();
        // Poll status until finished (test mocks resolve instantly).
        for _ in 0..50 {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let b = to_bytes(r.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
            if v["status"] == "finished" {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("scan {id} did not finish within ~1s");
    }
}

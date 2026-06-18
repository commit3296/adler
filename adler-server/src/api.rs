//! Axum router and request handlers.
//!
//! Routes:
//!
//! - `GET  /api/health`           — liveness probe (returns `{ "ok": true }`).
//! - `GET  /api/sites`            — site catalogue available to scans.
//! - `GET  /api/scans`            — scan history.
//! - `GET  /api/scans/timeline/:username` — finished-scan timeline.
//! - `GET  /api/scans/:from/diff/:to` — diff two finished scans.
//! - `POST /api/scan`             — start a scan; returns a [`ScanId`].
//! - `GET  /api/scan/:id`         — final aggregate (or 404 / 202 in-progress).
//! - `GET  /api/scan/:id/report`  — derived investigation report.
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
    diff_scans, get_scan, get_scan_report, health, list_access, list_scan_timeline, list_scans,
    list_sites, refilter_scan, retry_site, start_scan, stream_scan,
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
        .route("/api/scans/timeline/{username}", get(list_scan_timeline))
        .route("/api/scans/{from}/diff/{to}", get(diff_scans))
        .route("/api/scan", post(start_scan))
        .route("/api/scan/{id}", get(get_scan))
        .route("/api/scan/{id}/report", get(get_scan_report))
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
    use adler_core::{
        CheckOutcome, Client, KnownPresent, MatchKind, ProfileEvidence, Signal, Site, UrlTemplate,
    };
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

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/alice"),
            kind,
            reason: None,
            elapsed_ms: 10,
            enrichment: std::collections::BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: adler_core::ConfidenceScore::default(),
            transport: None,
            escalations: 0,
        }
    }

    fn found_with_website(site: &str, website: &str) -> CheckOutcome {
        let mut outcome = outcome(site, MatchKind::Found);
        outcome
            .profile_evidence
            .push(ProfileEvidence::from_enrichment(
                site,
                &outcome.url,
                "website",
                website,
            ));
        outcome.refresh_confidence();
        outcome
    }

    fn persisted_scan(
        scan_id: &str,
        created_at_ms: u64,
        outcomes: Vec<CheckOutcome>,
    ) -> crate::persist::PersistedScan {
        crate::persist::PersistedScan::from_finished(
            ScanId::from(scan_id.to_owned()),
            "alice".into(),
            outcomes.len(),
            created_at_ms,
            crate::scan::FinishedScan::from_outcomes("alice", outcomes, 20),
        )
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
        assert_eq!(v["sites"].as_array().unwrap().len(), 2);
        assert_eq!(v["disabled"].as_array().unwrap().len(), 0);
        assert_eq!(v["sites"][0]["name"], "A");
        assert!(
            v["sites"][0]["url"]
                .as_str()
                .unwrap()
                .contains("{username}")
        );
    }

    #[tokio::test]
    async fn list_sites_includes_disabled_catalog_entries() {
        let mock = MockServer::start().await;
        let enabled = site("A", &mock.uri(), "a");
        let mut disabled = site("Threads", &mock.uri(), "threads");
        disabled.disabled = true;
        disabled.disabled_reason = Some("Honest Limits: parked".to_owned());
        let client = Client::builder().build().unwrap();
        let state = AppState::with_catalog(vec![enabled], vec![disabled], client, 16);
        let resp = router(state)
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
        assert_eq!(v["sites"].as_array().unwrap().len(), 1);
        assert_eq!(v["disabled"][0]["name"], "Threads");
        assert_eq!(v["disabled"][0]["disabled_reason"], "Honest Limits: parked");
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
    async fn start_scan_persists_request_context() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let mut enabled = site("A", &mock.uri(), "a");
        enabled.tags = vec!["social".into()];
        enabled.popularity = Some(10);

        let mut disabled = site("A Parked", &mock.uri(), "parked");
        disabled.tags = vec!["social".into()];
        disabled.popularity = Some(20);
        disabled.disabled = true;
        disabled.disabled_reason = Some("Honest Limits: parked for test".into());

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let state = AppState::with_catalog(vec![enabled], vec![disabled], client, 16)
            .with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{
                            "username":"alice",
                            "only":["A"],
                            "tag":["social"],
                            "exclude_tag":["nsfw"],
                            "top":100,
                            "deadline_secs":5
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();

        for _ in 0..50 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{scan_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = to_bytes(resp.into_body(), 16384).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if v["status"] == "finished" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let raw = tokio::fs::read_to_string(tmp.path().join(format!("{scan_id}.json")))
            .await
            .unwrap();
        let persisted: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let ctx = &persisted["request_context"];
        assert_eq!(ctx["username"], "alice");
        assert_eq!(ctx["only"], serde_json::json!(["A"]));
        assert_eq!(ctx["tag"], serde_json::json!(["social"]));
        assert_eq!(ctx["exclude_tag"], serde_json::json!(["nsfw"]));
        assert_eq!(ctx["top"], 100);
        assert_eq!(ctx["deadline_secs"], 5);
        assert_eq!(ctx["disabled_matches"][0]["name"], "A Parked");
        assert_eq!(
            ctx["disabled_matches"][0]["disabled_reason"],
            "Honest Limits: parked for test"
        );
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

    #[test]
    fn filter_catalog_disabled_matches_use_same_filter() {
        let mut disabled = tagged_site("Threads", "http://x", "threads", &["social"]);
        disabled.disabled = true;
        disabled.disabled_reason = Some("Honest Limits".into());
        let sites = vec![site("GitHub", "http://x", "gh"), disabled];
        let req = StartScanRequest {
            only: vec!["threads".into()],
            tag: vec!["social".into()],
            ..Default::default()
        };

        let disabled = super::filter::disabled_matches(&sites, &req);
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].name, "Threads");
    }

    #[tokio::test]
    async fn start_scan_empty_filter_returns_disabled_matches() {
        let mock = MockServer::start().await;
        let mut disabled = site("Threads", &mock.uri(), "threads");
        disabled.disabled = true;
        disabled.disabled_reason = Some("Honest Limits: parked".to_owned());
        let client = Client::builder().build().unwrap();
        let state = AppState::with_catalog(Vec::new(), vec![disabled], client, 16);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice","only":["Threads"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "empty_site_filter");
        assert_eq!(v["message"], "no enabled sites match the requested filter");
        assert_eq!(v["disabled_matches"][0]["name"], "Threads");
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
    async fn diff_scans_returns_persisted_scan_diff() {
        let tmp = tempfile::TempDir::new().unwrap();
        let previous = persisted_scan(
            "old",
            1_000,
            vec![
                outcome("GitHub", MatchKind::Found),
                outcome("Reddit", MatchKind::Found),
                outcome("Mastodon", MatchKind::NotFound),
            ],
        );
        let current = persisted_scan(
            "new",
            2_000,
            vec![
                outcome("GitHub", MatchKind::Found),
                outcome("Reddit", MatchKind::NotFound),
                outcome("Mastodon", MatchKind::Found),
            ],
        );
        crate::persist::save(tmp.path(), &previous).await.unwrap();
        crate::persist::save(tmp.path(), &current).await.unwrap();

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans/old/diff/new")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["from_scan_id"], "old");
        assert_eq!(v["to_scan_id"], "new");
        assert_eq!(v["added_found"][0]["site"], "Mastodon");
        assert_eq!(v["removed_found"][0]["site"], "Reddit");
        assert_eq!(v["verdict_changes"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_scan_from_persisted_dir_applies_historical_confidence_overlay() {
        let tmp = tempfile::TempDir::new().unwrap();
        let older = persisted_scan(
            "older",
            1_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        let previous = persisted_scan(
            "previous",
            2_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        let current = persisted_scan(
            "current",
            3_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        crate::persist::save(tmp.path(), &older).await.unwrap();
        crate::persist::save(tmp.path(), &previous).await.unwrap();
        crate::persist::save(tmp.path(), &current).await.unwrap();

        let raw_before = tokio::fs::read(tmp.path().join("current.json"))
            .await
            .unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/current")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let reasons = v["outcomes"][0]["confidence"]["reasons"]
            .as_array()
            .unwrap();
        assert!(
            reasons.iter().any(|reason| {
                reason["kind"] == "historical_consistency" && reason["count"] == 2
            })
        );
        let raw_after = tokio::fs::read(tmp.path().join("current.json"))
            .await
            .unwrap();
        assert_eq!(raw_before, raw_after);
    }

    #[tokio::test]
    async fn scan_report_returns_json_markdown_and_html_from_persisted_scan() {
        let tmp = tempfile::TempDir::new().unwrap();
        let current = persisted_scan(
            "scan123",
            3_000,
            vec![
                found_with_website("GitHub", "https://alice.dev"),
                found_with_website("GitLab", "https://alice.dev"),
            ],
        );
        crate::persist::save(tmp.path(), &current).await.unwrap();

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let json_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/scan/scan123/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_resp.status(), StatusCode::OK);
        assert_eq!(
            json_resp.headers()[header::CONTENT_TYPE],
            "application/json"
        );
        let json_body = to_bytes(json_resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json_body).unwrap();
        assert_eq!(json["username"], "alice");
        assert_eq!(json["identity_clusters"][0]["id"], "identity-0001");
        assert_eq!(json["evidence_table"].as_array().unwrap().len(), 2);

        let markdown_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/scan/scan123/report?format=markdown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(markdown_resp.status(), StatusCode::OK);
        assert_eq!(
            markdown_resp.headers()[header::CONTENT_TYPE],
            "text/markdown; charset=utf-8"
        );
        let markdown = String::from_utf8(
            to_bytes(markdown_resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(markdown.contains("# Adler investigation report: alice"));
        assert!(markdown.contains("identity-0001"));

        let html_resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/scan123/report?format=html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(html_resp.status(), StatusCode::OK);
        assert_eq!(
            html_resp.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let html = String::from_utf8(
            to_bytes(html_resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("<h2>Identity Clusters</h2>"));
        assert!(!html.contains("<script"));
    }

    #[tokio::test]
    async fn scan_report_supports_finished_in_memory_scan() {
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16);
        let scan_id = ScanId::from("live".to_owned());
        let handle = ScanHandle::new("alice", 1, 16);
        handle
            .publish(crate::scan::FinishedScan::from_outcomes(
                "alice",
                vec![found_with_website("GitHub", "https://alice.dev")],
                20,
            ))
            .await;
        state.insert_scan(scan_id, handle).await;
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/live/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["username"], "alice");
        assert_eq!(json["summary"]["found"], 1);
        assert_eq!(json["found_accounts"][0]["site"], "GitHub");
    }

    #[tokio::test]
    async fn scan_report_rejects_running_unknown_and_invalid_format() {
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16);
        state
            .insert_scan(
                ScanId::from("running".to_owned()),
                ScanHandle::new("alice", 1, 16),
            )
            .await;
        let app = router(state);

        let running = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/scan/running/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(running.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(running.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "scan_not_finished");

        let unknown = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/scan/missing/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(unknown.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "scan_not_found");

        let invalid_format = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/running/report?format=pdf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_format.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(invalid_format.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_report_format");
    }

    #[tokio::test]
    async fn scan_report_derives_clusters_from_legacy_persisted_scan() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outcomes = vec![
            found_with_website("GitHub", "https://alice.dev"),
            found_with_website("GitLab", "https://alice.dev"),
        ];
        let legacy = serde_json::json!({
            "schema_version": 1,
            "scan_id": "legacy",
            "username": "alice",
            "site_count": 2,
            "created_at_ms": 3_000,
            "summary": { "found": 2, "not_found": 0, "uncertain": 0 },
            "outcomes": outcomes,
            "elapsed_ms": 20
        });
        tokio::fs::write(
            tmp.path().join("legacy.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .await
        .unwrap();

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/legacy/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["identity_clusters"][0]["id"], "identity-0001");
        assert_eq!(json["summary"]["identity_clusters"], 1);
    }

    #[tokio::test]
    async fn scan_report_applies_historical_overlay_without_rewriting_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let older = persisted_scan(
            "older",
            1_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        let previous = persisted_scan(
            "previous",
            2_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        let current = persisted_scan(
            "current",
            3_000,
            vec![found_with_website("GitHub", "https://alice.dev")],
        );
        crate::persist::save(tmp.path(), &older).await.unwrap();
        crate::persist::save(tmp.path(), &previous).await.unwrap();
        crate::persist::save(tmp.path(), &current).await.unwrap();

        let raw_before = tokio::fs::read(tmp.path().join("current.json"))
            .await
            .unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/current/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let reasons = json["found_accounts"][0]["confidence"]["reasons"]
            .as_array()
            .unwrap();
        assert!(
            reasons.iter().any(|reason| {
                reason["kind"] == "historical_consistency" && reason["count"] == 2
            })
        );
        let raw_after = tokio::fs::read(tmp.path().join("current.json"))
            .await
            .unwrap();
        assert_eq!(raw_before, raw_after);
    }

    #[tokio::test]
    async fn scan_timeline_returns_persisted_username_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let previous = persisted_scan("old", 1_000, vec![outcome("GitHub", MatchKind::Found)]);
        let current = persisted_scan("new", 2_000, vec![outcome("GitHub", MatchKind::NotFound)]);
        let mut unrelated = persisted_scan("bob", 3_000, vec![outcome("GitHub", MatchKind::Found)]);
        unrelated.username = "bob".into();
        crate::persist::save(tmp.path(), &previous).await.unwrap();
        crate::persist::save(tmp.path(), &current).await.unwrap();
        crate::persist::save(tmp.path(), &unrelated).await.unwrap();

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16).with_scans_dir(tmp.path().to_owned());
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans/timeline/alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["username"], "alice");
        assert_eq!(v["scan_count"], 2);
        assert_eq!(v["from_ms"], 1_000);
        assert_eq!(v["to_ms"], 2_000);
        assert_eq!(v["profiles"][0]["site"], "GitHub");
        assert_eq!(v["profiles"][0]["present_in_latest"], false);
        assert_eq!(v["events"][0]["kind"], "first_seen");
        assert_eq!(v["events"][1]["kind"], "disappeared");
    }

    #[tokio::test]
    async fn scan_timeline_rejects_invalid_username() {
        let (app, _mock) = test_app().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans/timeline/bad%20space")
                    .body(Body::empty())
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
    async fn scan_timeline_returns_requested_username_when_empty() {
        let (app, _mock) = test_app().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans/timeline/absent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["username"], "absent");
        assert_eq!(v["scan_count"], 0);
        assert!(v.get("profiles").is_none());
        assert!(v.get("events").is_none());
    }

    #[tokio::test]
    async fn diff_scans_rejects_running_scan() {
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(Vec::new(), client, 16);
        let running_id = ScanId::from("running".to_owned());
        state
            .insert_scan(running_id.clone(), ScanHandle::new("alice", 0, 16))
            .await;
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/scans/{running_id}/diff/missing"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "scan_not_finished");
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
                profile_evidence: Vec::new(),
                confidence: adler_core::ConfidenceScore::default(),
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

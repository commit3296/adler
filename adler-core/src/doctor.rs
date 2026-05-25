//! Site signature health check.
//!
//! Each [`Site`] declares which detection signals it relies on. Sites in the
//! wild change layouts, redirect to login walls, or start serving 200 for
//! deleted users — and detection rules silently rot. The doctor catches
//! that rot by exercising both polarities for every site:
//!
//! 1. If `known_present` is set, probe each of its candidate
//!    usernames in order; the present-check passes when **any one**
//!    of them resolves to `Found`. Listing more than one is defensive
//!    against sites that special-case specific accounts (e.g.
//!    Instagram's own `instagram` brand account returns a degenerate
//!    JSON shape).
//! 2. Probe with a random nonsense username and expect not-`Found`.
//!    (We don't require `NotFound` strictly: under-specified sites can
//!    legitimately return `Uncertain` for a nonexistent user. Only a
//!    `Found` here is wrong, because it means the rule generalises to
//!    arbitrary strings.)
//!
//! A site fails the check if either assertion is violated.
//!
//! [`suggest_fix`] goes one step further: for a failing site it diffs the
//! responses for the known-present user and a nonsense user and derives a
//! candidate signal set. It only *suggests* — applying changes to the
//! generated registry is the importer's job (see `scripts/import_sherlock.py`
//! `OVERRIDES`), and an auto-applied bad signature would be worse than a
//! flagged one.

use crate::check::{CheckOutcome, MatchKind};
use crate::client::Client;
use crate::error::Result;
use crate::site::{KnownPresent, Signal, Site, UrlTemplate};
use crate::username::Username;

const NONSENSE_LEN: usize = 24;
/// Cap on a body marker derived from a page title — keep suggestions tidy.
const MAX_TITLE_MARKER: usize = 120;

/// Verdict produced by [`check_site`].
#[derive(Debug, Clone)]
pub enum DoctorReport {
    /// All assertions held.
    Healthy {
        /// Outcome for every known-present candidate that was probed,
        /// in declaration order, with the username that produced it.
        /// Empty when the site declares no `known_present`. At least
        /// one of these is guaranteed to be `MatchKind::Found` when
        /// the report is `Healthy`.
        present: Vec<(String, CheckOutcome)>,
        /// Outcome for the random nonsense probe.
        absent: CheckOutcome,
    },
    /// One or more assertions failed; `issues` holds human-readable details.
    Unhealthy {
        /// Each entry is a short reason string.
        issues: Vec<String>,
        /// Outcome for every known-present candidate that was probed,
        /// in declaration order. Empty when the site declares no
        /// `known_present` or when none of the candidates parsed as
        /// valid usernames.
        present: Vec<(String, CheckOutcome)>,
        /// Outcome for the random nonsense probe.
        absent: CheckOutcome,
    },
}

impl DoctorReport {
    /// Convenience: is this report healthy?
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }
}

/// Run health probes against a single site.
pub async fn check_site(client: &Client, site: &Site) -> DoctorReport {
    let mut issues: Vec<String> = Vec::new();
    let mut present_outcomes: Vec<(String, CheckOutcome)> = Vec::new();

    if let Some(kp) = &site.known_present {
        for name in kp.as_slice() {
            match Username::new(name.clone()) {
                Ok(user) => {
                    let outcome = client.check(site, &user).await;
                    present_outcomes.push((name.clone(), outcome));
                }
                Err(err) => {
                    issues.push(format!(
                        "known_present {name:?} is not a valid username: {err}"
                    ));
                }
            }
        }
        // Pass the present-check if *any* candidate yielded Found.
        // Listing several is defensive against sites that special-case
        // specific accounts; only fail when every candidate misbehaves.
        if !present_outcomes.is_empty()
            && !present_outcomes
                .iter()
                .any(|(_, o)| o.kind == MatchKind::Found)
        {
            let summary = present_outcomes
                .iter()
                .map(|(n, o)| format!("{n}={:?}", o.kind))
                .collect::<Vec<_>>()
                .join(", ");
            issues.push(format!(
                "no known-present user yielded Found (tried: {summary})"
            ));
        }
    }

    let nonsense = site
        .known_absent
        .clone()
        .unwrap_or_else(random_nonsense_username);
    let absent_outcome = match Username::new(nonsense.clone()) {
        Ok(user) => client.check(site, &user).await,
        Err(err) => {
            issues.push(format!(
                "could not build absent-probe username {nonsense:?}: {err}",
            ));
            dummy_outcome(&site.name, "skipped: bad absent username")
        }
    };
    if absent_outcome.kind == MatchKind::Found {
        issues.push(format!(
            "nonsense user {nonsense:?} reported Found — detection rule too permissive",
        ));
    }

    if issues.is_empty() {
        DoctorReport::Healthy {
            present: present_outcomes,
            absent: absent_outcome,
        }
    } else {
        DoctorReport::Unhealthy {
            issues,
            present: present_outcomes,
            absent: absent_outcome,
        }
    }
}

fn random_nonsense_username() -> String {
    let mut s = String::with_capacity(NONSENSE_LEN + 7);
    s.push_str("adlerx");
    for _ in 0..NONSENSE_LEN {
        s.push(fastrand::alphanumeric());
    }
    s
}

/// Fixed pool of well-known accounts to probe when discovering a real
/// `known_present` for a site whose Sherlock-imported placeholder
/// (`"blue"`, `"example"`, etc.) doesn't actually exist. Order
/// approximates likelihood of a hit across a long-tail registry:
/// developer-aligned accounts first (since dev sites are common in
/// the imported corpus), then generic admin / brand handles.
///
/// Augmented at runtime with a brand-name candidate derived from the
/// site URL (see [`default_candidate_pool`]).
const DEFAULT_CANDIDATES: &[&str] = &[
    "torvalds", "octocat", "dhh", "tj", "admin", "support", "test",
];

/// Build the default candidate pool for [`discover_known_present`].
///
/// The first entry, when derivable, is the site's brand name — many
/// sites have an official `@<sitename>` account that's a near-
/// guaranteed hit (e.g. `github` on GitHub, `gitlab` on GitLab, `vk`
/// on vk.com). Followed by [`DEFAULT_CANDIDATES`] with duplicates
/// removed.
///
/// Brand derivation is heuristic: parse the URL template (with the
/// placeholder substituted), take the host, drop subdomain and TLD,
/// keep the registrable second-level label. Works cleanly for `.com`
/// / `.net` / `.org`; less so for double-suffix TLDs like `.co.uk`,
/// but the cost of a wrong candidate is just one wasted probe.
#[must_use]
pub fn default_candidate_pool(site: &Site) -> Vec<String> {
    use std::collections::HashSet;

    let mut pool: Vec<String> = Vec::with_capacity(DEFAULT_CANDIDATES.len() + 1);
    let mut seen: HashSet<String> = HashSet::new();
    let push = |pool: &mut Vec<String>, seen: &mut HashSet<String>, name: String| {
        if !name.is_empty() && seen.insert(name.clone()) {
            pool.push(name);
        }
    };
    if let Some(brand) = brand_name_from_site(site) {
        push(&mut pool, &mut seen, brand);
    }
    for name in DEFAULT_CANDIDATES {
        push(&mut pool, &mut seen, (*name).to_owned());
    }
    pool
}

fn brand_name_from_site(site: &Site) -> Option<String> {
    let probe = site.url.as_str().replace("{username}", "_");
    let url = url::Url::parse(&probe).ok()?;
    let host = url.host_str()?;
    let parts: Vec<&str> = host.split('.').collect();
    let label = if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        parts[0]
    };
    if label.is_empty() {
        None
    } else {
        Some(label.to_lowercase())
    }
}

/// Probe `candidates` against `site` and return the first one whose
/// scan resolves to [`MatchKind::Found`].
///
/// Used by `adler --doctor --suggest-known-present` to surface a
/// real-existing account for sites whose imported placeholder doesn't
/// exist on the live site. Stops at the first hit, so the average
/// probe count with the default pool ([`default_candidate_pool`]) is
/// 2–3 per site rather than `pool.len()`.
///
/// Candidates that aren't valid usernames are silently skipped (the
/// pool is small enough that an invalid entry isn't an error
/// condition). Returns `None` when no candidate yields `Found`.
pub async fn discover_known_present(
    client: &Client,
    site: &Site,
    candidates: &[String],
) -> Option<String> {
    for name in candidates {
        let Ok(user) = Username::new(name.clone()) else {
            continue;
        };
        let outcome = client.check(site, &user).await;
        if outcome.kind == MatchKind::Found {
            return Some(name.clone());
        }
    }
    None
}

/// A proposed signal set for a site whose current detection misbehaves.
#[derive(Debug, Clone)]
pub struct FixSuggestion {
    /// Site the suggestion applies to.
    pub site: String,
    /// Proposed replacement signals.
    pub signals: Vec<Signal>,
    /// Human-readable explanation of how the signals were derived.
    pub rationale: String,
}

/// Diff a site's known-present and nonsense responses to derive a candidate
/// signature.
///
/// Returns `None` when no `known_present` is set or the two responses can't
/// be told apart (commonly: a stale `known_present` user that itself no
/// longer exists, so both probes hit a not-found page). Issues two fresh
/// requests; intended for opt-in `--fix` use, not the hot scan path.
///
/// Routes through the [`BrowserBackend`](crate::BrowserBackend) configured
/// on `client` when the site is tagged `bot-protected` (Instagram,
/// Twitter, …), so the diff sees a real JS-rendered profile rather than
/// two identical login-wall shells. Without a backend, falls back to raw
/// HTTP; bot-protected sites will then typically return `None`.
pub async fn suggest_fix(client: &Client, site: &Site) -> Option<FixSuggestion> {
    // Diffing uses the primary (first) known_present candidate. If the
    // site declares several, the others are doctor-only fallbacks; for
    // signal derivation we want a single representative `Found` page.
    let present_name = site.known_present.as_ref()?.primary()?;
    let present_user = Username::new(present_name.to_owned()).ok()?;
    let absent_user = Username::new(random_nonsense_username()).ok()?;

    let present = client
        .fetch_for_doctor(site, &site.url_for(&present_user))
        .await?;
    let absent = client
        .fetch_for_doctor(site, &site.url_for(&absent_user))
        .await?;

    // 1. Distinct status codes are the cleanest discriminator. Require the
    //    present side to be a non-error status so we don't "fix" a site by
    //    treating two error pages as found/not-found.
    if present.status != absent.status && (200..400).contains(&present.status) {
        return Some(FixSuggestion {
            site: site.name.clone(),
            signals: vec![
                Signal::StatusFound {
                    codes: vec![present.status],
                },
                Signal::StatusNotFound {
                    codes: vec![absent.status],
                },
            ],
            rationale: format!(
                "status differs: present={}, absent={}",
                present.status, absent.status
            ),
        });
    }

    // 2. Same status — use a distinct page <title> from the absent page as a
    //    body marker, but only if it doesn't also appear on the present page
    //    (guards against generic site-name titles).
    if let (Some(present_title), Some(absent_title)) =
        (html_title(&present.body), html_title(&absent.body))
    {
        let usable = present_title != absent_title
            && !absent_title.is_empty()
            && !present.body.contains(&absent_title);
        if usable {
            return Some(FixSuggestion {
                site: site.name.clone(),
                signals: vec![
                    Signal::StatusFound {
                        codes: vec![present.status],
                    },
                    Signal::BodyAbsent {
                        text: absent_title.clone(),
                    },
                ],
                rationale: format!(
                    "same status {}, distinct page titles; absent title {absent_title:?} \
                     does not appear on the present page",
                    present.status
                ),
            });
        }
    }

    None
}

/// Scaffold a brand-new site definition from a URL template and a known
/// account.
///
/// Probes `url` (which must contain `{username}` and start with `http(s)://`)
/// with `known_present` and a random nonsense user, diffs the two responses,
/// and returns a complete [`Site`] with a derived signal set plus a
/// human-readable rationale. Returns `Ok(None)` when the two responses are
/// indistinguishable — usually because `known_present` doesn't actually exist
/// on the site, or the site is bot-protected and serves the same page to
/// everyone (try a stable API/feed endpoint, or probe through a clean IP).
///
/// # Errors
///
/// Returns an error if `url` is not a valid URL template.
pub async fn scaffold_site(
    client: &Client,
    name: &str,
    url: &str,
    known_present: &str,
) -> Result<Option<(Site, String)>> {
    let probe = Site {
        name: name.to_owned(),
        url: UrlTemplate::new(url)?,
        // suggest_fix ignores these; it only needs the url + known_present.
        signals: vec![Signal::StatusFound { codes: vec![200] }],
        known_present: Some(KnownPresent::Single(known_present.to_owned())),
        known_absent: None,
        extract: Vec::new(),
        tags: Vec::new(),
        request_headers: std::collections::BTreeMap::new(),
    };
    Ok(suggest_fix(client, &probe).await.map(|fix| {
        (
            Site {
                signals: fix.signals,
                ..probe
            },
            fix.rationale,
        )
    }))
}

/// Extract and trim the first HTML `<title>…</title>` text, capped in length.
fn html_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let gt = lower[open..].find('>')? + open + 1;
    let close = lower[gt..].find("</title>")? + gt;
    let title = body[gt..close].trim();
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(MAX_TITLE_MARKER).collect())
}

fn dummy_outcome(site: &str, note: &str) -> CheckOutcome {
    CheckOutcome {
        site: site.to_owned(),
        url: String::new(),
        kind: MatchKind::Uncertain,
        reason: Some(crate::check::UncertainReason::Other(note.to_owned())),
        elapsed_ms: 0,
        enrichment: std::collections::BTreeMap::new(),
        evidence: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{Signal, UrlTemplate};
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn build_client() -> Client {
        Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .min_request_interval(std::time::Duration::ZERO)
            .max_retries(0)
            .build()
            .unwrap()
    }

    fn site(server: &MockServer, name: &str, known_present: Option<&str>) -> Site {
        Site {
            name: name.into(),
            url: UrlTemplate::new(format!("{}/{{username}}", server.uri())).unwrap(),
            signals: vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::StatusNotFound { codes: vec![404] },
            ],
            known_present: known_present.map(KnownPresent::from),
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn healthy_when_present_returns_200_and_random_returns_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/alice$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // Catch-all for any other path (the random nonsense user).
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let site = site(&server, "Mock", Some("alice"));
        let report = check_site(&build_client(), &site).await;
        assert!(report.is_healthy(), "{report:?}");
    }

    #[tokio::test]
    async fn unhealthy_when_known_present_not_found() {
        let server = MockServer::start().await;
        // Even the "known present" user gets a 404 — broken signature.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let site = site(&server, "Mock", Some("alice"));
        let report = check_site(&build_client(), &site).await;
        match report {
            DoctorReport::Unhealthy { issues, .. } => {
                assert!(
                    issues.iter().any(|i| i.contains("known-present")),
                    "issues: {issues:?}",
                );
            }
            other @ DoctorReport::Healthy { .. } => {
                panic!("expected Unhealthy, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn unhealthy_when_random_user_reports_found() {
        let server = MockServer::start().await;
        // Always 200 — rule is too permissive.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let site = site(&server, "Mock", None);
        let report = check_site(&build_client(), &site).await;
        match report {
            DoctorReport::Unhealthy { issues, .. } => {
                assert!(
                    issues.iter().any(|i| i.contains("too permissive")),
                    "issues: {issues:?}",
                );
            }
            other @ DoctorReport::Healthy { .. } => {
                panic!("expected Unhealthy, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn healthy_when_one_of_several_known_present_yields_found() {
        // Simulates the Instagram-brand case: the first candidate
        // ("instagram") looks degenerate to our signals (random body,
        // 200 status with the wrong markers — modelled here as a 404),
        // but the second one ("torvalds") detects cleanly.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/torvalds$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let mut s = site(&server, "Mock", None);
        s.known_present = Some(KnownPresent::Multiple(vec![
            "instagram".into(),
            "torvalds".into(),
        ]));
        let report = check_site(&build_client(), &s).await;
        assert!(report.is_healthy(), "{report:?}");
        let DoctorReport::Healthy { present, .. } = &report else {
            unreachable!()
        };
        assert_eq!(present.len(), 2);
        assert!(
            present
                .iter()
                .any(|(n, o)| n == "torvalds" && o.kind == MatchKind::Found),
            "expected torvalds=Found in {present:?}"
        );
    }

    #[tokio::test]
    async fn unhealthy_when_no_known_present_candidate_is_found() {
        // All candidates fail the present-check → site reported as
        // broken, and the summary lists each verdict so a contributor
        // can see at a glance which ones rotted.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let mut s = site(&server, "Mock", None);
        s.known_present = Some(KnownPresent::Multiple(vec!["alpha".into(), "beta".into()]));
        let report = check_site(&build_client(), &s).await;
        match report {
            DoctorReport::Unhealthy {
                issues, present, ..
            } => {
                assert_eq!(present.len(), 2, "both candidates should be reported");
                let summary = issues.iter().find(|i| i.contains("known-present"));
                let summary = summary.expect("present-check issue should be raised");
                assert!(summary.contains("alpha"), "issue lacks alpha: {summary}");
                assert!(summary.contains("beta"), "issue lacks beta: {summary}");
            }
            other @ DoctorReport::Healthy { .. } => {
                panic!("expected Unhealthy, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn skips_present_check_when_known_present_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let site = site(&server, "Mock", None);
        let report = check_site(&build_client(), &site).await;
        // No known_present → the only check is "random doesn't yield Found".
        // 404 is fine for that.
        assert!(report.is_healthy(), "{report:?}");
        let DoctorReport::Healthy { present, .. } = &report else {
            unreachable!()
        };
        assert!(present.is_empty());
    }

    #[test]
    fn random_username_passes_validation() {
        let name = random_nonsense_username();
        let result = Username::new(&name);
        assert!(result.is_ok(), "generated {name:?} should pass validation");
        // ASCII alphanumeric guarantee.
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn html_title_extracts_and_trims() {
        assert_eq!(
            html_title("<html><head><TITLE> Hello </TITLE></head>").as_deref(),
            Some("Hello")
        );
        assert_eq!(html_title("<html>no title here</html>"), None);
        assert_eq!(html_title("<title></title>"), None);
    }

    #[tokio::test]
    async fn suggest_fix_derives_status_signals_when_status_differs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/blue$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(410)) // distinct absent status
            .mount(&server)
            .await;
        let s = site(&server, "Mock", Some("blue"));
        let fix = suggest_fix(&build_client(), &s)
            .await
            .expect("a suggestion");
        assert!(fix.rationale.contains("status differs"));
        assert!(matches!(
            fix.signals.as_slice(),
            [
                Signal::StatusFound { codes: f },
                Signal::StatusNotFound { codes: nf },
            ] if f == &[200] && nf == &[410]
        ));
    }

    #[tokio::test]
    async fn suggest_fix_derives_body_marker_from_title() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/blue$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<title>blue · Profile</title>ok"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<title>Page not found</title>"),
            )
            .mount(&server)
            .await;
        let s = site(&server, "Mock", Some("blue"));
        let fix = suggest_fix(&build_client(), &s)
            .await
            .expect("a suggestion");
        assert!(matches!(
            fix.signals.as_slice(),
            [Signal::StatusFound { .. }, Signal::BodyAbsent { text }]
                if text == "Page not found"
        ));
    }

    #[tokio::test]
    async fn suggest_fix_routes_bot_protected_sites_through_browser_backend() {
        // suggest_fix on a raw-HTTP path only sees the login wall both
        // sites return, so without the browser it'd produce no
        // signature. Wiring a backend in should make it see distinct
        // bodies (real profile vs not-found page) and derive a
        // BodyAbsent marker.
        use std::sync::Arc;
        use std::sync::Mutex;

        use serde_json::json;

        use crate::browser::cdp::CdpClient;
        use crate::browser::mock_cdp::{FrameOut, MockCdpServer};
        use crate::browser::{BrowserBackend, BrowserbaseBackend};

        // The mock dispatches Runtime.evaluate based on the URL the
        // most recent Page.navigate carried. Two probe URLs land in
        // sequence (suggest_fix issues present then absent), and the
        // returned body is keyed off the username path segment.
        let last_url: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let last_url_for_handler = Arc::clone(&last_url);
        let server = MockCdpServer::start(move |method, params, _sid| match method {
            "Target.createTarget" => vec![FrameOut::Response(json!({ "targetId": "T1" }))],
            "Target.attachToTarget" => vec![FrameOut::Response(json!({ "sessionId": "S1" }))],
            "Page.navigate" => {
                let url = params
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                *last_url_for_handler.lock().unwrap() = url.clone();
                vec![
                    FrameOut::Response(json!({ "frameId": "F1" })),
                    FrameOut::Event {
                        method: "Network.responseReceived".into(),
                        params: json!({
                            "type": "Document",
                            "response": { "status": 200, "url": url },
                        }),
                        session_id: Some("S1".into()),
                    },
                    FrameOut::Event {
                        method: "Page.frameStoppedLoading".into(),
                        params: json!({ "frameId": "F1" }),
                        session_id: Some("S1".into()),
                    },
                ]
            }
            "Runtime.evaluate" => {
                let url = last_url_for_handler.lock().unwrap().clone();
                let body = if url.contains("/torvalds") {
                    // present probe — real profile page
                    "<html><head><title>torvalds · profile</title></head>\
                     <body>real content</body></html>"
                } else {
                    // absent probe — not-found page
                    "<html><head><title>Profile not found</title></head>\
                     <body>Profile not found</body></html>"
                };
                vec![FrameOut::Response(json!({
                    "result": { "type": "string", "value": body },
                }))]
            }
            _ => vec![FrameOut::Response(json!({}))],
        })
        .await;

        let cdp = CdpClient::connect(&server.ws_url()).await.unwrap();
        let backend: std::sync::Arc<dyn BrowserBackend> =
            std::sync::Arc::new(BrowserbaseBackend::from_parts(cdp, "test-session".into()));

        let http_server = MockServer::start().await;
        let url_template = format!("{}/{{username}}", http_server.uri());
        let s = Site {
            name: "MockBP".into(),
            url: UrlTemplate::new(url_template).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: Some(KnownPresent::Single("torvalds".into())),
            known_absent: None,
            extract: Vec::new(),
            tags: vec!["bot-protected".into()],
            request_headers: std::collections::BTreeMap::new(),
        };

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .min_request_interval(std::time::Duration::ZERO)
            .max_retries(0)
            .browser(backend)
            .build()
            .unwrap();

        let fix = suggest_fix(&client, &s)
            .await
            .expect("suggest_fix should derive a signature from the browser-rendered diff");
        // Same status (200) on both sides → falls into the title /
        // body-marker branch, picking up "Profile not found" as
        // BodyAbsent.
        assert!(
            matches!(
                fix.signals.as_slice(),
                [Signal::StatusFound { codes }, Signal::BodyAbsent { text }]
                    if codes == &[200] && text.contains("not found")
            ),
            "unexpected signals: {:?}",
            fix.signals,
        );
        assert!(
            fix.rationale.contains("titles") || fix.rationale.contains("title"),
            "rationale should mention titles, got: {}",
            fix.rationale,
        );
    }

    #[tokio::test]
    async fn suggest_fix_returns_none_when_indistinguishable() {
        // Both probes get the same status and the same title → stale
        // known_present pattern; nothing to derive.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<title>Same</title>"))
            .mount(&server)
            .await;
        let s = site(&server, "Mock", Some("blue"));
        assert!(suggest_fix(&build_client(), &s).await.is_none());
    }

    #[tokio::test]
    async fn scaffold_site_builds_complete_entry_from_status_diff() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/torvalds$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let url = format!("{}/{{username}}", server.uri());
        let (site, rationale) = scaffold_site(&build_client(), "Mock", &url, "torvalds")
            .await
            .expect("valid url")
            .expect("a derived signature");
        assert_eq!(site.name, "Mock");
        assert_eq!(
            site.known_present.as_ref().and_then(KnownPresent::primary),
            Some("torvalds")
        );
        assert!(rationale.contains("status differs"));
        assert!(matches!(
            site.signals.as_slice(),
            [Signal::StatusFound { codes: f }, Signal::StatusNotFound { codes: nf }]
                if f == &[200] && nf == &[404]
        ));
    }

    #[tokio::test]
    async fn scaffold_site_none_when_indistinguishable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<title>Same</title>"))
            .mount(&server)
            .await;
        let url = format!("{}/{{username}}", server.uri());
        let scaffold = scaffold_site(&build_client(), "Mock", &url, "blue")
            .await
            .expect("valid url");
        assert!(scaffold.is_none());
    }

    #[tokio::test]
    async fn scaffold_site_rejects_bad_url() {
        let err = scaffold_site(&build_client(), "Bad", "not-a-url-no-placeholder", "u").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn suggest_fix_none_without_known_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let s = site(&server, "Mock", None);
        assert!(suggest_fix(&build_client(), &s).await.is_none());
    }

    #[tokio::test]
    async fn discover_returns_first_candidate_that_yields_found() {
        // Only `dhh` exists on this mock — the discovery should walk
        // past the brand-name candidate (mock's host doesn't match)
        // and through `torvalds`, `octocat` (404 → NotFound) before
        // landing on `dhh` (200 → Found).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/dhh$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let s = site(&server, "Mock", None);
        let candidates = vec![
            "torvalds".into(),
            "octocat".into(),
            "dhh".into(),
            "admin".into(),
        ];
        let found = discover_known_present(&build_client(), &s, &candidates).await;
        assert_eq!(found.as_deref(), Some("dhh"));
    }

    #[tokio::test]
    async fn discover_returns_none_when_no_candidate_yields_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let s = site(&server, "Mock", None);
        let candidates = vec!["torvalds".into(), "admin".into()];
        let found = discover_known_present(&build_client(), &s, &candidates).await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn discover_skips_invalid_usernames_silently() {
        // The empty string and one containing forbidden chars must
        // not abort the search — discovery continues to the next
        // candidate, finds `dhh`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/dhh$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let s = site(&server, "Mock", None);
        let candidates = vec![String::new(), "bad user with space".into(), "dhh".into()];
        let found = discover_known_present(&build_client(), &s, &candidates).await;
        assert_eq!(found.as_deref(), Some("dhh"));
    }

    #[test]
    fn default_pool_puts_brand_first_when_derivable() {
        let site = Site {
            name: "GitHub".into(),
            url: UrlTemplate::new("https://www.github.com/{username}").unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
        };
        let pool = default_candidate_pool(&site);
        assert_eq!(pool.first().map(String::as_str), Some("github"));
        // Brand is also among DEFAULT_CANDIDATES adjacent to the
        // contributor names, but it must not appear twice.
        let brand_occurrences = pool.iter().filter(|n| n.as_str() == "github").count();
        assert_eq!(brand_occurrences, 1, "brand should be deduplicated");
        // Sanity: a handful of known defaults follow.
        for expected in ["torvalds", "octocat", "admin"] {
            assert!(
                pool.iter().any(|n| n == expected),
                "pool missing {expected:?}; got {pool:?}"
            );
        }
    }

    #[test]
    fn default_pool_falls_back_to_canned_list_when_brand_underivable() {
        // A URL whose host is a single label (no TLD) makes brand
        // derivation degenerate to the host itself — still produces
        // a non-empty pool, just without a meaningful brand prefix.
        let site = Site {
            name: "Local".into(),
            url: UrlTemplate::new("http://localhost/{username}").unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
        };
        let pool = default_candidate_pool(&site);
        assert!(pool.contains(&"torvalds".to_owned()));
        assert!(pool.contains(&"admin".to_owned()));
    }
}

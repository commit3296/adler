//! Site signature health check.
//!
//! Each [`Site`] declares which detection signals it relies on. Sites in the
//! wild change layouts, redirect to login walls, or start serving 200 for
//! deleted users — and detection rules silently rot. The doctor catches
//! that rot by exercising both polarities for every site:
//!
//! 1. If `known_present` is set, probe with it and expect `Found`.
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
use crate::site::{Signal, Site, UrlTemplate};
use crate::username::Username;

const NONSENSE_LEN: usize = 24;
/// Cap on a body marker derived from a page title — keep suggestions tidy.
const MAX_TITLE_MARKER: usize = 120;

/// Verdict produced by [`check_site`].
#[derive(Debug, Clone)]
pub enum DoctorReport {
    /// All assertions held.
    Healthy {
        /// Optional outcome for the known-present probe (if `known_present` was set).
        present: Option<CheckOutcome>,
        /// Outcome for the random nonsense probe.
        absent: CheckOutcome,
    },
    /// One or more assertions failed; `issues` holds human-readable details.
    Unhealthy {
        /// Each entry is a short reason string.
        issues: Vec<String>,
        /// Optional outcome for the known-present probe.
        present: Option<CheckOutcome>,
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

    let present_outcome = if let Some(name) = &site.known_present {
        let user = match Username::new(name.clone()) {
            Ok(u) => u,
            Err(err) => {
                issues.push(format!("known_present is not a valid username: {err}"));
                return DoctorReport::Unhealthy {
                    issues,
                    present: None,
                    // We didn't get to probe the absent case either.
                    absent: dummy_outcome(&site.name, "skipped: invalid known_present"),
                };
            }
        };
        let outcome = client.check(site, &user).await;
        if outcome.kind != MatchKind::Found {
            issues.push(format!(
                "known-present user {name:?} reported {:?}, expected Found",
                outcome.kind
            ));
        }
        Some(outcome)
    } else {
        None
    };

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
            present: present_outcome,
            absent: absent_outcome,
        }
    } else {
        DoctorReport::Unhealthy {
            issues,
            present: present_outcome,
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
pub async fn suggest_fix(client: &Client, site: &Site) -> Option<FixSuggestion> {
    let present_name = site.known_present.as_ref()?;
    let present_user = Username::new(present_name.clone()).ok()?;
    let absent_user = Username::new(random_nonsense_username()).ok()?;

    let present = client.fetch(&site.url_for(&present_user)).await?;
    let absent = client.fetch(&site.url_for(&absent_user)).await?;

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
        known_present: Some(known_present.to_owned()),
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
            known_present: known_present.map(str::to_owned),
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
        assert!(present.is_none());
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
        assert_eq!(site.known_present.as_deref(), Some("torvalds"));
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
}

//! Site definitions and the multi-signal detection model.
//!
//! A site is a target URL plus a list of [`Signal`]s. Each signal is an
//! independent rule that, when triggered against a response, votes either
//! for the account existing ([`SignalVerdict::Found`]) or not
//! ([`SignalVerdict::NotFound`]). Non-triggering signals stay silent
//! ([`SignalVerdict::Ambiguous`]).
//!
//! Aggregation is **negative-priority**: if any signal votes
//! [`SignalVerdict::NotFound`] the verdict is [`MatchKind::NotFound`];
//! otherwise if any votes [`SignalVerdict::Found`] it is
//! [`MatchKind::Found`]; with no votes at all it is
//! [`MatchKind::Uncertain`].
//!
//! A `NotFound` vote wins over a `Found` vote because negative signals are
//! specific (an exact "user not found" message, a 404, a login redirect)
//! while a bare `200 OK` is weak positive evidence. This matches how
//! Sherlock-style detectors work: a site that always returns 200 and only
//! differentiates via an error string is correctly read as `NotFound` when
//! that string is present, even though the 200 also satisfies a
//! `StatusFound` signal.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::check::MatchKind;
use crate::error::{Error, Result};
use crate::username::Username;

/// One site we can probe for the existence of an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    /// Human-readable site name. Doubles as the stable filter key
    /// (case-insensitive) used by CLI `--only` / `--exclude`.
    pub name: String,
    /// URL template containing a `{username}` placeholder.
    pub url: UrlTemplate,
    /// Ordered list of detection signals. Aggregated per the type-level docs.
    pub signals: Vec<Signal>,
    /// One or more usernames known to exist on this site. Consumed by
    /// `adler doctor` to verify the signal list still reports `Found`
    /// for a real account. Accepts either a single string or an array
    /// of strings in JSON; the doctor probes each in declaration order
    /// and passes the present-check if **any** one of them resolves to
    /// `Found`. Listing several is defensive — brand accounts or other
    /// users that the site special-cases (e.g. Instagram's own
    /// `instagram` account) shouldn't false-fail the whole site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_present: Option<KnownPresent>,
    /// Username known to *not* exist on this site (optional). When omitted,
    /// the doctor generates a random nonsense username instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_absent: Option<String>,
    /// Optional CSS-selector rules for pulling profile fields (name, bio,
    /// avatar, …) out of a `Found` page. Only applied under `--enrich`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extract: Vec<Extractor>,
    /// Free-form classification tags for scanning a subset of the registry,
    /// e.g. `"social"`, `"dev"`, `"region:ru"`. Matched by CLI `--tag`.
    /// A site with no tags is universal (included unless a `--tag` filter
    /// excludes it). Conventionally lowercase; `axis:value` is just a naming
    /// convention, not enforced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Extra HTTP headers to send with the probe (e.g.
    /// `{"X-IG-App-ID": "936619743392459"}` to unlock Instagram's
    /// `web_profile_info` endpoint, or a custom `User-Agent`). Browser
    /// backends apply them via `Network.setExtraHTTPHeaders` before
    /// navigation; the raw-HTTP path doesn't read this yet.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub request_headers: std::collections::BTreeMap<String, String>,
}

/// Known-present declaration on a [`Site`].
///
/// In JSON this is `untagged`: a plain string `"torvalds"` deserialises
/// into [`KnownPresent::Single`], an array `["torvalds", "leomessi"]`
/// into [`KnownPresent::Multiple`]. Serialisation preserves the form
/// the site was authored with, so single-username entries stay
/// compact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum KnownPresent {
    /// Exactly one candidate username.
    Single(String),
    /// Two or more candidate usernames. Doctor passes if any resolve
    /// to `Found`.
    Multiple(Vec<String>),
}

impl KnownPresent {
    /// View all candidate usernames as a slice, in declaration order.
    /// Always non-empty for `Single`; may be empty for a hand-authored
    /// `Multiple([])` (validation rejects that).
    pub fn as_slice(&self) -> &[String] {
        match self {
            Self::Single(s) => std::slice::from_ref(s),
            Self::Multiple(v) => v.as_slice(),
        }
    }

    /// Primary candidate — the first declared username. `Single`
    /// always has one; `Multiple` may be empty if a contributor wrote
    /// `[]` (caught by [`Site::validate`]).
    pub fn primary(&self) -> Option<&str> {
        self.as_slice().first().map(String::as_str)
    }
}

impl From<&str> for KnownPresent {
    fn from(s: &str) -> Self {
        Self::Single(s.to_owned())
    }
}

impl From<String> for KnownPresent {
    fn from(s: String) -> Self {
        Self::Single(s)
    }
}

/// A rule for extracting one profile field from a page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extractor {
    /// Output field name, e.g. `"avatar"`, `"bio"`, `"name"`.
    pub field: String,
    /// CSS selector locating the element.
    pub selector: String,
    /// Attribute to read (e.g. `"src"`, `"content"`). When omitted, the
    /// element's trimmed text content is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
}

impl Site {
    /// Render the site URL for a given username.
    pub fn url_for(&self, username: &Username) -> String {
        self.url.substitute(username.as_str())
    }

    /// Validate semantic invariants the type system can't enforce
    /// (empty signals list, empty markers, empty status code sets).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::InvalidSite {
                reason: "site name is empty".into(),
            });
        }
        if self.signals.is_empty() {
            return Err(Error::InvalidSite {
                reason: format!("site {:?}: signals list is empty", self.name),
            });
        }
        for signal in &self.signals {
            signal.validate().map_err(|reason| Error::InvalidSite {
                reason: format!("site {:?}: {reason}", self.name),
            })?;
        }
        for extractor in &self.extract {
            if extractor.field.trim().is_empty() {
                return Err(Error::InvalidSite {
                    reason: format!("site {:?}: extractor has an empty field name", self.name),
                });
            }
            if scraper::Selector::parse(&extractor.selector).is_err() {
                return Err(Error::InvalidSite {
                    reason: format!(
                        "site {:?}: invalid CSS selector {:?} for field {:?}",
                        self.name, extractor.selector, extractor.field
                    ),
                });
            }
        }
        if let Some(kp) = &self.known_present {
            if kp.as_slice().is_empty() {
                return Err(Error::InvalidSite {
                    reason: format!("site {:?}: known_present is an empty list", self.name),
                });
            }
            for name in kp.as_slice() {
                if name.trim().is_empty() {
                    return Err(Error::InvalidSite {
                        reason: format!(
                            "site {:?}: known_present contains an empty username",
                            self.name
                        ),
                    });
                }
            }
        }
        for tag in &self.tags {
            if tag.trim().is_empty() {
                return Err(Error::InvalidSite {
                    reason: format!("site {:?}: tag is empty", self.name),
                });
            }
        }
        Ok(())
    }
}

/// URL template containing a `{username}` placeholder.
///
/// Validated at construction: must contain the placeholder and start with
/// `http://` or `https://`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlTemplate(String);

const PLACEHOLDER: &str = "{username}";

impl UrlTemplate {
    /// Build a template, validating placeholder and scheme.
    pub fn new(template: impl Into<String>) -> Result<Self> {
        let t = template.into();
        if !t.contains(PLACEHOLDER) {
            return Err(Error::InvalidSite {
                reason: format!("url template missing {PLACEHOLDER} placeholder: {t:?}"),
            });
        }
        if !(t.starts_with("http://") || t.starts_with("https://")) {
            return Err(Error::InvalidSite {
                reason: format!("url template must start with http(s)://: {t:?}"),
            });
        }
        Ok(Self(t))
    }

    fn substitute(&self, username: &str) -> String {
        self.0.replace(PLACEHOLDER, username)
    }

    /// Borrow the raw template (with placeholder).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UrlTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for UrlTemplate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for UrlTemplate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// A single piece of evidence about whether an account exists.
///
/// Signals are tagged in JSON by their `kind`. New variants will land for
/// Phase 2 length-baseline scoring; the enum is `#[non_exhaustive]` so
/// adding variants is not a breaking change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Signal {
    /// Votes **`Found`** when the response status is in `codes`.
    StatusFound {
        /// Status codes that vote for existence. Must be non-empty.
        codes: Vec<u16>,
    },
    /// Votes **`NotFound`** when the response status is in `codes`.
    StatusNotFound {
        /// Status codes that vote for non-existence. Must be non-empty.
        codes: Vec<u16>,
    },
    /// Votes **`Found`** when the response body contains `text`.
    BodyPresent {
        /// Substring whose appearance votes for existence. Must be non-empty.
        text: String,
    },
    /// Votes **`NotFound`** when the response body contains `text`.
    BodyAbsent {
        /// Substring whose appearance votes for non-existence (e.g.
        /// `"Profile not found"`). Must be non-empty.
        text: String,
    },
    /// Votes **`NotFound`** when the final URL (post-redirect) contains
    /// `fragment`.
    RedirectAbsent {
        /// Substring that, when present in the final URL, indicates the
        /// account is missing (typically `"/login"` or `"/404"`). Must be
        /// non-empty.
        fragment: String,
    },
}

/// Probe data extracted from an HTTP response, fed to each [`Signal`].
///
/// Internal detection plumbing — not part of the public API.
#[derive(Debug)]
pub(crate) struct Probe<'a> {
    /// HTTP status code.
    pub(crate) status: u16,
    /// Final URL after redirects.
    pub(crate) final_url: &'a str,
    /// Decoded response body. Empty string when no body-using signal is configured.
    pub(crate) body: &'a str,
}

/// What one signal concluded after looking at a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalVerdict {
    /// This signal votes that the account exists.
    Found,
    /// This signal votes that the account does not exist.
    NotFound,
    /// This signal had nothing to say (its trigger condition didn't match).
    Ambiguous,
}

impl Signal {
    /// True if this signal needs to inspect the response body. Used by the
    /// client to skip body reads when no signal requires them.
    pub(crate) fn needs_body(&self) -> bool {
        matches!(self, Self::BodyPresent { .. } | Self::BodyAbsent { .. })
    }

    /// Evaluate this signal against a probe and produce a vote.
    pub(crate) fn evaluate(&self, probe: &Probe<'_>) -> SignalVerdict {
        match self {
            Self::StatusFound { codes } => {
                if codes.contains(&probe.status) {
                    SignalVerdict::Found
                } else {
                    SignalVerdict::Ambiguous
                }
            }
            Self::StatusNotFound { codes } => {
                if codes.contains(&probe.status) {
                    SignalVerdict::NotFound
                } else {
                    SignalVerdict::Ambiguous
                }
            }
            Self::BodyPresent { text } => {
                if probe.body.contains(text.as_str()) {
                    SignalVerdict::Found
                } else {
                    SignalVerdict::Ambiguous
                }
            }
            Self::BodyAbsent { text } => {
                if probe.body.contains(text.as_str()) {
                    SignalVerdict::NotFound
                } else {
                    SignalVerdict::Ambiguous
                }
            }
            Self::RedirectAbsent { fragment } => {
                if probe.final_url.contains(fragment.as_str()) {
                    SignalVerdict::NotFound
                } else {
                    SignalVerdict::Ambiguous
                }
            }
        }
    }

    /// Human-readable description of why this signal fired against `probe`,
    /// for verdict explainability. Only meaningful for a signal that voted
    /// (i.e. didn't return [`SignalVerdict::Ambiguous`]); the caller filters.
    pub(crate) fn describe_match(&self, probe: &Probe<'_>) -> String {
        match self {
            Self::StatusFound { .. } => format!("HTTP {} (status_found)", probe.status),
            Self::StatusNotFound { .. } => format!("HTTP {} (status_not_found)", probe.status),
            Self::BodyPresent { text } => format!("body contains {text:?} (body_present)"),
            Self::BodyAbsent { text } => format!("body contains {text:?} (body_absent)"),
            Self::RedirectAbsent { fragment } => {
                format!("final URL contains {fragment:?} (redirect_absent)")
            }
        }
    }

    fn validate(&self) -> std::result::Result<(), String> {
        match self {
            Self::StatusFound { codes } | Self::StatusNotFound { codes } => {
                if codes.is_empty() {
                    return Err("status signal codes list is empty".into());
                }
            }
            Self::BodyPresent { text } | Self::BodyAbsent { text } => {
                if text.is_empty() {
                    return Err("body signal text is empty".into());
                }
            }
            Self::RedirectAbsent { fragment } => {
                if fragment.is_empty() {
                    return Err("redirect signal fragment is empty".into());
                }
            }
        }
        Ok(())
    }
}

/// Aggregate per-signal verdicts into a final [`MatchKind`].
///
/// Negative-priority counting: any `NotFound` vote → `NotFound`; otherwise
/// any `Found` vote → `Found`; no votes at all → `Uncertain`. See the module
/// docs for why a `NotFound` vote outranks a `Found` vote.
pub(crate) fn aggregate<I>(verdicts: I) -> MatchKind
where
    I: IntoIterator<Item = SignalVerdict>,
{
    let mut found = false;
    let mut not_found = false;
    for v in verdicts {
        match v {
            SignalVerdict::Found => found = true,
            SignalVerdict::NotFound => not_found = true,
            SignalVerdict::Ambiguous => {}
        }
    }
    if not_found {
        MatchKind::NotFound
    } else if found {
        MatchKind::Found
    } else {
        MatchKind::Uncertain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site_with(signals: Vec<Signal>) -> Site {
        Site {
            name: "Example".into(),
            url: UrlTemplate::new("https://example.com/{username}").unwrap(),
            signals,
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn url_template_substitutes_placeholder() {
        let user = Username::new("alice").unwrap();
        let site = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        assert_eq!(site.url_for(&user), "https://example.com/alice");
    }

    #[test]
    fn url_template_rejects_missing_placeholder() {
        assert!(UrlTemplate::new("https://example.com/users/").is_err());
    }

    #[test]
    fn url_template_rejects_bad_scheme() {
        assert!(UrlTemplate::new("ftp://example.com/{username}").is_err());
    }

    #[test]
    fn validate_requires_non_empty_signals() {
        let err = site_with(vec![]).validate().unwrap_err();
        assert!(err.to_string().contains("signals list is empty"));
    }

    #[test]
    fn validate_rejects_empty_status_codes() {
        let err = site_with(vec![Signal::StatusFound { codes: vec![] }])
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("status signal"));
    }

    #[test]
    fn validate_rejects_empty_body_text() {
        let err = site_with(vec![Signal::BodyAbsent {
            text: String::new(),
        }])
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("body signal"));
    }

    #[test]
    fn validate_rejects_empty_redirect_fragment() {
        let err = site_with(vec![Signal::RedirectAbsent {
            fragment: String::new(),
        }])
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("redirect signal"));
    }

    #[test]
    fn signal_status_found_votes_only_on_match() {
        let signal = Signal::StatusFound { codes: vec![200] };
        let probe = Probe {
            status: 200,
            final_url: "https://example.com/alice",
            body: "",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Found);
        let probe = Probe {
            status: 404,
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn signal_status_not_found_votes_only_on_match() {
        let signal = Signal::StatusNotFound { codes: vec![404] };
        let probe = Probe {
            status: 404,
            final_url: "",
            body: "",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::NotFound);
        let probe = Probe {
            status: 200,
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn signal_body_absent_votes_not_found_when_text_present() {
        let signal = Signal::BodyAbsent {
            text: "Profile not found".into(),
        };
        let probe = Probe {
            status: 200,
            final_url: "",
            body: "<h1>Profile not found</h1>",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::NotFound);
        let probe = Probe {
            body: "<h1>Welcome alice</h1>",
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn signal_redirect_absent_inspects_final_url() {
        let signal = Signal::RedirectAbsent {
            fragment: "/login".into(),
        };
        let probe = Probe {
            status: 200,
            final_url: "https://example.com/login?next=/alice",
            body: "",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::NotFound);
        let probe = Probe {
            final_url: "https://example.com/alice",
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn aggregate_found_when_only_found_signals_fire() {
        let kind = aggregate([SignalVerdict::Found, SignalVerdict::Ambiguous]);
        assert_eq!(kind, MatchKind::Found);
    }

    #[test]
    fn aggregate_not_found_when_only_not_found_signals_fire() {
        let kind = aggregate([SignalVerdict::NotFound, SignalVerdict::Ambiguous]);
        assert_eq!(kind, MatchKind::NotFound);
    }

    #[test]
    fn aggregate_not_found_wins_over_found() {
        // Negative-priority: a NotFound vote outranks a Found vote.
        let kind = aggregate([SignalVerdict::Found, SignalVerdict::NotFound]);
        assert_eq!(kind, MatchKind::NotFound);
    }

    #[test]
    fn aggregate_uncertain_when_no_signals_fire() {
        let kind = aggregate([SignalVerdict::Ambiguous, SignalVerdict::Ambiguous]);
        assert_eq!(kind, MatchKind::Uncertain);
    }

    #[test]
    fn aggregate_empty_is_uncertain() {
        let kind = aggregate(std::iter::empty());
        assert_eq!(kind, MatchKind::Uncertain);
    }

    #[test]
    fn needs_body_is_true_only_for_body_signals() {
        assert!(!Signal::StatusFound { codes: vec![200] }.needs_body());
        assert!(!Signal::StatusNotFound { codes: vec![404] }.needs_body());
        assert!(
            !Signal::RedirectAbsent {
                fragment: "/login".into()
            }
            .needs_body()
        );
        assert!(Signal::BodyPresent { text: "x".into() }.needs_body());
        assert!(Signal::BodyAbsent { text: "x".into() }.needs_body());
    }

    #[test]
    fn deserializes_signal_list() {
        let json = r#"{
            "name": "GitHub",
            "url": "https://github.com/{username}",
            "signals": [
                { "kind": "status_found", "codes": [200] },
                { "kind": "status_not_found", "codes": [404] }
            ]
        }"#;
        let site: Site = serde_json::from_str(json).unwrap();
        assert_eq!(site.name, "GitHub");
        assert_eq!(site.signals.len(), 2);
        site.validate().unwrap();
    }

    proptest::proptest! {
        /// For any mix of per-signal verdicts, aggregation obeys the
        /// negative-priority spec: any NotFound wins; else any Found; else
        /// Uncertain.
        #[test]
        fn aggregate_matches_negative_priority_spec(
            votes in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::strategy::Just(SignalVerdict::Found),
                    proptest::strategy::Just(SignalVerdict::NotFound),
                    proptest::strategy::Just(SignalVerdict::Ambiguous),
                ],
                0..16,
            ),
        ) {
            let kind = aggregate(votes.iter().copied());
            let expected = if votes.contains(&SignalVerdict::NotFound) {
                MatchKind::NotFound
            } else if votes.contains(&SignalVerdict::Found) {
                MatchKind::Found
            } else {
                MatchKind::Uncertain
            };
            proptest::prop_assert_eq!(kind, expected);
        }
    }
}

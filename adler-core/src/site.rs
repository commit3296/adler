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

use crate::access::AccessPolicy;
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
    /// Optional in source JSON when [`Site::engine`] is set — the engine's
    /// signals are inherited at load time. After
    /// [`crate::Registry`] resolution this vec is always non-empty (or the
    /// site fails `validate`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// Optional regular expression describing usernames a site will
    /// accept. When set and the scanned username doesn't match, the
    /// site is skipped (the outcome is reported as `Uncertain` with
    /// reason `UsernameNotAllowed`, without issuing any HTTP request).
    /// Saves work AND avoids the false-positive class where a site
    /// 404s on illegal usernames in ways our signal can't tell apart
    /// from a missing account.
    ///
    /// Imported from Sherlock's `regexCheck` field; 95+ sites
    /// upstream carry one (length bounds, character classes, etc.).
    /// Validation at load time compiles the regex with `regex::Regex`
    /// — a malformed pattern rejects the site rather than silently
    /// degrading at scan time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex_check: Option<String>,
    /// Name of a shared [`Engine`] this site inherits from (e.g.
    /// `"Discourse"`, `"vBulletin"`). Forum-software platforms host
    /// thousands of instances with identical detection signatures;
    /// defining the signature once on an engine and inheriting it
    /// keeps the registry small and the cost of a platform-wide
    /// HTML change one fix instead of hundreds.
    ///
    /// At registry-load time the engine fields are merged *under* the
    /// site's own — anything the site declares explicitly (`signals`,
    /// `request_headers`, `regex_check`) wins on
    /// conflict; anything left empty / unset is filled from the
    /// engine. An `engine: "X"` referring to a non-existent X is a
    /// load-time error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Characters the site silently drops from the username server-side
    /// before matching — `john.doe` and `johndoe` resolve to the same
    /// account on a site that lists `strip_bad_char: "."`. We pre-strip
    /// at probe time so the URL we issue matches the canonical form
    /// the site uses, avoiding a false `NotFound` on a benign
    /// punctuation variant. Mirrors `WhatsMyName`'s field of the same
    /// name; carried verbatim through `scripts/import_whatsmyname.py`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_bad_char: Option<String>,
    /// HTTP method used to probe this site. Defaults to GET — the vast
    /// majority of sites are GET-probed. A few (Anilist's GraphQL API,
    /// some Discord/Holopin endpoints) only answer to POST.
    #[serde(default, skip_serializing_if = "is_default_method")]
    pub request_method: HttpMethod,
    /// Request body to send when [`Site::request_method`] is POST. The
    /// literal `{username}` placeholder is substituted with the probe
    /// username (same as URL templates). For GraphQL endpoints this
    /// is typically the JSON `{"query":"...","variables":{"name":"{username}"}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// Specific anti-bot mechanisms the site is known to deploy. A
    /// richer alternative to the flat `bot-protected` tag — knowing
    /// *which* protection a site uses lets future routing pick the
    /// right backend (`Cloudflare` → cloudscraper-style bypass,
    /// `CfFirewall` → full browser, `UserAuth` → skip, …) instead
    /// of the all-or-nothing `bot-protected` decision.
    ///
    /// Independent of [`Site::tags`]: the existing `bot-protected`
    /// tag stays as a back-compat shorthand and routes through the
    /// browser backend exactly as before. When this vector is
    /// non-empty Adler also treats the site as bot-protected
    /// regardless of the tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protection: Vec<ProtectionKind>,
    /// Disable the site without removing it from the registry.
    /// Disabled sites are skipped by [`crate::Registry::filter`] —
    /// they don't get probed, don't appear in `--list-sites`, and
    /// don't count toward the doctor's tally. Useful for parking
    /// known-broken entries with a reason comment instead of
    /// deleting them outright, so a future contributor can re-enable
    /// the entry by flipping the flag once they've authored a
    /// working signature.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Free-form annotation explaining why a [`Site::disabled`] entry
    /// was parked. The Rust runtime doesn't act on it — the JSON
    /// loader, scan path and doctor all just look at `disabled` — but
    /// downstream tooling (`scripts/doctor_aggregate.py`, ad-hoc
    /// audits) and human maintainers reading `sites.json` directly
    /// rely on it to tell categories apart at-a-glance:
    /// `duplicate of <canonical>`, `Honest Limits: …`, `doctor: 3+
    /// consecutive structural failures`, etc. Optional; only meaningful
    /// when `disabled` is also `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Canonical-source link for mirror-style sites. When a site is
    /// a mirror of another (e.g. Nitter ↔ Twitter, Invidious ↔
    /// `YouTube`), `source` carries the name of the primary site this
    /// one mirrors. Lets future UX surface "Twitter is offline,
    /// here's the same account on Nitter" without hand-curated
    /// linkage. Empty / `None` for canonical sites and sites with
    /// no known mirror relationship.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Approximate popularity rank — lower numbers are more popular.
    /// Used by `adler --top N` as a rank ceiling (`popularity <= N`),
    /// useful for fast checks of high-signal targets. Ranks are curated,
    /// not derived from traffic data: the seed set covers well-known
    /// OSINT-relevant sites where most users have accounts. Sites
    /// without a rank are skipped by `--top N`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popularity: Option<u32>,
    /// Egress requirement for reaching this site — country and/or IP
    /// type the probe must exit from (see [`AccessPolicy`]). Default
    /// (empty) means no special routing: the request uses the client's
    /// default egress. When constrained and no configured egress fits,
    /// the probe is reported `Uncertain(GeoUnavailable)` rather than
    /// fetched from the wrong location.
    #[serde(default, skip_serializing_if = "AccessPolicy::is_default")]
    pub access: AccessPolicy,
}

/// A specific anti-bot mechanism a site is known to deploy. Used to
/// route probes to the right backend (raw HTTP, cloudscraper, full
/// browser) and to inform users what blocks reliable detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ProtectionKind {
    /// Standard Cloudflare WAF — challenge pages, `cf_clearance`
    /// cookie. Bypassable by cloudscraper-style HTTP-level solvers
    /// (e.g. `FlareSolverr`) without a full browser.
    Cloudflare,
    /// AWS `CloudFront` edge protection. Often UA-strictness only.
    Cloudfront,
    /// `DDoS-Guard` (used by some Russian/CIS hosts). Similar
    /// challenge model to Cloudflare.
    DdosGuard,
    /// Cloudflare's JS-challenge ("I am under attack" mode).
    /// Needs a JS-executing backend.
    CfJsChallenge,
    /// Cloudflare's WAF firewall blocking by signature, requiring
    /// a real browser fingerprint to clear.
    CfFirewall,
    /// JA3/JA4 TLS-fingerprint matching (servers that classify the
    /// client by its TLS handshake shape, not its UA).
    TlsFingerprint,
    /// `Anubis` proof-of-work challenge. Used by codeberg + a
    /// growing number of FOSS projects to discourage scraping.
    Anubis,
    /// Generic captcha challenge (hCaptcha, reCAPTCHA, …). Almost
    /// always blocking — `Uncertain` is the honest answer.
    Captcha,
    /// Trivial UA-strictness: rejects unknown User-Agent strings
    /// but lets through a real-browser UA. Cheapest to bypass.
    UserAgent,
    /// Endpoint requires authentication; no anonymous probe path
    /// exists. Practically unscrapable for OSINT.
    UserAuth,
}

/// HTTP method used to probe a site. Only GET and POST are supported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// Standard GET — the default for ~99% of sites in the registry.
    #[default]
    Get,
    /// POST — for API endpoints that only differentiate accounts via a
    /// body payload (GraphQL queries, form submissions). Pair with
    /// [`Site::request_body`].
    Post,
}

/// serde's `skip_serializing_if` callback contract requires a
/// reference, so the by-value lint on a 1-byte type doesn't apply.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_method(m: &HttpMethod) -> bool {
    matches!(m, HttpMethod::Get)
}

/// Shared detection signature template for a family of sites that
/// run the same forum / blog / wiki software (Discourse, vBulletin,
/// `XenForo`, `MediaWiki`, …). Referenced from [`Site::engine`].
///
/// Engines carry the same kinds of fields as a [`Site`] does (just
/// the inheritable ones — there's no per-engine `url`, that comes
/// from the site itself). At registry load, the engine's fields
/// are merged *under* each referring site's own fields: site wins
/// on conflict.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Engine {
    /// Default detection signals for sites of this family.
    /// Inherited only when the site itself declares no `signals`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<Signal>,
    /// Default extra HTTP headers (e.g. a User-Agent that the
    /// platform accepts where the browser default gets blocked).
    /// Merged with the site's own headers; site wins per-key.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub request_headers: std::collections::BTreeMap<String, String>,
    /// Default username-validity regex inherited only when the site
    /// itself doesn't declare one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex_check: Option<String>,
}

impl Engine {
    /// Compile-check the engine's own constraints — the inheritable
    /// fields are subject to the same validation as a site's would
    /// be.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSite`] when the engine name is
    /// empty, a signal carries an empty marker, or any other
    /// constraint a [`Site::validate`] would also flag.
    pub fn validate(&self, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            return Err(Error::InvalidSite {
                reason: "engine name is empty".into(),
            });
        }
        for signal in &self.signals {
            signal.validate().map_err(|reason| Error::InvalidSite {
                reason: format!("engine {name:?}: {reason}"),
            })?;
        }
        if let Some(pat) = &self.regex_check {
            if let Err(err) = regex::Regex::new(pat) {
                // The Rust `regex` crate refuses look-around for DoS
                // reasons; some upstream registries (Sherlock, WMN)
                // ship patterns that need it. Downgraded from WARN to
                // DEBUG: it's a known structural limit, the probe
                // path falls back gracefully, and the noise dominated
                // CLI startup.
                tracing::debug!(
                    engine = %name, pattern = %pat, error = %err,
                    "engine regex_check did not compile; gate disabled for inheriting sites",
                );
            }
        }
        Ok(())
    }

    /// Fill the inheritable empty / unset fields of `site` from
    /// this engine. Site fields are authoritative: if the site has
    /// any signals at all, no engine signals are merged in.
    /// `request_headers` merge per-key (site wins on per-key
    /// conflict).
    pub fn merge_into(&self, site: &mut Site) {
        if site.signals.is_empty() {
            site.signals.clone_from(&self.signals);
        }
        for (k, v) in &self.request_headers {
            site.request_headers
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        if site.regex_check.is_none() {
            site.regex_check.clone_from(&self.regex_check);
        }
    }
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

/// Upper bound on a site name's length. Names appear in CLI output,
/// CSV columns, and the validate-sites.yml workflow's run-summary
/// table — keeping them short avoids both UI breakage and
/// pathological CI artefacts.
const NAME_MAX_LEN: usize = 80;

/// True when `name` consists only of characters safe to interpolate
/// into shell, CSV, and CLI argument contexts. Matches the JSON
/// Schema pattern `^[\w][\w .()!/+-]*$`.
fn is_safe_site_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == ' '
            || matches!(c, '.' | '(' | ')' | '!' | '/' | '+' | '-')
    })
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
    ///
    /// If the site declares [`strip_bad_char`](Site::strip_bad_char),
    /// those characters are removed from `username` before
    /// substitution — so a `john.doe` probe against a site that
    /// lists `strip_bad_char: "."` actually hits the URL for
    /// `johndoe`, matching the canonical form the site stores
    /// internally.
    pub fn url_for(&self, username: &Username) -> String {
        self.url.substitute(&self.canonical_username(username))
    }

    /// Render the username in the canonical form this site expects.
    ///
    /// This mirrors [`Site::url_for`] without tying callers to URL
    /// substitution, so detection signals can compare the response body
    /// against the same username form that was actually probed.
    pub(crate) fn canonical_username(&self, username: &Username) -> String {
        let raw = username.as_str();
        match self.strip_bad_char.as_deref() {
            Some(chars) if !chars.is_empty() && raw.chars().any(|c| chars.contains(c)) => {
                raw.chars().filter(|c| !chars.contains(*c)).collect()
            }
            _ => raw.to_owned(),
        }
    }

    /// Validate semantic invariants the type system can't enforce
    /// (empty signals list, empty markers, empty status code sets).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::InvalidSite {
                reason: "site name is empty".into(),
            });
        }
        // Site names doubled as shell-interpolation values in the
        // `validate-sites.yml` PR gate; an unsanitised name like
        // `Foo"; rm -rf /; #` would have broken out of `"$name"`
        // quoting and run arbitrary commands on the runner. Both the
        // JSON Schema and this Rust loader enforce a safe character
        // class (word chars plus a few visual punctuation marks) at
        // every entry point.
        if self.name.len() > NAME_MAX_LEN {
            return Err(Error::InvalidSite {
                reason: format!(
                    "site name longer than {NAME_MAX_LEN} chars: {:?}",
                    self.name
                ),
            });
        }
        if !is_safe_site_name(&self.name) {
            return Err(Error::InvalidSite {
                reason: format!(
                    "site name {:?} contains characters outside the allowed \
                     set (word chars, space, `.()!/+-`)",
                    self.name
                ),
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
        if let Some(pat) = &self.regex_check {
            if let Err(err) = regex::Regex::new(pat) {
                // Sherlock's regexes occasionally use lookarounds
                // (e.g. `(?![.-])`), which the Rust `regex` crate
                // doesn't support — it's a true regular-language
                // engine for performance + DoS safety. Rather than
                // reject the whole site over a username-gate the
                // probe path will simply skip and let the site keep
                // working at the cost of one wasted probe per
                // illegal username. Logged at DEBUG (not WARN) — it's
                // a known structural limit, ~8 sites in the embedded
                // registry need look-around. The noise dominated CLI
                // startup; set `ADLER_LOG=debug` to see them again.
                tracing::debug!(
                    site = %self.name, pattern = %pat, error = %err,
                    "regex_check did not compile; username-gate disabled for this site",
                );
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
    /// Votes **`Found`** when the response body contains `text` after
    /// substituting `{username}` with the site's canonical username.
    BodyUsername {
        /// Username-confirming body marker. Must be non-empty and must
        /// contain the literal `{username}` placeholder.
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
    /// Username in the canonical form used for this site.
    pub(crate) username: &'a str,
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
        matches!(
            self,
            Self::BodyPresent { .. } | Self::BodyUsername { .. } | Self::BodyAbsent { .. }
        )
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
            Self::BodyUsername { text } => {
                if probe
                    .body
                    .contains(render_username_marker(text, probe.username).as_str())
                {
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
            Self::BodyUsername { text } => format!(
                "body contains {:?} (body_username)",
                render_username_marker(text, probe.username)
            ),
            Self::BodyAbsent { text } => format!("body contains {text:?} (body_absent)"),
            Self::RedirectAbsent { fragment } => {
                format!("final URL contains {fragment:?} (redirect_absent)")
            }
        }
    }

    /// Whether this signal confirms the concrete username for the current
    /// probe instead of only reporting a generic positive match.
    pub(crate) const fn confirms_username(&self) -> bool {
        matches!(self, Self::BodyUsername { .. })
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
            Self::BodyUsername { text } => {
                if text.is_empty() {
                    return Err("body username signal text is empty".into());
                }
                if !text.contains(PLACEHOLDER) {
                    return Err(format!(
                        "body username signal text missing {PLACEHOLDER} placeholder"
                    ));
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

fn render_username_marker(template: &str, username: &str) -> String {
    template.replace(PLACEHOLDER, username)
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
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        }
    }

    #[test]
    fn url_template_substitutes_placeholder() {
        let user = Username::new("alice").unwrap();
        let site = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        assert_eq!(site.url_for(&user), "https://example.com/alice");
    }

    #[test]
    fn url_for_strips_bad_chars_before_substitution() {
        let user = Username::new("john.doe").unwrap();
        let mut site = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        site.strip_bad_char = Some(".".into());
        assert_eq!(site.url_for(&user), "https://example.com/johndoe");
    }

    #[test]
    fn url_for_strip_bad_char_noop_when_no_match() {
        let user = Username::new("alice").unwrap();
        let mut site = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        site.strip_bad_char = Some(".".into());
        assert_eq!(site.url_for(&user), "https://example.com/alice");
    }

    #[test]
    fn canonical_username_matches_url_stripping() {
        let user = Username::new("john.doe").unwrap();
        let mut site = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        site.strip_bad_char = Some(".".into());
        assert_eq!(site.canonical_username(&user), "johndoe");
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
    fn validate_rejects_bad_body_username_marker() {
        let err = site_with(vec![Signal::BodyUsername {
            text: String::new(),
        }])
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("body username signal"));

        let err = site_with(vec![Signal::BodyUsername {
            text: "username".into(),
        }])
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("missing {username} placeholder"));
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
    fn validate_rejects_shell_metacharacters_in_name() {
        // The validate-sites.yml workflow used to inject `--only "$name"`
        // where `$name` came from PR-controlled sites.json. A name like
        // `Foo"; rm -rf /; #` would have broken out of `"..."` quoting
        // and executed on the runner. Schema + this loader both enforce
        // a safe character class; verify a representative selection of
        // dangerous chars is rejected.
        for bad in [
            "Foo\"; rm -rf /; #",
            "Bar$(curl evil.com)",
            "Baz`whoami`",
            "Qux\\nfoo",
            "back\\slash",
            "pipe|ish",
            "semi;colon",
            "amp&and",
            "lt<gt>",
        ] {
            let mut s = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
            s.name = bad.into();
            let err = s.validate().unwrap_err();
            assert!(
                err.to_string()
                    .contains("characters outside the allowed set"),
                "expected unsafe-name rejection for {bad:?}, got {err}",
            );
        }
    }

    #[test]
    fn validate_accepts_real_world_site_names() {
        // Cross-check the validation against names we actually ship.
        for ok in [
            "GitHub",
            "Steam Community (User)",
            "X / Twitter",
            "osu!",
            "Eintracht Frankfurt Forum",
            "Archive of Our Own",
            "Career.habr",
            "fl",
            "GitLab.com",
            "Sbazar.cz",
        ] {
            let mut s = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
            s.name = ok.into();
            assert!(s.validate().is_ok(), "expected {ok:?} to validate");
        }
    }

    #[test]
    fn validate_rejects_overlong_name() {
        let mut s = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        s.name = "A".repeat(100);
        let err = s.validate().unwrap_err();
        assert!(err.to_string().contains("longer than"));
    }

    #[test]
    fn validate_accepts_well_formed_regex_check() {
        let mut s = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        s.regex_check = Some("^[a-zA-Z0-9_-]{3,40}$".into());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_tolerates_unsupported_regex_features() {
        // Sherlock-imported regexes occasionally use lookarounds
        // (e.g. `(?!...)`) that Rust's `regex` crate can't compile —
        // those sites should still load, with the username-gate
        // silently disabled rather than rejecting the whole site.
        let mut s = site_with(vec![Signal::StatusFound { codes: vec![200] }]);
        s.regex_check = Some("^(?![.-])[a-zA-Z0-9_.-]{3,20}$".into());
        assert!(
            s.validate().is_ok(),
            "lookaround-bearing regex should warn, not reject the site"
        );
    }

    #[test]
    fn signal_status_found_votes_only_on_match() {
        let signal = Signal::StatusFound { codes: vec![200] };
        let probe = Probe {
            status: 200,
            final_url: "https://example.com/alice",
            body: "",
            username: "alice",
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
            username: "alice",
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
            username: "alice",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::NotFound);
        let probe = Probe {
            body: "<h1>Welcome alice</h1>",
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn signal_body_username_votes_found_only_for_rendered_username() {
        let signal = Signal::BodyUsername {
            text: r#""username":"{username}""#.into(),
        };
        let probe = Probe {
            status: 200,
            final_url: "",
            body: r#"{"username":"johndoe"}"#,
            username: "johndoe",
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Found);

        let probe = Probe {
            username: "john.doe",
            ..probe
        };
        assert_eq!(signal.evaluate(&probe), SignalVerdict::Ambiguous);
    }

    #[test]
    fn generic_body_present_does_not_confirm_username() {
        assert!(
            !Signal::BodyPresent {
                text: "username".into()
            }
            .confirms_username()
        );
        assert!(
            Signal::BodyUsername {
                text: "{username}".into()
            }
            .confirms_username()
        );
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
            username: "alice",
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

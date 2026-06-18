//! Normalized profile evidence collected from a positive result.
//!
//! The legacy `CheckOutcome::evidence` field records human-readable signal
//! matches such as `HTTP 200 (status_found)`. This module is the product
//! layer above that: typed profile facts that can later feed confidence,
//! identity clustering, timelines, and reports.

use serde::{Deserialize, Serialize};

use crate::escalation::TransportTier;

/// A normalized fact observed on a profile or profile-like endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEvidence {
    /// What kind of profile fact this is.
    pub kind: ProfileEvidenceKind,
    /// Original extractor/enrichment field name, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Cleaned observed value.
    pub value: String,
    /// Where this fact came from.
    pub source: EvidenceSource,
}

/// Profile evidence categories that higher-level analysis can reason over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileEvidenceKind {
    /// Exact username value confirmed by a site-authored detection signal.
    Username,
    /// A displayed human name.
    DisplayName,
    /// Profile biography or description text.
    Bio,
    /// Avatar or profile image URL.
    AvatarUrl,
    /// Privacy-safe perceptual hash derived from an avatar image.
    AvatarHash,
    /// External website or social link.
    ExternalLink,
    /// Location-like profile field.
    Location,
    /// Account creation or joined date.
    JoinedDate,
    /// HTML/profile title.
    ProfileTitle,
    /// Meta/OpenGraph/Twitter description.
    MetaDescription,
    /// Field that does not yet map to a richer category.
    ExtractedField,
}

/// Source metadata attached to every normalized evidence item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSource {
    /// Site name that produced the result.
    pub site: String,
    /// Concrete profile URL that was probed.
    pub url: String,
    /// Which Adler subsystem produced the fact.
    pub origin: EvidenceOrigin,
    /// Unix epoch milliseconds when Adler observed this fact. Missing on
    /// older persisted scans and on manually-built evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
    /// Coarse, non-secret access context for the probe that produced this
    /// evidence. Deliberately excludes session names, proxy URLs, header
    /// values, and egress identifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_path: Option<EvidenceAccessPath>,
}

/// Non-secret access context for an evidence item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceAccessPath {
    /// Transport that produced the response.
    pub transport: TransportTier,
    /// Whether the result came from an automatic retry through a heavier
    /// transport.
    #[serde(default, skip_serializing_if = "is_false")]
    pub escalated: bool,
    /// Whether an operator-supplied authenticated session was applied.
    #[serde(default, skip_serializing_if = "is_false")]
    pub authenticated: bool,
    /// Reserved for future evidence types that record a missing session as
    /// evidence. Extracted profile evidence should normally leave this false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub session_required: bool,
}

impl EvidenceAccessPath {
    /// Build a non-secret access summary from the live probe path.
    #[must_use]
    pub const fn new(transport: TransportTier, escalations: u8, authenticated: bool) -> Self {
        Self {
            transport,
            escalated: escalations > 0,
            authenticated,
            session_required: false,
        }
    }
}

/// Adler subsystem that produced an evidence item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    /// Evidence came from a detection signal that matched the concrete probe.
    Signal,
    /// Evidence came from a registry extractor rule.
    Extractor,
    /// Evidence was derived from another observed fact without storing the
    /// raw source material.
    Derived,
}

impl ProfileEvidence {
    /// Build normalized evidence from a legacy enrichment field.
    #[must_use]
    pub fn from_enrichment(site: &str, url: &str, field: &str, value: &str) -> Self {
        Self::from_enrichment_with_source(site, url, field, value, None, None)
    }

    /// Build normalized evidence from a live enrichment field with optional
    /// observation/access metadata.
    #[must_use]
    pub fn from_enrichment_with_source(
        site: &str,
        url: &str,
        field: &str,
        value: &str,
        observed_at_ms: Option<u64>,
        access_path: Option<EvidenceAccessPath>,
    ) -> Self {
        Self {
            kind: ProfileEvidenceKind::from_field(field),
            field: Some(field.to_owned()),
            value: value.to_owned(),
            source: EvidenceSource {
                site: site.to_owned(),
                url: url.to_owned(),
                origin: EvidenceOrigin::Extractor,
                observed_at_ms,
                access_path,
            },
        }
    }

    /// Build exact username evidence from a signal that matched the concrete
    /// probed username.
    #[must_use]
    pub fn from_signal_username(
        site: &str,
        url: &str,
        username: &str,
        observed_at_ms: Option<u64>,
        access_path: Option<EvidenceAccessPath>,
    ) -> Self {
        Self {
            kind: ProfileEvidenceKind::Username,
            field: None,
            value: username.to_owned(),
            source: EvidenceSource {
                site: site.to_owned(),
                url: url.to_owned(),
                origin: EvidenceOrigin::Signal,
                observed_at_ms,
                access_path,
            },
        }
    }

    /// Build privacy-safe avatar hash evidence derived from a previously
    /// extracted avatar URL.
    #[must_use]
    pub fn from_avatar_hash(
        site: &str,
        url: &str,
        hash: &str,
        observed_at_ms: Option<u64>,
        access_path: Option<EvidenceAccessPath>,
    ) -> Self {
        Self {
            kind: ProfileEvidenceKind::AvatarHash,
            field: Some("avatar_hash".to_owned()),
            value: hash.to_owned(),
            source: EvidenceSource {
                site: site.to_owned(),
                url: url.to_owned(),
                origin: EvidenceOrigin::Derived,
                observed_at_ms,
                access_path,
            },
        }
    }
}

impl ProfileEvidenceKind {
    /// Map a registry enrichment field name to a normalized evidence kind.
    #[must_use]
    pub fn from_field(field: &str) -> Self {
        match field {
            "name" | "display_name" | "fullname" | "full_name" => Self::DisplayName,
            "bio" | "description" => Self::Bio,
            "avatar" | "avatar_url" | "image" | "profile_image" => Self::AvatarUrl,
            "avatar_hash" | "avatar_perceptual_hash" | "profile_image_hash" => Self::AvatarHash,
            "website" | "url" | "link" | "external_url" => Self::ExternalLink,
            "location" => Self::Location,
            "joined" | "created" | "created_at" | "join_date" => Self::JoinedDate,
            "title" => Self::ProfileTitle,
            "meta_description" | "og_description" => Self::MetaDescription,
            _ => Self::ExtractedField,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_enrichment_fields_to_profile_evidence_kinds() {
        assert_eq!(
            ProfileEvidenceKind::from_field("name"),
            ProfileEvidenceKind::DisplayName
        );
        assert_eq!(
            ProfileEvidenceKind::from_field("avatar"),
            ProfileEvidenceKind::AvatarUrl
        );
        assert_eq!(
            ProfileEvidenceKind::from_field("avatar_hash"),
            ProfileEvidenceKind::AvatarHash
        );
        assert_eq!(
            ProfileEvidenceKind::from_field("website"),
            ProfileEvidenceKind::ExternalLink
        );
        assert_eq!(
            ProfileEvidenceKind::from_field("custom"),
            ProfileEvidenceKind::ExtractedField
        );
    }

    #[test]
    fn serializes_profile_evidence_as_snake_case_wire_data() {
        let ev =
            ProfileEvidence::from_enrichment("GitHub", "https://github.com/alice", "name", "Alice");
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "display_name");
        assert_eq!(json["field"], "name");
        assert_eq!(json["source"]["origin"], "extractor");
        assert!(json["source"].get("observed_at_ms").is_none());
        assert!(json["source"].get("access_path").is_none());
    }

    #[test]
    fn signal_username_evidence_serializes_as_signal_origin_without_field() {
        let ev = ProfileEvidence::from_signal_username(
            "GitLab",
            "https://gitlab.com/api/v4/users?username=alice",
            "alice",
            Some(123),
            Some(EvidenceAccessPath::new(TransportTier::Http, 0, false)),
        );

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "username");
        assert_eq!(json["value"], "alice");
        assert!(json.get("field").is_none());
        assert_eq!(json["source"]["origin"], "signal");
        assert_eq!(json["source"]["observed_at_ms"], 123);
        assert_eq!(json["source"]["access_path"]["transport"], "http");
    }

    #[test]
    fn avatar_hash_evidence_serializes_without_raw_image_data() {
        let ev = ProfileEvidence::from_avatar_hash(
            "Example",
            "https://example.com/alice",
            "ahash64_v1:0123456789abcdef",
            Some(456),
            Some(EvidenceAccessPath::new(TransportTier::Http, 0, false)),
        );

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "avatar_hash");
        assert_eq!(json["field"], "avatar_hash");
        assert_eq!(json["value"], "ahash64_v1:0123456789abcdef");
        assert_eq!(json["source"]["origin"], "derived");
        assert!(!json.to_string().contains("PNG"));
    }

    #[test]
    fn old_profile_evidence_json_defaults_missing_source_metadata() {
        let raw = r#"{
            "kind": "display_name",
            "field": "name",
            "value": "Alice",
            "source": {
                "site": "GitHub",
                "url": "https://github.com/alice",
                "origin": "extractor"
            }
        }"#;

        let ev: ProfileEvidence = serde_json::from_str(raw).unwrap();

        assert_eq!(ev.source.site, "GitHub");
        assert_eq!(ev.source.observed_at_ms, None);
        assert_eq!(ev.source.access_path, None);
    }

    #[test]
    fn source_metadata_serializes_without_secret_names() {
        let ev = ProfileEvidence::from_enrichment_with_source(
            "GitHub",
            "https://github.com/alice",
            "name",
            "Alice",
            Some(1_800_000_000_000),
            Some(EvidenceAccessPath::new(TransportTier::Browser, 1, true)),
        );

        let json = serde_json::to_value(&ev).unwrap();

        assert_eq!(json["source"]["observed_at_ms"], 1_800_000_000_000_u64);
        assert_eq!(json["source"]["access_path"]["transport"], "browser");
        assert_eq!(json["source"]["access_path"]["escalated"], true);
        assert_eq!(json["source"]["access_path"]["authenticated"], true);
        assert!(
            json["source"]["access_path"]
                .get("session_required")
                .is_none()
        );

        let encoded = serde_json::to_string(&ev).unwrap();
        assert!(!encoded.contains("sessionid"));
        assert!(!encoded.contains("acct"));
        assert!(!encoded.contains("proxy"));
    }
}

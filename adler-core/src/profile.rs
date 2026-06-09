//! Normalized profile evidence collected from a positive result.
//!
//! The legacy `CheckOutcome::evidence` field records human-readable signal
//! matches such as `HTTP 200 (status_found)`. This module is the product
//! layer above that: typed profile facts that can later feed confidence,
//! identity clustering, timelines, and reports.

use serde::{Deserialize, Serialize};

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
    /// A displayed human name.
    DisplayName,
    /// Profile biography or description text.
    Bio,
    /// Avatar or profile image URL.
    AvatarUrl,
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
}

/// Adler subsystem that produced an evidence item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    /// Evidence came from a registry extractor rule.
    Extractor,
}

impl ProfileEvidence {
    /// Build normalized evidence from a legacy enrichment field.
    #[must_use]
    pub fn from_enrichment(site: &str, url: &str, field: &str, value: &str) -> Self {
        Self {
            kind: ProfileEvidenceKind::from_field(field),
            field: Some(field.to_owned()),
            value: value.to_owned(),
            source: EvidenceSource {
                site: site.to_owned(),
                url: url.to_owned(),
                origin: EvidenceOrigin::Extractor,
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
            "website" | "url" | "link" | "external_url" => Self::ExternalLink,
            "location" => Self::Location,
            "joined" | "created" | "created_at" | "join_date" => Self::JoinedDate,
            "title" => Self::ProfileTitle,
            "meta_description" | "og_description" => Self::MetaDescription,
            _ => Self::ExtractedField,
        }
    }
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
    }
}

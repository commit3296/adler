//! Public JSON compatibility contracts for core Adler models.

use std::collections::BTreeMap;

use adler_core::{
    CheckOutcome, ConfidenceScore, EvidenceAccessPath, InvestigationReport, MatchKind,
    ProfileEvidence, ReportDisabledSite, ReportTimelineEvent, ReportTimelineEventKind,
    TransportTier, UncertainReason, build_identity_clusters,
};

const OBSERVED_AT_MS: u64 = 1_781_192_451_000;
const GENERATED_AT_MS: u64 = 1_781_192_452_000;

fn pretty_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).expect("serializes")
}

fn evidence(
    site: &str,
    url: &str,
    field: &str,
    value: &str,
    transport: TransportTier,
    escalations: u8,
    authenticated: bool,
) -> ProfileEvidence {
    ProfileEvidence::from_enrichment_with_source(
        site,
        url,
        field,
        value,
        Some(OBSERVED_AT_MS),
        Some(EvidenceAccessPath::new(
            transport,
            escalations,
            authenticated,
        )),
    )
}

fn found_profile(site: &str, profile_evidence: Vec<ProfileEvidence>) -> CheckOutcome {
    let mut outcome = CheckOutcome {
        site: site.to_owned(),
        url: format!("https://{}.example/alice", site.to_lowercase()),
        kind: MatchKind::Found,
        reason: None,
        elapsed_ms: 25,
        enrichment: BTreeMap::new(),
        evidence: vec!["HTTP 200 (status_found)".to_owned()],
        profile_evidence,
        confidence: ConfidenceScore::default(),
        transport: Some(TransportTier::Http),
        escalations: 0,
    };
    outcome.refresh_confidence();
    outcome
}

fn github_found() -> CheckOutcome {
    let site = "GitHub";
    let url = "https://github.example/alice";
    let mut outcome = found_profile(
        site,
        vec![
            evidence(
                site,
                url,
                "name",
                "Alice Liddell",
                TransportTier::Browser,
                1,
                true,
            ),
            evidence(
                site,
                url,
                "bio",
                "Security researcher",
                TransportTier::Browser,
                1,
                true,
            ),
            evidence(
                site,
                url,
                "website",
                "https://alice.dev",
                TransportTier::Browser,
                1,
                true,
            ),
        ],
    );
    url.clone_into(&mut outcome.url);
    outcome.transport = Some(TransportTier::Browser);
    outcome.escalations = 1;
    outcome.refresh_confidence();
    outcome
}

fn gitlab_found() -> CheckOutcome {
    let site = "GitLab";
    let url = "https://gitlab.example/alice";
    let mut outcome = found_profile(
        site,
        vec![evidence(
            site,
            url,
            "website",
            "https://alice.dev",
            TransportTier::Http,
            0,
            false,
        )],
    );
    url.clone_into(&mut outcome.url);
    outcome.refresh_confidence();
    outcome
}

fn forum_captcha() -> CheckOutcome {
    let mut outcome = CheckOutcome {
        site: "Forum".to_owned(),
        url: "https://forum.example/u/alice".to_owned(),
        kind: MatchKind::Uncertain,
        reason: Some(UncertainReason::Captcha),
        elapsed_ms: 40,
        enrichment: BTreeMap::new(),
        evidence: Vec::new(),
        profile_evidence: Vec::new(),
        confidence: ConfidenceScore::default(),
        transport: Some(TransportTier::Http),
        escalations: 0,
    };
    outcome.refresh_confidence();
    outcome
}

#[test]
fn check_outcome_json_contract() {
    insta::assert_snapshot!(pretty_json(&github_found()));
}

#[test]
fn identity_cluster_json_contract() {
    let outcomes = vec![github_found(), gitlab_found()];
    let clusters = build_identity_clusters("alice", &outcomes);

    insta::assert_snapshot!(pretty_json(&clusters));
}

#[test]
fn investigation_report_json_contract() {
    let outcomes = vec![github_found(), gitlab_found(), forum_captcha()];
    let clusters = build_identity_clusters("alice", &outcomes);
    let timeline = vec![ReportTimelineEvent {
        kind: ReportTimelineEventKind::AddedFound,
        site: Some("GitHub".to_owned()),
        scan_id: Some("scan123".to_owned()),
        observed_at_ms: Some(OBSERVED_AT_MS),
        detail: Some("new found".to_owned()),
    }];
    let disabled = ReportDisabledSite {
        name: "Threads".to_owned(),
        url: "https://threads.example/@{username}".to_owned(),
        tags: vec!["social".to_owned(), "parked".to_owned()],
        disabled_reason: "login wall".to_owned(),
    };

    let report = InvestigationReport::builder("alice", &outcomes)
        .identity_clusters(clusters)
        .timeline(timeline)
        .disabled_sites(vec![disabled])
        .generated_at_ms(GENERATED_AT_MS)
        .build();

    insta::assert_snapshot!(pretty_json(&report));
}

//! End-to-end tests for the `adler` binary.

use std::io::BufRead;
use std::io::BufReader;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt as _;
use predicates::str;
use tempfile::NamedTempFile;
use wiremock::matchers::{any, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sites_file(json: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    f.write_all(json.as_bytes()).expect("write");
    f
}

/// A registry pointing at a closed local port — every probe will be Uncertain.
fn dead_local_sites() -> NamedTempFile {
    sites_file(
        r#"{"sites":[{"name":"Local","url":"http://127.0.0.1:1/{username}","signals":[{"kind":"status_found","codes":[200]}]}]}"#,
    )
}

fn adler() -> Command {
    Command::cargo_bin("adler").expect("cargo bin adler")
}

#[test]
fn help_lists_flags_and_examples() {
    adler()
        .arg("--help")
        .assert()
        .success()
        .stdout(str::contains("Examples:"))
        .stdout(str::contains("--only"))
        .stdout(str::contains("--format"))
        .stdout(str::contains("--exclude"));
}

#[test]
fn list_sites_prints_filtered_names_without_username() {
    let sites = sites_file(
        r#"{"sites":[
            {"name":"GitHub","url":"https://github.com/{username}","signals":[{"kind":"status_found","codes":[200]}]},
            {"name":"GitLab","url":"https://gitlab.com/{username}","signals":[{"kind":"status_found","codes":[200]}]},
            {"name":"Reddit","url":"https://reddit.com/u/{username}","signals":[{"kind":"status_found","codes":[200]}]}
        ]}"#,
    );
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--list-sites",
            "--only",
            "git",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, ["GitHub", "GitLab"], "got {lines:?}");
}

#[test]
fn top_n_filters_to_ranked_sites_in_popularity_order() {
    // Three sites: GitHub rank 9, Reddit rank 6, GitLab unranked.
    // `--top 30` keeps the two ranked entries and orders them by
    // popularity (Reddit before GitHub). The unranked site is
    // dropped — it has no rank to compete with.
    let sites = sites_file(
        r#"{"sites":[
            {"name":"GitHub","url":"https://github.com/{username}","signals":[{"kind":"status_found","codes":[200]}],"popularity":9},
            {"name":"GitLab","url":"https://gitlab.com/{username}","signals":[{"kind":"status_found","codes":[200]}]},
            {"name":"Reddit","url":"https://reddit.com/u/{username}","signals":[{"kind":"status_found","codes":[200]}],"popularity":6}
        ]}"#,
    );
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--list-sites",
            "--top",
            "30",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, ["Reddit", "GitHub"], "got {lines:?}");
}

#[test]
fn completions_emit_a_script() {
    adler()
        .args(["--completions", "bash"])
        .assert()
        .success()
        .stdout(str::contains("_adler"));
}

#[test]
fn version_prints_semver() {
    adler()
        .arg("--version")
        .assert()
        .success()
        .stdout(str::starts_with("adler "));
}

#[test]
fn invalid_username_exits_2() {
    let sites = dead_local_sites();
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "bad space",
        ])
        .assert()
        .code(2)
        .stderr(str::contains("invalid username"));
}

#[test]
fn missing_sites_file_exits_2() {
    adler()
        .args([
            "--sites",
            "/this/path/does/not/exist.json",
            "--no-progress",
            "alice",
        ])
        .assert()
        .code(2);
}

#[test]
fn empty_filter_result_exits_2() {
    let sites = dead_local_sites();
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--only",
            "this-substring-matches-nothing-xyz",
            "--no-progress",
            "alice",
        ])
        .assert()
        .code(2)
        .stderr(str::contains("no sites match"));
}

#[test]
fn disabled_only_filter_explains_parked_site() {
    let sites = sites_file(
        r#"{"sites":[
            {"name":"TikTok","url":"https://www.tiktok.com/@{username}","signals":[{"kind":"status_found","codes":[200]}],"disabled":true,"disabled_reason":"Honest Limits: JS-only SPA never hydrates"}
        ]}"#,
    );
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--only",
            "TikTok",
            "--no-progress",
            "alice",
        ])
        .assert()
        .code(2)
        .stderr(str::contains("no enabled sites match the filter"))
        .stderr(str::contains("TikTok: Honest Limits"));
}

#[test]
fn nothing_found_exits_1_and_emits_valid_json_array() {
    let sites = dead_local_sites();
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--timeout",
            "1",
            "--format",
            "json",
            "alice",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = value.as_array().expect("top-level JSON array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "uncertain");
    assert!(
        value.get("summary").is_none(),
        "scan JSON must not become an envelope"
    );
    assert!(
        value.get("outcomes").is_none(),
        "scan JSON must remain a top-level array"
    );
    // Connection refused → a structured Network reason: {"network": "..."}.
    assert!(arr[0]["reason"]["network"].is_string(), "{}", arr[0]);
}

#[test]
fn ndjson_emits_one_object_per_line() {
    let sites = sites_file(
        r#"{"sites":[
            {"name":"A","url":"http://127.0.0.1:1/{username}","signals":[{"kind":"status_found","codes":[200]}]},
            {"name":"B","url":"http://127.0.0.1:1/b/{username}","signals":[{"kind":"status_found","codes":[200]}]}
        ]}"#,
    );
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--timeout",
            "1",
            "--format",
            "ndjson",
            "alice",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        assert!(v.get("site").is_some());
        assert!(v.get("kind").is_some());
    }
}

#[test]
fn text_output_summary_is_stable() {
    // Snapshot the text output for a deterministic scenario: a single dead
    // site that always produces Uncertain. We normalise the dynamic
    // elapsed-time field via insta filters.
    let sites = dead_local_sites();
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--timeout",
            "1",
            "alice",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(r"\d+\.\d{2}s", "<TIME>s");
    settings.add_filter(r"127\.0\.0\.1:\d+", "127.0.0.1:<PORT>");
    settings.add_filter(r"error sending request[^\n]*", "<reqwest error>");
    settings.bind(|| {
        insta::assert_snapshot!("text_uncertain_run", stdout);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn found_via_status_strategy_exits_0() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "alice",
        ])
        .assert()
        .success()
        .stdout(str::contains("[+] Mock"))
        .stdout(str::contains("1 found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_healthy_for_well_behaved_site() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[
            {{"kind":"status_found","codes":[200]}},
            {{"kind":"status_not_found","codes":[404]}}
        ],"known_present":"alice"}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--doctor",
        ])
        .assert()
        .success()
        .stdout(str::contains("[OK]"))
        .stdout(str::contains("1 site(s) checked, 0 failed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_format_json_emits_structured_envelope() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[
            {{"name":"Healthy","url":"{0}/{{username}}","signals":[
                {{"kind":"status_found","codes":[200]}},
                {{"kind":"status_not_found","codes":[404]}}
            ],"known_present":"alice"}}
        ]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--doctor",
            "--format",
            "json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON envelope");
    let sites_arr = parsed["sites"].as_array().expect("sites array");
    assert_eq!(sites_arr.len(), 1);
    assert_eq!(sites_arr[0]["name"], "Healthy");
    assert_eq!(sites_arr[0]["verdict"], "healthy");
    assert!(sites_arr[0]["issues"].as_array().unwrap().is_empty());
    assert_eq!(parsed["summary"]["total"], 1);
    assert_eq!(parsed["summary"]["healthy"], 1);
    assert_eq!(parsed["summary"]["failing"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_format_ndjson_emits_line_delimited_records_plus_summary() {
    let server = MockServer::start().await;
    // Site that fails too-permissive: any URL → 200.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[
            {{"name":"TooPerm","url":"{0}/{{username}}","signals":[
                {{"kind":"status_found","codes":[200]}}
            ],"known_present":"alice"}}
        ]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--doctor",
            "--format",
            "ndjson",
        ])
        .assert()
        .code(1); // failures > 0 → exit 1, same as text format
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected one site record + one summary: {lines:?}"
    );

    let site_record: serde_json::Value = serde_json::from_str(lines[0]).expect("site record JSON");
    assert_eq!(site_record["name"], "TooPerm");
    assert_eq!(site_record["verdict"], "unhealthy");
    let issues = site_record["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.as_str().unwrap_or("").contains("too permissive")),
        "expected too-permissive issue, got {issues:?}",
    );

    let summary: serde_json::Value = serde_json::from_str(lines[1]).expect("summary JSON");
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["failing"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_rejects_format_csv_with_actionable_message() {
    // Tiny registry so this is fast even though the rejection happens
    // after sites load.
    let sites = sites_file(
        r#"{"sites":[{"name":"X","url":"https://x.example/{username}","signals":[
            {"kind":"status_found","codes":[200]}]}]}"#,
    );
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--doctor",
            "--format",
            "csv",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("--doctor supports --format text|json|ndjson"),
        "expected actionable error, got: {stderr}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_exits_1_when_signature_too_permissive() {
    let server = MockServer::start().await;
    // Always 200 — both real and nonsense users look "Found". Signature is
    // broken; doctor must catch it.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[
            {{"kind":"status_found","codes":[200]}}
        ],"known_present":"alice"}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--doctor",
        ])
        .assert()
        .code(1)
        .stdout(str::contains("[FAIL]"))
        .stdout(str::contains("too permissive"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_serves_found_after_backend_goes_down() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let uri = server.uri();
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{uri}/{{username}}","signals":[
            {{"kind":"status_found","codes":[200]}},
            {{"kind":"status_not_found","codes":[404]}}
        ]}}]}}"#,
    );
    let sites = sites_file(&json);
    let cache = NamedTempFile::new().unwrap();
    let cache_path = cache.path().to_path_buf();
    drop(cache); // we only want the path; cache file is written by adler

    // First run populates the cache with a Found verdict.
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--cache-path",
            cache_path.to_str().unwrap(),
            "--no-progress",
            "alice",
        ])
        .assert()
        .success()
        .stdout(str::contains("[+] Mock"));

    // Take the backend down; a fresh probe would now be Uncertain.
    drop(server);

    // Second run must serve the cached Found without touching the network.
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--cache-path",
            cache_path.to_str().unwrap(),
            "--no-progress",
            "--timeout",
            "1",
            "alice",
        ])
        .assert()
        .success()
        .stdout(str::contains("[+] Mock"));

    let _ = std::fs::remove_file(&cache_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrich_extracts_profile_fields_for_found_sites() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body><h1 class="name">Alice L</h1><img class="av" src="https://cdn/a.png"></body></html>"#,
        ))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[
            {{"kind":"status_found","codes":[200]}}
        ],"extract":[
            {{"field":"name","selector":"h1.name"}},
            {{"field":"avatar","selector":"img.av","attr":"src"}}
        ]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--enrich",
            "--no-progress",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("name: Alice L"), "missing name: {stdout}");
    assert!(
        stdout.contains("avatar: https://cdn/a.png"),
        "missing avatar: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correlate_links_accounts_with_matching_name() {
    let server = MockServer::start().await;
    // Two sites, both found, both expose the same name via an extractor.
    for seg in ["a", "b"] {
        Mock::given(any())
            .and(path(format!("/{seg}/alice")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"<h1 class="n">Alice Liddell</h1>"#),
            )
            .mount(&server)
            .await;
    }
    let json = format!(
        r#"{{"sites":[
            {{"name":"SiteA","url":"{0}/a/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}],
              "extract":[{{"field":"name","selector":"h1.n"}}]}},
            {{"name":"SiteB","url":"{0}/b/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}],
              "extract":[{{"field":"name","selector":"h1.n"}}]}}
        ]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--correlate",
            "--no-progress",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("correlation:"), "no section: {stdout}");
    assert!(
        stdout.contains("SiteA, SiteB"),
        "expected a cluster of both sites: {stdout}"
    );
    assert!(
        stdout.contains("shared name: \"alice liddell\""),
        "expected shared name: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn html_format_renders_self_contained_report() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"<h1 class="n">Alice L</h1>"#))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}],
            "extract":[{{"field":"name","selector":"h1.n"}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--enrich",
            "--format",
            "html",
            "--no-progress",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.starts_with("<!DOCTYPE html>"), "not HTML: {stdout}");
    assert!(stdout.contains("<title>Adler report — alice</title>"));
    assert!(stdout.contains(">Mock</a>"));
    assert!(stdout.contains("name</span> Alice L"));
    assert!(stdout.trim_end().ends_with("</html>"));
}

fn write_report_scan_fixture(dir: &Path) {
    let scan = serde_json::json!({
        "schema_version": 2,
        "scan_id": "scan123",
        "username": "alice",
        "site_count": 2,
        "created_at_ms": 1_781_192_451_000_u64,
        "summary": {"found": 2, "not_found": 0, "uncertain": 0},
        "outcomes": [
            {
                "site": "GitHub",
                "url": "https://github.com/alice",
                "kind": "found",
                "elapsed_ms": 12,
                "evidence": ["HTTP 200 (status_found)"],
                "profile_evidence": [{
                    "kind": "external_link",
                    "field": "website",
                    "value": "https://alice.dev",
                    "source": {
                        "site": "GitHub",
                        "url": "https://github.com/alice",
                        "origin": "extractor"
                    }
                }]
            },
            {
                "site": "GitLab",
                "url": "https://gitlab.com/alice",
                "kind": "found",
                "elapsed_ms": 15,
                "evidence": ["HTTP 200 (status_found)"],
                "profile_evidence": [{
                    "kind": "external_link",
                    "field": "website",
                    "value": "https://alice.dev",
                    "source": {
                        "site": "GitLab",
                        "url": "https://gitlab.com/alice",
                        "origin": "extractor"
                    }
                }]
            }
        ],
        "elapsed_ms": 42
    });
    std::fs::write(
        dir.join("scan123.json"),
        serde_json::to_vec_pretty(&scan).unwrap(),
    )
    .expect("write scan");
}

#[test]
fn report_scan_renders_markdown_from_persisted_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_report_scan_fixture(dir.path());

    adler()
        .args([
            "--report-scan",
            "scan123",
            "--scans-dir",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(str::contains("# Adler investigation report: alice"))
        .stdout(str::contains("## Identity Clusters"))
        .stdout(str::contains("identity-0001"))
        .stdout(str::contains("shared external link"))
        .stdout(str::contains("## Evidence Table"));
}

#[test]
fn report_scan_renders_json_from_persisted_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_report_scan_fixture(dir.path());

    let assert = adler()
        .args([
            "--report-scan",
            "scan123",
            "--report-format",
            "json",
            "--scans-dir",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid report JSON");

    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["username"], "alice");
    assert_eq!(report["summary"]["found"], 2);
    assert!(
        report.get("report").is_none(),
        "report JSON must not be wrapped in an envelope"
    );
    assert!(
        report.get("outcomes").is_none(),
        "report JSON must be InvestigationReport, not raw persisted scan"
    );
    assert_eq!(report["found_accounts"].as_array().unwrap().len(), 2);
    assert_eq!(report["identity_clusters"][0]["id"], "identity-0001");
    assert_eq!(
        report["identity_clusters"][0]["reasons"][0]["kind"],
        "shared_external_link"
    );
    assert_eq!(report["evidence_table"].as_array().unwrap().len(), 2);
    assert_eq!(report["timeline"].as_array().unwrap().len(), 2);
}

#[test]
fn report_scan_renders_html_from_persisted_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_report_scan_fixture(dir.path());

    adler()
        .args([
            "--report-scan",
            "scan123",
            "--report-format",
            "html",
            "--scans-dir",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(str::contains("<!doctype html>"))
        .stdout(str::contains("<h2>Summary</h2>"))
        .stdout(str::contains("<h2>Identity Clusters</h2>"))
        .stdout(str::contains("identity-0001"))
        .stdout(str::contains("shared external link"))
        .stdout(str::contains("<h2>Evidence Table</h2>"))
        .stdout(str::contains("<h2>Timeline</h2>"))
        .stdout(str::contains("<h2>Limitations</h2>"));
}

#[test]
fn report_format_requires_report_scan() {
    adler()
        .args(["--report-format", "json"])
        .assert()
        .failure()
        .stderr(str::contains("--report-scan"));
}

#[test]
fn permute_basic_scans_separator_variants() {
    // Dead local port → all Uncertain, but every variant URL must appear.
    let sites = sites_file(
        r#"{"sites":[{"name":"Local","url":"http://127.0.0.1:1/{username}","signals":[{"kind":"status_found","codes":[200]}]}]}"#,
    );
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--permute",
            "basic",
            "--no-progress",
            "--no-cache",
            "--timeout",
            "1",
            "john_doe",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for variant in ["john_doe", "johndoe", "john.doe", "john-doe"] {
        assert!(
            stdout.contains(&format!("/{variant}")),
            "missing variant {variant}: {stdout}"
        );
    }
}

#[test]
fn audit_log_appends_ndjson_records() {
    let sites = sites_file(
        r#"{"sites":[{"name":"Local","url":"http://127.0.0.1:1/{username}","signals":[{"kind":"status_found","codes":[200]}]}]}"#,
    );
    let log = NamedTempFile::new().unwrap();
    let log_path = log.path().to_path_buf();
    drop(log);
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--audit-log",
            log_path.to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--timeout",
            "1",
            "alice",
        ])
        .assert()
        .code(1);
    let contents = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1, "one record per outcome: {contents}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["username"], "alice");
    assert_eq!(v["site"], "Local");
    assert!(v["ts"].is_number());
    assert!(v["kind"].is_string());
    let _ = std::fs::remove_file(&log_path);
}

#[test]
fn no_cache_flag_skips_cache_file() {
    let sites = dead_local_sites();
    let cache = NamedTempFile::new().unwrap();
    let cache_path = cache.path().to_path_buf();
    drop(cache);
    adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--cache-path",
            cache_path.to_str().unwrap(),
            "--no-cache",
            "--no-progress",
            "--timeout",
            "1",
            "alice",
        ])
        .assert()
        .code(1);
    assert!(
        !cache_path.exists(),
        "--no-cache must not create a cache file",
    );
}

/// Build a two-site mock (one 200/Found, one 404/NotFound) and return the
/// sites-file plus the running server (kept alive by the caller).
async fn yes_no_sites() -> (MockServer, tempfile::NamedTempFile) {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/yes/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .and(path("/no/alice"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[
            {{"name":"Yes","url":"{0}/yes/{{username}}","signals":[
                {{"kind":"status_found","codes":[200]}},
                {{"kind":"status_not_found","codes":[404]}}
            ]}},
            {{"name":"No","url":"{0}/no/{{username}}","signals":[
                {{"kind":"status_found","codes":[200]}},
                {{"kind":"status_not_found","codes":[404]}}
            ]}}
        ]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    (server, sites)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_hides_not_found_rows() {
    let (_server, sites) = yes_no_sites().await;
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[+] Yes"), "expected Found row: {stdout}");
    assert!(
        !stdout.contains("[-] No"),
        "NotFound row hidden by default: {stdout}"
    );
    // Summary still reflects all sites.
    assert!(stdout.contains("1 found"));
    assert!(stdout.contains("1 not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_flag_shows_not_found_rows() {
    let (_server, sites) = yes_no_sites().await;
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--all",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[+] Yes"), "{stdout}");
    assert!(
        stdout.contains("[-] No"),
        "--all should show NotFound: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_prints_only_found_urls() {
    let (_server, sites) = yes_no_sites().await;
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--quiet",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("/yes/alice"), "{stdout}");
    assert!(!stdout.contains("/no/alice"), "{stdout}");
    assert!(
        !stdout.contains("found ·"),
        "no tally in quiet mode: {stdout}"
    );
    assert!(!stdout.contains('['), "no symbols in quiet mode: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_site_emits_entry_from_status_diff() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/{{username}}", server.uri());
    let assert = adler()
        .args(["--add-site", &url, "--name", "Mock", "alice"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let entry: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON entry");
    assert_eq!(entry["name"], "Mock");
    assert_eq!(entry["known_present"], "alice");
    let kinds: Vec<&str> = entry["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"status_found"), "{stdout}");
    assert!(kinds.contains(&"status_not_found"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_site_exits_1_when_indistinguishable() {
    let server = MockServer::start().await;
    // Same status + body for everyone → nothing to derive.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("<title>Same</title>"))
        .mount(&server)
        .await;
    let url = format!("{}/{{username}}", server.uri());
    adler()
        .args(["--add-site", &url, "alice"])
        .assert()
        .code(1)
        .stderr(str::contains("couldn't derive"));
}

#[test]
fn add_site_without_username_errors() {
    // No positional username → our tailored error (clap allows it via
    // required_unless, we reject in run_add_site).
    adler()
        .args(["--add-site", "https://example.com/{username}"])
        .assert()
        .code(2)
        .stderr(str::contains("needs a username"));
}

fn tagged_sites_file() -> NamedTempFile {
    sites_file(
        r#"{"sites":[
            {"name":"DevSite","url":"https://dev.example/{username}","signals":[{"kind":"status_found","codes":[200]}],"tags":["dev"]},
            {"name":"SocialSite","url":"https://soc.example/{username}","signals":[{"kind":"status_found","codes":[200]}],"tags":["social","region:ru"]},
            {"name":"PlainSite","url":"https://plain.example/{username}","signals":[{"kind":"status_found","codes":[200]}]}
        ]}"#,
    )
}

#[test]
fn list_tags_prints_tags_with_counts() {
    let sites = tagged_sites_file();
    let assert = adler()
        .args(["--sites", sites.path().to_str().unwrap(), "--list-tags"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // Sorted, "<tag>\t<count>" per line.
    assert!(stdout.contains("dev\t1"), "{stdout}");
    assert!(stdout.contains("social\t1"), "{stdout}");
    assert!(stdout.contains("region:ru\t1"), "{stdout}");
}

#[test]
fn tag_filter_restricts_listed_sites() {
    let sites = tagged_sites_file();
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--list-sites",
            "--tag",
            "dev",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["DevSite"],
        "tag filter should keep only dev-tagged: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_batch_scans_each_username_grouped_json() {
    let server = MockServer::start().await;
    // alice exists, others 404.
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}},{{"kind":"status_not_found","codes":[404]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);

    let mut users = NamedTempFile::new().unwrap();
    // blank line + comment must be skipped; "alice" duplicated must dedupe.
    writeln!(users, "alice\n# a comment\n\nbob\nalice").unwrap();

    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--input",
            users.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .assert()
        .success(); // alice found somewhere -> exit 0
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let arr: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = arr.as_array().unwrap();
    // alice + bob, deduped (alice once), positional none.
    assert_eq!(arr.len(), 2, "{stdout}");
    assert_eq!(arr[0]["username"], "alice");
    assert_eq!(arr[1]["username"], "bob");
    assert_eq!(arr[0]["results"][0]["kind"], "found");
    assert_eq!(arr[1]["results"][0]["kind"], "not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_quiet_emits_username_tab_url() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}},{{"kind":"status_not_found","codes":[404]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let mut users = NamedTempFile::new().unwrap();
    writeln!(users, "alice\nbob").unwrap();

    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-cache",
            "--quiet",
            "--input",
            users.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("alice\t"), "{stdout}");
    assert!(
        stdout.lines().next().unwrap().ends_with("/alice"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("bob\t"),
        "bob not found -> no line: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_prints_signal_evidence_and_json_always_has_it() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);

    // text + --explain prints a "why:" line.
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--explain",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("why: HTTP 200 (status_found)"), "{stdout}");

    // JSON always carries evidence (no flag).
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--format",
            "json",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let arr: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(arr[0]["evidence"][0], "HTTP 200 (status_found)", "{stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csv_output_has_header_and_rows() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--format",
            "csv",
            "alice",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "site,url,kind,reason,elapsed_ms,evidence");
    assert!(lines[1].starts_with("Mock,"), "{stdout}");
    assert!(lines[1].contains(",found,"), "{stdout}");
    assert!(lines[1].ends_with("HTTP 200 (status_found)"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csv_batch_prepends_username_column() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}},{{"kind":"status_not_found","codes":[404]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    let mut users = NamedTempFile::new().unwrap();
    writeln!(users, "alice\nbob").unwrap();
    let assert = adler()
        .args([
            "--sites",
            sites.path().to_str().unwrap(),
            "--no-progress",
            "--no-cache",
            "--format",
            "csv",
            "--input",
            users.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(
        stdout.lines().next().unwrap(),
        "username,site,url,kind,reason,elapsed_ms,evidence"
    );
    assert!(stdout.contains("alice,Mock,"), "{stdout}");
    assert!(stdout.contains("bob,Mock,"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_records_baseline_then_reports_no_change() {
    let server = MockServer::start().await;
    Mock::given(any())
        .and(path("/alice"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let json = format!(
        r#"{{"sites":[{{"name":"Mock","url":"{}/{{username}}","signals":[{{"kind":"status_found","codes":[200]}},{{"kind":"status_not_found","codes":[404]}}]}}]}}"#,
        server.uri()
    );
    let sites = sites_file(&json);
    // A unique temp dir so the watch snapshot (<cache_dir>/watch/alice.json)
    // is isolated from other runs — a plain NamedTempFile lives in the shared
    // /tmp, so its sibling watch/ dir would leak between test runs.
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache.json");

    let common = [
        "--sites",
        sites.path().to_str().unwrap(),
        "--no-progress",
        "--watch",
        "--cache-path",
        cache.to_str().unwrap(),
    ];

    // First run: baseline.
    let assert = adler().args(common).arg("alice").assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("baseline recorded"), "{out}");
    assert!(out.contains("1 found"), "{out}");

    // Second run: same result → no change.
    let assert = adler().args(common).arg("alice").assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("no change"), "{out}");
}

#[test]
fn mcp_stdio_serves_initialize_tools_resources_prompts() {
    // Spawn the binary with `--mcp` (stdio transport), drive a full
    // JSON-RPC handshake plus one of each: tools/list, resources/list,
    // prompts/list. Exercises the same surface a Claude-Desktop-style
    // host would see when configuring adler-mcp as a subprocess.
    let mut child = std::process::Command::cargo_bin("adler")
        .expect("cargo bin adler")
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn adler --mcp");

    let stdin = child.stdin.as_mut().expect("stdin piped");
    let writes = [
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"integration","version":"0.0"}},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"tools/list","id":2}"#,
        r#"{"jsonrpc":"2.0","method":"resources/list","id":3}"#,
        r#"{"jsonrpc":"2.0","method":"prompts/list","id":4}"#,
    ];
    for line in writes {
        writeln!(stdin, "{line}").expect("write stdin");
    }
    // Drop stdin so the server sees EOF and exits cleanly once the
    // pending requests are drained.
    drop(child.stdin.take());

    let stdout = child.stdout.take().expect("stdout piped");
    let reader = BufReader::new(stdout);
    let mut responses_by_id: std::collections::HashMap<u64, serde_json::Value> =
        std::collections::HashMap::new();
    for line in reader.lines() {
        let Ok(raw) = line else { break };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) {
            responses_by_id.insert(id, value);
        }
        if responses_by_id.contains_key(&1)
            && responses_by_id.contains_key(&2)
            && responses_by_id.contains_key(&3)
            && responses_by_id.contains_key(&4)
        {
            break;
        }
    }

    let _ = child.wait();

    // 1 — initialize
    let init = &responses_by_id[&1];
    assert_eq!(init["result"]["serverInfo"]["name"], "adler-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["capabilities"]["resources"].is_object());
    assert!(init["result"]["capabilities"]["prompts"].is_object());

    // 2 — tools/list: core MCP tool names
    let tools = responses_by_id[&2]["result"]["tools"].as_array().unwrap();
    let tool_names: std::collections::HashSet<&str> =
        tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "list_sites",
        "scan_username",
        "scan_batch",
        "doctor_check",
        "get_scan_history",
        "diff_scans",
    ] {
        assert!(
            tool_names.contains(expected),
            "missing tool {expected}: have {tool_names:?}",
        );
    }

    // 3 — resources/list: the 4 static resources
    let resources = responses_by_id[&3]["result"]["resources"]
        .as_array()
        .unwrap();
    let resource_names: std::collections::HashSet<&str> = resources
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    for expected in [
        "registry_sites",
        "registry_tags",
        "registry_disabled",
        "scans_recent",
    ] {
        assert!(
            resource_names.contains(expected),
            "missing resource {expected}: have {resource_names:?}",
        );
    }

    // 4 — prompts/list: 3 names
    let prompts = responses_by_id[&4]["result"]["prompts"].as_array().unwrap();
    let prompt_names: std::collections::HashSet<&str> =
        prompts.iter().filter_map(|p| p["name"].as_str()).collect();
    for expected in [
        "investigate_username",
        "audit_registry_health",
        "correlate_accounts",
    ] {
        assert!(
            prompt_names.contains(expected),
            "missing prompt {expected}: have {prompt_names:?}",
        );
    }
}

#[test]
fn mcp_stdio_tool_call_returns_structured_content() {
    // End-to-end tool call: list_sites with a tag filter. Exercises the
    // tools/call dispatch path — initialize → tools/call → parse the
    // structuredContent envelope.
    let mut child = std::process::Command::cargo_bin("adler")
        .expect("cargo bin adler")
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn adler --mcp");

    let stdin = child.stdin.as_mut().expect("stdin piped");
    for line in [
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"integration","version":"0.0"}},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_sites","arguments":{"tag":["coding"]}},"id":2}"#,
    ] {
        writeln!(stdin, "{line}").expect("write stdin");
    }
    drop(child.stdin.take());

    let stdout = child.stdout.take().expect("stdout piped");
    let reader = BufReader::new(stdout);
    let mut call_response: Option<serde_json::Value> = None;
    for line in reader.lines() {
        let Ok(raw) = line else { break };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        if value.get("id").and_then(serde_json::Value::as_u64) == Some(2) {
            call_response = Some(value);
            break;
        }
    }
    let _ = child.wait();

    let response = call_response.expect("expected response for id=2");
    let structured = &response["result"]["structuredContent"];
    let total = structured["total"].as_u64().expect("total field");
    assert!(
        total >= 5,
        "expected several coding-tagged sites, got {total}"
    );
    let sites = structured["sites"].as_array().unwrap();
    let names: std::collections::HashSet<&str> =
        sites.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains("GitHub") || names.contains("GitLab"),
        "expected GitHub or GitLab in coding-tagged list_sites response: {names:?}",
    );
}

#[test]
fn mcp_stdio_handles_adversarial_inputs() {
    // Drive a single stdio session through five adversarial JSON-RPC
    // calls and verify every one produces a well-formed error response
    // (no server crash, no empty result, no malformed JSON). Each
    // probe targets a different boundary check we don't otherwise
    // exercise:
    //   1. tools/call with an unknown tool name
    //   2. prompts/get with a missing required argument
    //   3. resources/read with a path-traversal `{id}` in the URI
    //   4. prompts/get with an unknown prompt name
    //   5. tools/call with a syntactically-valid but semantically-empty
    //      filter that matches zero sites
    let mut child = std::process::Command::cargo_bin("adler")
        .expect("cargo bin adler")
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn adler --mcp");

    let stdin = child.stdin.as_mut().expect("stdin piped");
    for line in [
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"adv","version":"0.0"}},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        // 2. unknown tool
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"does_not_exist","arguments":{}},"id":2}"#,
        // 3. prompts/get missing required arg
        r#"{"jsonrpc":"2.0","method":"prompts/get","params":{"name":"investigate_username","arguments":{}},"id":3}"#,
        // 4. resources/read with path traversal
        r#"{"jsonrpc":"2.0","method":"resources/read","params":{"uri":"adler://scans/../../etc/passwd"},"id":4}"#,
        // 5. prompts/get unknown name
        r#"{"jsonrpc":"2.0","method":"prompts/get","params":{"name":"nope","arguments":{}},"id":5}"#,
        // 6. tools/call list_sites with a tag filter that matches zero sites.
        // Note: list_sites doesn't error on empty matches; it returns total=0.
        // This verifies the "no error, just empty" contract.
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_sites","arguments":{"tag":["this-tag-matches-nothing-xyz-42"]}},"id":6}"#,
    ] {
        writeln!(stdin, "{line}").expect("write stdin");
    }
    drop(child.stdin.take());

    let stdout = child.stdout.take().expect("stdout piped");
    let reader = BufReader::new(stdout);
    let mut responses: std::collections::HashMap<u64, serde_json::Value> =
        std::collections::HashMap::new();
    for line in reader.lines() {
        let Ok(raw) = line else { break };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) {
            responses.insert(id, value);
        }
        if responses.contains_key(&1)
            && responses.contains_key(&2)
            && responses.contains_key(&3)
            && responses.contains_key(&4)
            && responses.contains_key(&5)
            && responses.contains_key(&6)
        {
            break;
        }
    }
    let _ = child.wait();

    // 1 — initialize succeeded (covered by other tests, smoke-only here).
    assert_eq!(responses[&1]["result"]["serverInfo"]["name"], "adler-mcp");

    // 2 — unknown tool name. rmcp surfaces this as an error envelope.
    let r2 = &responses[&2];
    assert!(
        r2.get("error").is_some() || r2["result"]["isError"].as_bool() == Some(true),
        "expected error for unknown tool, got {r2}",
    );

    // 3 — missing required arg.
    let r3 = &responses[&3];
    let r3_msg = r3["error"]["message"].as_str().unwrap_or("");
    assert!(
        r3_msg.to_lowercase().contains("requires argument"),
        "expected 'requires argument' error, got {r3_msg:?}",
    );

    // 4 — path traversal id rejected.
    let r4 = &responses[&4];
    let r4_msg = r4["error"]["message"].as_str().unwrap_or("");
    assert!(
        r4_msg.to_lowercase().contains("unknown resource"),
        "expected 'unknown resource' error for path-traversal id, got {r4_msg:?}",
    );

    // 5 — unknown prompt name.
    let r5 = &responses[&5];
    let r5_msg = r5["error"]["message"].as_str().unwrap_or("");
    assert!(
        r5_msg.to_lowercase().contains("unknown prompt"),
        "expected 'unknown prompt' error, got {r5_msg:?}",
    );

    // 6 — list_sites with empty match doesn't error; returns total=0.
    let r6 = &responses[&6];
    let total = r6["result"]["structuredContent"]["total"]
        .as_u64()
        .expect("total field on successful list_sites response");
    assert_eq!(
        total, 0,
        "expected zero sites for nonsense tag, got {total}"
    );
}

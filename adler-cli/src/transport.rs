//! HTTP client / browser-backend / proxy / session plumbing.
//!
//! Builds the configured [`Client`] from CLI flags, the optional
//! browser backend (`--browser-backend`), and parses the two TOML
//! config files (`--proxy-pool`, `--sessions`). Each piece is
//! independent of any other CLI subcommand; main.rs just calls
//! `build_client(&cli)` once per run.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adler_core::browser::{BrowserbaseBackend, BrowserbaseConfig, LocalBackend, LocalConfig};
use adler_core::{BrowserBackend, Client, EgressSpec, Session, SessionStore};
use anyhow::{Context as _, Result};

use crate::{BrowserBackendChoice, Cli};

const REDDIT_SESSION_NAME: &str = "reddit";
const REDDIT_TOKEN_URL: &str = "https://www.reddit.com/api/v1/access_token";
const REDDIT_CLIENT_ID_ENV: &str = "REDDIT_CLIENT_ID";
const REDDIT_CLIENT_SECRET_ENV: &str = "REDDIT_CLIENT_SECRET";
const REDDIT_USER_AGENT: &str = "adler-osint/0.12 (+https://github.com/commit3296/adler)";

pub(crate) const USER_AGENT_POOL: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
];

pub(crate) const TOR_PROXY: &str = "socks5://127.0.0.1:9050";

/// A proxy-pool config file (`--proxy-pool`): `[[egress]]` entries
/// describing the geo / IP-type-tagged proxies that sites can require
/// via their access policy.
#[derive(serde::Deserialize)]
struct ProxyPoolFile {
    #[serde(default)]
    egress: Vec<EgressSpec>,
}

/// Parse the TOML body of a proxy-pool file into egress specs.
pub(crate) fn parse_proxy_pool(text: &str) -> Result<Vec<EgressSpec>> {
    let parsed: ProxyPoolFile = toml::from_str(text).context("parsing proxy pool TOML")?;
    Ok(parsed.egress)
}

/// Read and parse a `--proxy-pool` file.
fn load_proxy_pool(path: &Path) -> Result<Vec<EgressSpec>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading proxy pool {}", path.display()))?;
    parse_proxy_pool(&text).with_context(|| format!("in proxy pool {}", path.display()))
}

/// Parse the TOML body of a `--sessions` file: each top-level `[name]`
/// table is a set of HTTP headers for that named session.
pub(crate) fn parse_sessions(text: &str) -> Result<SessionStore> {
    let raw: std::collections::HashMap<String, std::collections::BTreeMap<String, String>> =
        toml::from_str(text).context("parsing sessions TOML")?;
    let mut store = SessionStore::new();
    for (name, headers) in raw {
        store.insert(name, Session::from_headers(headers));
    }
    Ok(store)
}

/// Read and parse a `--sessions` file.
fn load_sessions(path: &Path) -> Result<SessionStore> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading sessions {}", path.display()))?;
    parse_sessions(&text).with_context(|| format!("in sessions {}", path.display()))
}

pub(crate) async fn build_client(cli: &Cli) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(cli.timeout))
        .max_retries(cli.max_retries);
    if let Some(rps) = cli.max_rps {
        builder = builder.max_rps(rps);
    }
    let proxy_for_browser: Option<String> = if cli.tor {
        builder = builder.proxy(TOR_PROXY);
        Some(TOR_PROXY.to_owned())
    } else if let Some(url) = &cli.proxy {
        builder = builder.proxy(url.clone());
        Some(url.clone())
    } else {
        None
    };
    if let Some(path) = &cli.proxy_pool {
        builder = builder.egress_pool(load_proxy_pool(path)?);
    }
    let mut sessions = if let Some(path) = &cli.sessions {
        load_sessions(path)?
    } else {
        SessionStore::new()
    };
    maybe_insert_reddit_oauth_session(&mut sessions).await?;
    if !sessions.is_empty() {
        builder = builder.sessions(sessions);
    }
    if cli.rotate_ua {
        builder =
            builder.rotate_user_agents(USER_AGENT_POOL.iter().map(|s| (*s).to_owned()).collect());
    }

    if let Some(backend) = build_browser_backend(cli, proxy_for_browser.as_deref()).await? {
        builder = builder.browser(backend).browser_budget(cli.browser_budget);
    }

    builder = builder.escalation_budget(cli.escalation_budget);
    if cli.no_escalation {
        builder = builder.disable_escalation();
    }

    builder
        // --correlate needs profile fields, so it implies enrichment.
        .enrich(cli.enrich || cli.correlate)
        .respect_robots(cli.respect_robots)
        .build()
        .context("building HTTP client")
}

async fn maybe_insert_reddit_oauth_session(store: &mut SessionStore) -> Result<()> {
    if store.contains(REDDIT_SESSION_NAME) {
        return Ok(());
    }

    let client_id = std::env::var(REDDIT_CLIENT_ID_ENV).ok();
    let client_secret = std::env::var(REDDIT_CLIENT_SECRET_ENV).ok();
    let (Some(client_id), Some(client_secret)) = (client_id, client_secret) else {
        if std::env::var_os(REDDIT_CLIENT_ID_ENV).is_some()
            || std::env::var_os(REDDIT_CLIENT_SECRET_ENV).is_some()
        {
            anyhow::bail!(
                "{REDDIT_CLIENT_ID_ENV} and {REDDIT_CLIENT_SECRET_ENV} must both be set for Reddit OAuth"
            );
        }
        return Ok(());
    };

    let token = fetch_reddit_app_token(&client_id, &client_secret, REDDIT_TOKEN_URL).await?;
    store.insert(REDDIT_SESSION_NAME, reddit_session_from_token(&token));
    Ok(())
}

async fn fetch_reddit_app_token(
    client_id: &str,
    client_secret: &str,
    token_url: &str,
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
        #[serde(default)]
        token_type: String,
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(REDDIT_USER_AGENT)
        .build()
        .context("building Reddit OAuth client")?;
    let resp = http
        .post(token_url)
        .basic_auth(client_id, Some(client_secret))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("grant_type=client_credentials")
        .send()
        .await
        .context("requesting Reddit OAuth token")?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Reddit OAuth token request failed with HTTP {status}");
    }

    let token: TokenResponse = resp.json().await.context("parsing Reddit OAuth token")?;
    if token.access_token.trim().is_empty() {
        anyhow::bail!("Reddit OAuth token response did not include access_token");
    }
    if !token.token_type.is_empty() && !token.token_type.eq_ignore_ascii_case("bearer") {
        anyhow::bail!(
            "Reddit OAuth token response used unsupported token_type {:?}",
            token.token_type
        );
    }
    Ok(token.access_token)
}

fn reddit_session_from_token(token: &str) -> Session {
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
    Session::from_headers(headers)
}

/// Construct the browser backend selected by CLI flags, or `None` when no
/// backend should be used. `--no-browser` short-circuits to `None` even if
/// a backend is configured.
async fn build_browser_backend(
    cli: &Cli,
    proxy_url: Option<&str>,
) -> Result<Option<Arc<dyn BrowserBackend>>> {
    if cli.no_browser {
        return Ok(None);
    }
    // `--flaresolverr <URL>` is a shorthand for `--browser-backend
    // flaresolverr` plus the endpoint — if the user passed the URL
    // but not the explicit backend choice, promote it.
    let effective =
        if cli.flaresolverr.is_some() && cli.browser_backend == BrowserBackendChoice::None {
            BrowserBackendChoice::Flaresolverr
        } else {
            cli.browser_backend
        };
    match effective {
        BrowserBackendChoice::None => Ok(None),
        BrowserBackendChoice::Local => {
            let cfg = LocalConfig {
                proxy_url: proxy_url.map(str::to_owned),
            };
            let backend = LocalBackend::launch(cfg)
                .await
                .context("launching local browser backend (is Chrome installed?)")?;
            eprintln!(
                "adler: launched local Chrome for bot-protected sites (budget: {})",
                cli.browser_budget
            );
            Ok(Some(Arc::new(backend) as Arc<dyn BrowserBackend>))
        }
        BrowserBackendChoice::Browserbase => {
            let api_key = std::env::var("ADLER_BROWSERBASE_API_KEY").map_err(|_| {
                anyhow::anyhow!(
                    "--browser-backend browserbase requires ADLER_BROWSERBASE_API_KEY env var"
                )
            })?;
            let project_id = std::env::var("ADLER_BROWSERBASE_PROJECT_ID").map_err(|_| {
                anyhow::anyhow!(
                    "--browser-backend browserbase requires ADLER_BROWSERBASE_PROJECT_ID env var"
                )
            })?;
            let cfg = BrowserbaseConfig {
                api_key: secrecy::SecretString::from(api_key),
                project_id,
            };
            let backend = BrowserbaseBackend::connect(cfg)
                .await
                .context("opening Browserbase session")?;
            // Cost reality check, on stderr so it survives stdout redirects.
            // Stays terse so it doesn't drown the progress bar.
            eprintln!(
                "adler: opened Browserbase session (id={}) — sites tagged bot-protected will route through it, billed per session-minute. Budget: {}.",
                backend.session_id(),
                cli.browser_budget,
            );
            Ok(Some(Arc::new(backend) as Arc<dyn BrowserBackend>))
        }
        BrowserBackendChoice::Flaresolverr => {
            let endpoint = cli
                .flaresolverr
                .clone()
                .or_else(|| std::env::var("ADLER_FLARESOLVERR_URL").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--browser-backend flaresolverr requires --flaresolverr <URL> or ADLER_FLARESOLVERR_URL env var"
                    )
                })?;
            let backend = adler_core::browser::FlareSolverrBackend::new(&endpoint)
                .context("connecting to FlareSolverr")?;
            eprintln!(
                "adler: routing bot-protected sites through FlareSolverr at {endpoint} (budget: {})",
                cli.browser_budget,
            );
            Ok(Some(Arc::new(backend) as Arc<dyn BrowserBackend>))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::EgressKind;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_proxy_pool_toml() {
        let toml = r#"
            [[egress]]
            url = "socks5://pl.example:1080"
            country = "PL"
            kind = "residential"

            [[egress]]
            url = "http://dc.example:8080"
        "#;
        let specs = parse_proxy_pool(toml).expect("parses");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].country.as_ref().unwrap().as_str(), "pl");
        assert!(matches!(specs[0].kind, EgressKind::Residential));
        // Second entry omits country/kind → None + default Datacenter.
        assert!(specs[1].country.is_none());
        assert!(matches!(specs[1].kind, EgressKind::Datacenter));
    }

    #[test]
    fn empty_proxy_pool_toml_is_ok() {
        assert!(parse_proxy_pool("").expect("parses").is_empty());
    }

    #[test]
    fn parses_sessions_toml() {
        let toml = r#"
            [ig]
            Cookie = "sessionid=abc"
            X-CSRF-Token = "tok"

            [reddit]
            Cookie = "reddit_session=xyz"
        "#;
        let store = parse_sessions(toml).expect("parses");
        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
    }

    #[test]
    fn empty_sessions_toml_is_ok() {
        assert!(parse_sessions("").expect("parses").is_empty());
    }

    #[tokio::test]
    async fn fetches_reddit_app_only_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/access_token"))
            .and(header("authorization", "Basic aWQ6c2VjcmV0"))
            .and(body_string_contains("grant_type=client_credentials"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "token_type": "bearer",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let token = fetch_reddit_app_token(
            "id",
            "secret",
            &format!("{}/api/v1/access_token", server.uri()),
        )
        .await
        .expect("token fetch succeeds");

        assert_eq!(token, "tok");
    }

    #[tokio::test]
    async fn reddit_oauth_token_requires_bearer_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "token_type": "mac"
            })))
            .mount(&server)
            .await;

        let err = fetch_reddit_app_token(
            "id",
            "secret",
            &format!("{}/api/v1/access_token", server.uri()),
        )
        .await
        .expect_err("non-bearer token should fail");

        assert!(err.to_string().contains("unsupported token_type"));
    }
}

//! `AdlerMcp` — the [`rmcp::ServerHandler`] that backs the MCP server.
//!
//! Built on the `rmcp` `#[tool_router]` + `#[tool_handler]` macro
//! pattern: tool methods are declared on the inherent `impl`, and the
//! macros wire `list_tools` / `call_tool` automatically. Resource and
//! prompt support land in follow-up commits.

use std::sync::Arc;

use adler_core::Registry;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Json;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::Implementation;
use rmcp::model::InitializeResult;
use rmcp::model::ServerCapabilities;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// MCP server backing Adler's OSINT capabilities.
///
/// Construct via [`AdlerMcp::new`] or [`AdlerMcp::with_registry`] and
/// hand to one of the transport launchers
/// (`adler_mcp::run_stdio`, `run_http` — the latter ships in a
/// follow-up). Cloning is cheap (the registry sits behind an
/// [`Arc`]) — the server passes itself by `Arc` to rmcp's transport
/// drivers.
#[derive(Clone)]
pub struct AdlerMcp {
    registry: Arc<Registry>,
    // The `#[tool_handler]` macro reads `self.tool_router` to dispatch
    // tool calls; the field would otherwise look unused to the
    // compiler.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl AdlerMcp {
    /// Build a server backed by the default embedded registry.
    ///
    /// # Errors
    ///
    /// Returns an error if the embedded registry fails to load
    /// (shouldn't happen on a release build — the registry is
    /// validated at compile time via `include_str!`).
    pub fn new() -> crate::Result<Self> {
        Ok(Self::with_registry(Arc::new(Registry::default_embedded()?)))
    }

    /// Build a server backed by an explicit registry. Useful when the
    /// caller has already loaded a custom `--sites` file or wants the
    /// WMN-merged variant.
    #[must_use]
    pub fn with_registry(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            tool_router: Self::tool_router(),
        }
    }

    /// The shared registry — exposed so transport launchers and
    /// future tools can reach the live site list without re-loading.
    #[must_use]
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }
}

#[tool_router]
impl AdlerMcp {
    /// List sites in the embedded registry, optionally filtered.
    ///
    /// Mirrors the CLI's `--list-sites` flag: respects `tag` /
    /// `exclude_tag` / `include_nsfw`. Returns one entry per matching
    /// enabled site (disabled entries are never surfaced — for a view
    /// of disabled entries with their reasons, the planned
    /// `adler://registry/disabled` resource is the right surface).
    #[tool(
        name = "list_sites",
        description = "List enabled sites in the Adler registry, optionally filtered \
                       by tag / exclude_tag / include_nsfw. Returns name, URL template, \
                       tags, and popularity rank for each match."
    )]
    pub fn list_sites(&self, Parameters(args): Parameters<ListSitesArgs>) -> Json<ListSitesOutput> {
        let sites = self.registry.filter(
            &[],
            &[],
            &args.tag.unwrap_or_default(),
            &args.exclude_tag.unwrap_or_default(),
            args.include_nsfw.unwrap_or(false),
        );
        let entries: Vec<SiteEntry> = sites
            .into_iter()
            .map(|s| SiteEntry {
                name: s.name,
                url: s.url.as_str().to_owned(),
                tags: s.tags,
                popularity: s.popularity,
            })
            .collect();
        let total = entries.len();
        Json(ListSitesOutput {
            total,
            sites: entries,
        })
    }
}

/// Parameters for the `list_sites` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSitesArgs {
    /// Keep only sites carrying at least one of these tags
    /// (case-insensitive). Empty / unset means "no tag filter".
    #[serde(default)]
    pub tag: Option<Vec<String>>,
    /// Drop sites carrying any of these tags. Useful for fast clean
    /// runs (`--exclude-tag bot-protected`).
    #[serde(default)]
    pub exclude_tag: Option<Vec<String>>,
    /// Include `nsfw`-tagged sites in the result. Defaults to
    /// `false`, mirroring Sherlock's opt-in pattern and the CLI's
    /// `--nsfw` flag.
    #[serde(default)]
    pub include_nsfw: Option<bool>,
}

/// Per-site row in the `list_sites` response.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SiteEntry {
    /// Display name.
    pub name: String,
    /// URL template with `{username}` placeholder.
    pub url: String,
    /// Tags attached to this site.
    pub tags: Vec<String>,
    /// Popularity rank (lower = more popular), if set.
    pub popularity: Option<u32>,
}

/// Envelope for the `list_sites` response.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSitesOutput {
    /// Number of sites returned after filtering.
    pub total: usize,
    /// Matching site entries, in registry order.
    pub sites: Vec<SiteEntry>,
}

#[tool_handler]
impl ServerHandler for AdlerMcp {
    fn get_info(&self) -> InitializeResult {
        let mut server_info = Implementation::new("adler-mcp", env!("CARGO_PKG_VERSION"));
        server_info.title = Some("Adler OSINT".to_owned());
        server_info.website_url = Some("https://github.com/commit3296/adler".to_owned());

        let mut result =
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        result.server_info = server_info;
        result.instructions = Some(ADLER_MCP_INSTRUCTIONS.to_owned());
        result
    }
}

const ADLER_MCP_INSTRUCTIONS: &str = concat!(
    "Adler is an OSINT username-search tool — given a username, it probes a curated ",
    "registry of sites for the presence of a matching account. Use the `list_sites` ",
    "tool to browse what's available (filter by tag like 'social' or 'coding', ",
    "exclude with 'bot-protected'). Scan tools (single-username, batch) and ",
    "doctor / history tools land in follow-up versions of this server.\n\n",
    "For ethical use: Adler is for authorised security testing, OSINT research, and ",
    "defensive security work only. The tool detects anti-bot gates but never ",
    "circumvents them.",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs_with_embedded_registry() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        assert!(
            server.registry().len() > 100,
            "registry should be populated"
        );
    }

    #[test]
    fn list_sites_tool_returns_filtered_results() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let args = ListSitesArgs {
            tag: Some(vec!["dev".to_owned()]),
            exclude_tag: None,
            include_nsfw: Some(false),
        };
        let Json(output) = server.list_sites(Parameters(args));
        assert!(output.total > 0, "expected some dev-tagged sites");
        assert!(
            output.sites.iter().any(|s| s.name == "GitHub"),
            "GitHub should be in the dev-tagged set",
        );
        assert_eq!(output.sites.len(), output.total);
    }

    #[test]
    fn server_info_advertises_tools_capability() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, "adler-mcp");
    }
}

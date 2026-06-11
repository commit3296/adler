//! Templated OSINT prompts the MCP server advertises.
//!
//! Each [`PromptSpec`] has a static name, description, declared
//! arguments, and a body with `{placeholder}` slots that
//! [`render_prompt`] fills in from the caller's args map.
//! Substitution is single-pass so a value that looks like
//! `{other_arg}` never expands recursively — preventing cross-slot
//! data smuggling (see the regression test in the module's tests).

/// Argument spec for one prompt template.
pub(super) struct PromptArgSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) required: bool,
}

/// Static spec for one prompt template, including the body text with
/// `{placeholder}` substitution points.
pub(super) struct PromptSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) arguments: &'static [PromptArgSpec],
    /// Body text with `{arg}` placeholders. Substitution is literal —
    /// arguments come from MCP and are quoted into the body verbatim,
    /// so a malicious-looking arg can't open new placeholders.
    pub(super) body: &'static str,
}

pub(super) const PROMPT_SPECS: &[PromptSpec] = &[
    PromptSpec {
        name: "investigate_username",
        description: "Walk the agent through a full OSINT investigation of a single username — pick a \
             scope from the registry, scan, and report Found accounts.",
        arguments: &[
            PromptArgSpec {
                name: "username",
                description: "The username to investigate.",
                required: true,
            },
            PromptArgSpec {
                name: "regions",
                description: "Comma-separated ISO-3166 country codes to prefer (e.g. \"ru,ua\"). \
                              Empty means all regions.",
                required: false,
            },
            PromptArgSpec {
                name: "categories",
                description: "Comma-separated registry tags to scope the scan (e.g. \"social,\
                              coding\"). Empty means every category — uses `adler://registry/\
                              tags` to pick.",
                required: false,
            },
        ],
        body: "\
Please investigate the username `{username}` across Adler's site registry.

Workflow:

1. Read `adler://registry/tags` to see what categories are available, and \
`adler://registry/sites` if you want the full enabled set.
2. Pick a scoped subset: regions = `{regions}`, categories = `{categories}`. \
If both are empty, default to the `social` + `coding` tags. If only regions are \
set, filter via the `region:<cc>` tags Adler attaches to each entry.
3. Call the `scan_username` tool with `username=\"{username}\"` and your filter. \
Subscribe to `notifications/progress` if you want to see per-site results stream.
4. Group the response by verdict (Found / NotFound / Uncertain). For each \
Found account, report the canonical URL, `confidence.label`, \
`confidence.score`, and the strongest `evidence` / `profile_evidence` values \
available. If `identity_clusters` is non-empty, summarize the cluster \
members, cluster confidence, reasons, and whether `uncertain` is true.
5. If any sites came back Uncertain, note them but do not infer existence \
either way — cite their `confidence` reasons and any session/transport \
limitations instead.

Be honest about scope: Adler is for authorised security testing and OSINT \
research. Do not generate or suggest harassment, doxxing, or unauthorised \
surveillance of individuals.\
",
    },
    PromptSpec {
        name: "audit_registry_health",
        description: "Walk the doctor + dedup + disabled-state surface and report what needs \
             maintainer attention (broken signals, stale known_present, importer \
             duplicates).",
        arguments: &[PromptArgSpec {
            name: "focus",
            description: "Optional sub-area: \"known_present\" / \"disabled\" / \"signals\". \
                          Empty means walk all three.",
            required: false,
        }],
        body: "\
Please audit the health of Adler's site registry. Focus area: `{focus}` (empty \
means walk everything).

Workflow:

1. Read `adler://registry/disabled` for the disabled-entry surface. Tally by \
`disabled_reason` prefix (`duplicate of …`, `Honest Limits: …`, \
`doctor: 3+ …`). Flag any entries whose reason looks stale (e.g. a `Honest \
Limits: …` site whose upstream restriction has plausibly lifted).
2. Read `adler://registry/tags` to spot tags with abnormally low or \
abnormally high counts — both are signs of importer-tag drift.
3. For each candidate (1-5 entries that look most-worth-investigating), \
invoke `doctor_check` to confirm the current verdict. Don't run more than \
~5 of these — the doctor takes ~1 second per site.
4. Report your findings as a short table: site name, current state, what \
maintainer action you recommend (re-enable, change reason, file an issue, no \
action). For \"file an issue\", link to \
<https://github.com/commit3296/adler/issues>.

If you find any obvious mistakes (e.g. a Honest-Limits-disabled site that \
clearly works now), state your evidence explicitly so the maintainer can \
verify before flipping the flag.\
",
    },
    PromptSpec {
        name: "correlate_accounts",
        description: "Scan multiple usernames, then look for shared profile signal (name, bio, \
             avatar) across the Found accounts to suggest whether they belong to one person.",
        arguments: &[PromptArgSpec {
            name: "usernames",
            description: "Comma-separated list of usernames to correlate (e.g. \
                          \"alice,alice_dev,a-liddell\").",
            required: true,
        }],
        body: "\
Please correlate the following usernames to see whether they likely belong to \
one person: `{usernames}`.

Workflow:

1. Call `scan_batch` with the comma-split username list. Use a small filter \
(e.g. `tag=[\"social\",\"coding\"]`) so the scan finishes quickly — broad \
sweeps add noise without helping correlation.
2. For each username's Found accounts, use `identity_clusters` first. Then \
inspect `outcomes[].profile_evidence`, `outcomes[].confidence`, and \
`outcomes[].url` for supporting details. Do not scrape presentation text \
when structured evidence is present.
3. Compare across usernames: shared exact name, shared bio fragments, shared \
external links, and shared avatar URLs are signals; shared sites alone are weak. \
Treat any cluster with `uncertain=true` as a candidate, not a hard merge.
4. Report a confidence verdict (Strong / Plausible / Weak / Distinct) per \
pair, with the evidence that supports it.

Be honest about uncertainty: matching usernames across sites does NOT prove \
they're the same person. Multiple people use common handles. State your \
limits explicitly.\
",
    },
];

/// Substitute `{name}` placeholders in a prompt's body with the
/// argument values supplied by the client. Missing required args
/// produce `invalid_params`; missing optional args render as empty
/// strings so the prompt still parses cleanly.
pub(super) fn render_prompt(
    spec: &PromptSpec,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, rmcp::ErrorData> {
    // Resolve every declared argument up-front: enforces the "required"
    // contract before any output is produced, and lets the single-pass
    // substitution below treat the lookup table as authoritative — so an
    // arg's value that happens to contain `{other_arg}` is never
    // expanded recursively.
    let mut resolved: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::with_capacity(spec.arguments.len());
    for arg_spec in spec.arguments {
        let value = match args.get(arg_spec.name) {
            Some(v) => v.as_str().unwrap_or(""),
            None if arg_spec.required => {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "prompt {:?} requires argument {:?}",
                        spec.name, arg_spec.name
                    ),
                    None,
                ));
            }
            None => "",
        };
        resolved.insert(arg_spec.name, value);
    }

    // Single-pass scan: every `{ident}` whose ident matches a declared
    // argument is replaced with the resolved value. Unknown placeholders
    // (or stray braces inside literal prose) pass through verbatim so
    // body authors can mention `{foo}` without it disappearing.
    let body = spec.body;
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('}') {
            let ident = &after_open[..close];
            if let Some(value) = resolved.get(ident) {
                out.push_str(value);
                rest = &after_open[close + 1..];
                continue;
            }
        }
        // No matching placeholder — emit the `{` literally and keep
        // scanning from the next character.
        out.push('{');
        rest = after_open;
    }
    out.push_str(rest);
    Ok(out)
}

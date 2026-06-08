# Contributing to Adler

## Quick start

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All four must pass; CI enforces them on every push.

If you touch the embedded web UI, run its local checks too:

```bash
cd adler-server/web
npm run typecheck
npm run build
npm run smoke
```

`npm run smoke` starts the Vite dev server through Playwright and covers
the home/catalog path plus a cold `#/scan/:id` snapshot route. Use it for
SPA routing, store/action, and component changes; use the Rust workspace
checks above for server/API changes.

## Quality bar

- **No `unwrap` / `expect` / `panic!` in non-test code** unless the invariant
  is locally proven and commented (the only exceptions today are two
  `const … = match NonZeroUsize::new(16) { … None => unreachable!() }`).
- Errors flow as `Result`; transient per-site failures become
  `MatchKind::Uncertain`, never aborts.
- Clippy runs with `pedantic` + `nursery`. A handful of noisy lints are
  allowed workspace-wide with justification in `Cargo.toml`.
- New public items in `adler-core` need `///` docs;
  `RUSTDOCFLAGS=-D warnings cargo doc` must stay clean.

## Commit messages

We follow [Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<optional scope>): <imperative subject>

<optional body explaining WHY, not what>
```

Common `<type>`: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`,
`perf`, `revert`. `<scope>` is the touched area, e.g. `tui`, `core`, `cli`,
`registry`.

- Subject in the imperative ("add X", not "added X") and ≤ 72 chars.
- Body explains the *why* (a hidden constraint, an incident, a tradeoff).
  The diff already shows the *what*.
- One concern per commit — don't bundle a refactor with a feature.
- No emoji. No `Co-Authored-By` trailer unless the contributor explicitly
  wants it.

Examples:

```
feat(tui): jump to next found account with n / N
fix(core): treat 429 with no Retry-After as a transient ban
docs(contributing): document commit-message conventions
```

## Adding or fixing a site

Growing the registry is the highest-leverage contribution. The flow is meant
to be short: describe the site, let `--doctor` verify it, open a PR.

The registry (`adler-core/data/sites.json`) was originally **generated**
from Sherlock by `scripts/import_sherlock.py`, with two later tranches
merged in via `scripts/import_maigret.py` and (under CC BY-SA, behind
an opt-in load path) `scripts/import_whatsmyname.py`. The importers
aren't run on a schedule today — the file is hand-curated between
imports, and the doctor's `--apply` flow (see *Maintaining existing
sites* below) is the daily-driver for live edits. If you *do* re-run an
importer, hand-edits to entries the importer also touches would be
overwritten unless they're captured in the importer's `OVERRIDES` /
`KNOWN_BROKEN` maps — so durable surgery (a deliberate disable, a
hand-authored signature) should land there too.

### Anatomy of a site

A site is a `name`, a `url` template with `{username}`, and a list of
`signals`:

```json
{
  "name": "Example",
  "url": "https://example.com/{username}",
  "signals": [
    { "kind": "status_found", "codes": [200] },
    { "kind": "status_not_found", "codes": [404] }
  ],
  "known_present": "a-real-account"
}
```

Signal kinds: `status_found` / `status_not_found` (by HTTP code),
`body_present` / `body_absent` (substring in the body), `redirect_absent`
(substring in the final URL). Aggregation is **negative-priority** — any
NotFound vote wins over Found, and with no votes the verdict is `Uncertain`.
The field-level shape is described by [`docs/sites.schema.json`](docs/sites.schema.json)
(point your editor at it for autocomplete).

Always set `known_present` to a **real existing account** so the doctor can
verify the positive case.

Optionally add `tags` (e.g. `["social", "region:ru"]`) so users can scan a
relevant subset with `--tag`. The importer seeds a starter set automatically
(`derive_tags`: ccTLD → `region:xx`, plus a curated category map); extend
either the map in `scripts/import_sherlock.py` or the site's `tags` directly.
`adler --list-tags` shows what exists.

### Prefer API/feed endpoints over canonical pages

The single most useful lesson from validating the registry: **a bot-protected
canonical profile page is a bad detection target.** Sites like Instagram,
X/Twitter, and TikTok serve a login wall or JS app to a plain HTTP request
(no browser, no residential IP), so the response looks the same for an
existing and a missing account — Adler can only return `Uncertain`.

The robust entries route around this with a stable machine endpoint:

| Site | URL used | Why it works |
| --- | --- | --- |
| Pinterest | `…/oembed.json?url=…/{username}/` | oEmbed API: 200 vs 404 |
| Medium | `…/feed/@{username}` | RSS feed: present vs error page |
| GitLab | `…/api/v4/users?username={username}` | public API: `[]` when absent |

So when you add a site, look for an oEmbed / RSS / public JSON API / sitemap
endpoint before reaching for the human-facing page. It won't get
Cloudflare-walled and the signal is crisp.

### Scaffold a new site in one command

`adler add-site` does the whole derivation for you — give it the URL
template and an account that exists there, and it prints a ready-to-paste
entry:

```bash
adler --add-site "https://github.com/{username}" --name GitHub torvalds
# → { "name": "GitHub", "url": "...", "signals": [status_found 200,
#      status_not_found 404], "known_present": "torvalds" }
```

It probes the existing account and a random nonsense one, diffs the
responses, and derives the `signals`. Add `--proxy socks5://…` to probe from
a clean IP if your network is blocked. `--name` defaults to the URL host.

### Verify, then open a PR

```bash
adler --doctor --only "Example"            # does detection hold? expect [OK]
adler --doctor --fix --only "Example"      # derive a signature from the diff
```

`--doctor --fix` (and `add-site`) probe the `known_present` user and a random
nonsense user, diff the responses, and print a ready-to-paste signature — the
fastest way to get the `signals` right.

Open a PR (the template walks through the checklist). The `validate-sites`
workflow then runs automatically:

- **hard gates (block the PR):** JSON-Schema validation and the Rust loader's
  invariants (unique names, valid CSS selectors, url placeholder + scheme).
- **advisory:** a `--doctor` pass over just your changed sites, posted to the
  run summary. It runs from a GitHub datacenter IP, so a bot-protected site
  may show `Uncertain` there even when your signature is correct — it informs
  review, it doesn't fail the build.

No live network? Say so in the PR and a maintainer will run the doctor.

## Maintaining existing sites

Once a site is in the registry, the doctor's three `--apply` flows are
the daily-driver for keeping it healthy. Each pairs a `--suggest-*` or
`--fix` discovery step with an atomic JSON rewrite, so the registry
file ends up byte-stable everywhere except the patched fields.

| Symptom (from `--doctor`) | Fix flow |
| --- | --- |
| `known_present "blue" reported NotFound` (placeholder never resolved) | `adler --doctor --suggest-known-present --apply --sites adler-core/data/sites.json --yes` |
| `--enrich <user>` returns empty profile | `adler --doctor --suggest-extract --apply --sites adler-core/data/sites.json --yes` |
| `signal too permissive` *and* the page has distinguishable present/absent shapes | `adler --doctor --fix --apply --sites adler-core/data/sites.json --only "<Site>" --yes` |

`--apply` rewrites through a sibling `*.tmp` so a crash mid-write
leaves the original intact. Each flow prints a per-site `- old + new`
diff and prompts once; `--yes` skips the prompt for CI batch repair.
`--apply` requires `--sites <writable>` because the embedded registry
isn't patchable in place. A bare `--apply` without any of `--fix` /
`--suggest-known-present` / `--suggest-extract` errors out — `--apply`
is the verb, the suggestion flag is the noun.

The discovery side has one important guard: `--suggest-known-present`
first probes a random nonsense user and aborts with `None` if *that*
already returns `Found`. So a site whose signal is structurally too
permissive (returns `Found` for arbitrary strings — a brand-name
catch-all, a sign-up funnel, …) won't yield a false-positive
"discovery". When `--apply` skips a site for this reason it says so.

### Disabling instead of deleting

Some entries can't be fixed from any IP (login-walled, JS-SPA-only,
OAuth-only API). Park them rather than delete — disabled entries stay
discoverable for re-enablement if upstream ever changes, and the
`disabled_reason` tells the next maintainer at-a-glance which bucket
they're in:

```json
{
  "name": "Reddit",
  "url": "https://www.reddit.com/user/{username}",
  "signals": [{ "kind": "status_found", "codes": [200] }],
  "disabled": true,
  "disabled_reason": "Honest Limits: 403s anonymous requests since the 2023 API restriction"
}
```

The Rust loader keys all gating decisions off `disabled: bool` — the
reason is annotation for humans and `scripts/doctor_aggregate.py`.
The conventions in use:

- `duplicate of <canonical>` — surplus entry from importer overlap
  (Sherlock/Maigret/WhatsMyName each named the same site differently).
  Pick the canonical by metadata-completeness, disable the rest.
- `Honest Limits: <one-line reason>` — structurally unscrapable for
  anonymous OSINT. See the *Honest limits* section in `PLAN.md` for
  the canonical list.
- `doctor: 3+ consecutive structural failures` — written by the
  nightly aggregator's auto-PR (`scripts/doctor_aggregate.py`) when a
  site has failed `--doctor` for *N* nights running.

Anything else gets a free-form one-liner; the field is unconstrained
in the schema.

### URL + signals uniqueness

`Registry::validate` rejects any registry where two enabled entries
share both a URL template *and* a signal set. The check fires at load
time, so the `validate-sites` workflow blocks the PR before CI even
gets to the doctor step:

```
duplicate (URL, signals) among enabled sites: "Hub Code" and "HubCode"
both back "https://example.com/{username}" with identical signals.
Mark one `disabled: true` with `disabled_reason: "duplicate of Hub Code"`
(or, if the two entries are supposed to disambiguate via different
markers, give each a distinct signal set).
```

Two shapes are legitimate and the rule lets them through:

- **Disabled sibling at the same URL** — the dedup pattern. The
  canonical is enabled; the surplus carries `disabled: true` +
  `disabled_reason: "duplicate of <canonical>"`. The rule only counts
  enabled entries.
- **Same URL, distinct signals** — intentional aliases. WordPress.com
  (Public/Private/Deleted) all hit one API endpoint and disambiguate
  via different `body_present` markers; the doctor reads three
  independent verdicts on one URL.

If an importer run ever re-introduces a known duplicate, the
registry won't load and the PR fails fast. Two ways to handle the
re-introduction durably:

- Add the matching entry to `scripts/import_sherlock.py`'s
  `OVERRIDES` map (or the equivalent in `import_maigret.py` /
  `import_whatsmyname.py`) with `{"disabled": True,
  "disabled_reason": "duplicate of <canonical>"}` so the next
  importer run lands the same disabled state.
- Or add the name to `KNOWN_BROKEN` so the importer skips it entirely.

## MCP server (`adler-mcp`)

The `adler-mcp` workspace crate exposes Adler's OSINT surface to AI
assistants over the
[Model Context Protocol](https://modelcontextprotocol.io/). The CLI
launches it via `adler --mcp` (stdio) or `adler --mcp-http <addr>`
(HTTP+SSE). Built on the official Rust SDK (`rmcp`); the daily-driver
pattern is the `#[tool_router]` + `#[tool_handler]` macro pair that
auto-generates `list_tools` / `call_tool` from per-method `#[tool]`
annotations.

Surface conventions:

- **Tools** (callable actions) live in the `#[tool_router] impl
  AdlerMcp` block. New tools should reuse the existing `ScanFilter`
  shape via `#[serde(flatten)]` when they take a filter — keeps the
  agent's mental model consistent. The canonical cross-surface filter
  contract is documented in README's *Filter contract* table; MCP uses
  `include_nsfw` where CLI/web say `--nsfw` / `nsfw`, and `top` is a
  popularity-rank ceiling (`popularity <= top`), not a result-count
  limit.
- **Resources** (browsable data) live in the `ServerHandler` impl on
  the same struct. Static URIs go in the `STATIC_RESOURCES` table;
  parameterized URIs (e.g. `adler://scans/{id}`) go in
  `list_resource_templates`. Resource readers must defend against path
  traversal — see `render_scan_by_id`'s `/` / `\` rejection.
- **Prompts** (templated OSINT workflows) live in the
  `PROMPT_SPECS` table. The body is `&'static str` with `{name}`
  placeholders and a small `render_prompt` substitutor. Substitution
  is literal — argument values are quoted into the body verbatim, so
  no placeholder-injection from a malicious arg.

Tests:

- Unit tests in `adler-mcp/src/server.rs` cover the tools / resources
  / prompts logic against the embedded registry — no network required.
- End-to-end stdio tests in `adler-cli/tests/cli.rs` (`mcp_stdio_*`)
  spawn the real binary and drive a full JSON-RPC handshake +
  `tools/call` / `tools/list` / `resources/list` / `prompts/list`.
  These exercise the same code path Claude Desktop would.
- Hand-runnable probes in `adler-mcp/examples/probe_stdio.py` and
  `probe_http.py` walk the *entire* advertised surface (every tool
  including live `scan_username`, every resource, every prompt with
  argument substitution) and double as reference implementations of
  a minimal MCP client in each transport. See
  `adler-mcp/examples/README.md` for usage.

Ethical line: every new tool that scans / probes must respect the
project's bound — authorised security testing / OSINT research /
defensive work only. The MCP `instructions` block and the
`investigate_username` prompt both restate this, so an agent's first
peek at the server names what's in-scope.

## Versioning & releases

Adler follows **SemVer**. While we're pre-1.0, the version reads as
`0.<minor>.<patch>`:

| Change | Bump | Conventional Commit prefix |
| --- | --- | --- |
| Breaking public API (trait signatures, removed `pub` items, behaviour change of a flag's default) | `0.X.0` | `feat!:` / `fix!:` (or any type with `!`) |
| Additive: new site in the registry, new CLI flag, new backend, new `pub` item | `0.x.Y` | `feat:` |
| Bugfix | `0.x.Y` | `fix:` |
| Anything else (clippy, docs, CI, refactor, tests) | none | `chore:` / `docs:` / `ci:` / `refactor:` / `test:` |

After 1.0.0 we switch to standard SemVer (MAJOR for breaks).

Both crates (`adler-core`, `adler-cli`) share one workspace version
and ship together.

### Release pipeline

Releases are fully automated by
[release-plz](https://release-plz.dev). Contributors write
[Conventional Commits](https://www.conventionalcommits.org); the
machinery handles the rest:

1. **You** open a normal PR with a `feat:` / `fix:` (or breaking
   variant) and merge it into `main`.
2. **release-plz** sees the new commit and opens (or updates) a
   *Release PR* titled `chore(release): release vX.Y.Z`. The PR
   contains exactly two kinds of change: the workspace version bump
   in `Cargo.toml` + `Cargo.lock`, and a fresh section appended to
   `CHANGELOG.md` derived from the qualifying commits.
3. **A maintainer** reviews the Release PR — most importantly: is the
   bump right, and does the changelog accurately describe what
   shipped? Edits to the changelog text on the PR are preserved when
   release-plz refreshes the branch.
4. **Merging the Release PR** triggers, in order:
   - `cargo publish -p adler-core` then `cargo publish -p adler-cli`
     (release-plz, in dependency order);
   - a `vX.Y.Z` git tag and a GitHub Release named the same, with
     the new changelog section in the body;
   - `.github/workflows/release.yml`, which builds the five platform
     binaries and attaches them to the Release (`cargo binstall`
     fetches from there).

What this means in practice:

- **Never bump versions or write CHANGELOG.md by hand.** Both are
  generated; manual edits will be overwritten on the next refresh.
  Want a release? Land a `feat:` / `fix:` and merge the Release PR.
- **Commit subjects become changelog bullets.** Write them for end
  users, not for the diff. `fix(browser): handle CDP reconnect`
  beats `fix bug`.
- **Scope is optional but recommended.** `(browser)`, `(registry)`,
  `(cli)`, `(ci)` are the common ones — they group the changelog
  visibly.
- **Breaking changes use the `!`.** `feat!: replace BrowserBackend
  trait with two methods`. release-plz then bumps `0.X.0` instead
  of `0.x.Y` and tags the entry `[breaking]`.

### Secrets the pipeline needs

One-time repo setup (Settings → Secrets → Actions):

- `CARGO_REGISTRY_TOKEN` — a crates.io API token with
  `publish-update` scope for both `adler-core` and `adler-cli`.
- `RELEASE_PLZ_APP_ID` — the numeric *App ID* of the
  [`adler-release`](https://github.com/settings/apps/adler-release)
  GitHub App installed on this repo.
- `RELEASE_PLZ_APP_PRIVATE_KEY` — the PEM-format private key
  generated for that same App.

The reason for the GitHub App (vs a Personal Access Token) is that
events authored by the default `GITHUB_TOKEN` do **not** trigger
downstream workflows — release-plz creating the GitHub Release with
`GITHUB_TOKEN` would silently fail to fire `release.yml` and ship no
binaries. A GitHub App is authored by its bot identity, so its events
cascade normally, and its installation tokens are short-lived (~1 h)
which is materially safer than a long-lived PAT.

The App needs only three repository permissions: **Contents: Read &
Write**, **Pull requests: Read & Write**, **Workflows: Read & Write**.

### Emergency manual release

If automation is broken and you need to ship now, the fallback is
the old four-step dance: `cargo set-version`, hand-edit
`CHANGELOG.md`, `chore(release): vX.Y.Z` commit, `git tag vX.Y.Z`,
push. The `release.yml` workflow now triggers on `release.published`
rather than tag-push, so you also need a manual `gh release create
vX.Y.Z` before the binaries will build. Prefer fixing the
automation.

## Ethics

Adler detects anti-bot gates but never circumvents them, and is for
authorized use only. See [SECURITY.md](SECURITY.md) and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

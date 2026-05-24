# Contributing to Adler

## Quick start

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All four must pass; CI enforces them on every push.

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

The registry (`adler-core/data/sites.json`) is **generated** from Sherlock by
`scripts/import_sherlock.py`. Hand-edits to the JSON are overwritten on
re-import, so durable changes go in the importer's `OVERRIDES` /
`KNOWN_BROKEN` maps (and you regenerate the JSON).

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

Plus Settings → Actions → General → *Workflow permissions* must
allow GitHub Actions to **create and approve pull requests** (so
release-plz can open the Release PR).

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

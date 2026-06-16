# Security operations

This page records the repository-level controls used to keep Adler's
release and CI supply chain auditable.

## GitHub Security

- Dependabot vulnerability alerts and automated security updates are enabled.
- Secret scanning and push protection are enabled.
- CodeQL, cargo-audit, OpenSSF Scorecard, and the main CI workflow run from
  GitHub Actions.
- The `main` branch is protected through PR review, required checks, and
  CODEOWNERS review.

## Pinned inputs

GitHub Actions and Docker base images are intentionally pinned to immutable
commit SHAs or image digests. This makes Scorecard findings actionable and
prevents a tag move from changing CI or release behavior without review.

When updating pins:

1. Prefer a Dependabot PR when one is available.
2. For manual action updates, resolve the new tag to a commit SHA with
   `git ls-remote https://github.com/<owner>/<action>.git <tag>`.
3. For Docker images, resolve the new digest from the registry and keep the
   human-readable tag before `@sha256:...`.
4. Keep comments next to pins current enough for reviewers to understand which
   stream is being tracked.

## Fuzzing

The `fuzz/` crate is intentionally kept outside the workspace. Normal
`cargo test --workspace` runs stay fast and deterministic, while
`cargo +nightly fuzz build` and the `fuzz.yml` workflow exercise registry JSON
parsing, site validation, and username permutation invariants.

Fuzz targets must not perform network I/O, read secrets, or depend on external
state.

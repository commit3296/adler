# Security policy

[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/13082/badge)](https://www.bestpractices.dev/projects/13082)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/commit3296/adler/badge)](https://scorecard.dev/viewer/?uri=github.com/commit3296/adler)

## Reporting a vulnerability

Please report security issues privately rather than opening a public issue.
Email the maintainers (see the repository's contact / `Cargo.toml` authors)
or use GitHub's private "Report a vulnerability" advisory flow. Include steps
to reproduce and the affected version. We aim to acknowledge reports within a
few days.

## Supported versions

Pre-1.0: only the latest release receives fixes.

## Design principle: detect, never circumvent

Adler classifies anti-bot responses (HTTP 429, Cloudflare interstitials,
captcha pages) as `Uncertain` and reports them. It does **not** attempt to
solve captchas, bypass Cloudflare, rotate through residential proxy networks
to evade blocks, or otherwise defeat a site's access controls. Pull requests
that add such circumvention to `adler-core` will be declined: surfacing that
a site is gating access is in scope; defeating the gate is not.

The same boundary applies to scope and intensity: Adler ships conservative
per-host rate limiting and a global `--max-rps` cap, and supports
`--respect-robots`. It is built for checking accounts you are authorized to
investigate, not for mass-targeting or denial of service.

See the "Ethics & responsible use" section of the README for acceptable-use
guidance.

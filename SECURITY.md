# Security Policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security problems.

Report privately through one of:

- **GitHub private vulnerability reporting** — use *Report a vulnerability* on
  the repository's [Security tab](https://github.com/joaoh82/rustunnel/security/advisories/new).
- **Email** — [joaoh82@gmail.com](mailto:joaoh82@gmail.com) with the subject
  prefix `[rustunnel security]`.

Include the affected component (`rustunnel-server`, `rustunnel-client`,
`rustunnel-mcp`, dashboard), the commit or release you tested, reproduction
steps or a proof of concept, and your assessment of the impact.

## What to expect

- Acknowledgement within **3 business days**.
- A fix or mitigation plan within **30 days** for confirmed issues; faster for
  anything remotely exploitable against the hosted `rustunnel.com` edges.
- We ask that you give us that window before disclosing publicly.
- Reporters are credited in the fix's pull request and in the release notes
  unless they ask to remain anonymous.

## Supported versions

Only the latest release on the `main` branch receives security fixes. Self-hosted
operators should track tagged releases and upgrade promptly.

## Scope

In scope: the code in this repository and the hosted edges at
`*.edge.rustunnel.com`. Out of scope: denial of service by traffic volume,
findings that require a compromised operator machine or `server.toml`, and
issues in third-party dependencies that have no rustunnel-specific impact
(report those upstream).

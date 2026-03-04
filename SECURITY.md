# Security Policy

## Supported Versions

Security fixes are currently targeted at:

- the latest `main` branch
- the most recent tagged release

Older pre-release snapshots and unpublished local branches should be treated as unsupported.

## Reporting A Vulnerability

Please do not open a public GitHub issue for suspected vulnerabilities.

Instead:

1. Email `muthmann@physik.uni-bielefeld.de` with the subject line `AugurRS security report`.
2. Include the affected version or commit, platform details, impact, and clear reproduction steps.
3. Attach logs, traces, or proof-of-concept material only when needed to explain the issue.
4. If private GitHub Security Advisories are enabled for the repository, you may use that channel instead of email.

## Response Expectations

- An initial acknowledgement target is within 5 business days.
- A follow-up status update target is within 10 business days after acknowledgement.
- Coordinated disclosure is preferred once a fix or mitigation is available.

## Scope Notes

Please report vulnerabilities in:

- USB transport or protocol handling
- recording, parsing, or configuration loading behavior
- release artifacts or packaging scripts
- GitHub Actions workflows that affect distributed artifacts

General hardware reliability issues, unsupported platform failures, and normal camera misconfiguration are better handled through the regular issue tracker.

# Security Policy

## Supported versions

Security fixes are provided for the latest release in the `0.1.x` series.

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |
| Earlier versions | No |

## Reporting a vulnerability

Please report suspected vulnerabilities privately through [GitHub's private vulnerability reporting page](https://github.com/luw2007/resume/security/advisories/new). Include:

- the affected version and environment;
- a clear description of the issue and its impact;
- steps or a minimal proof of concept that reproduces it; and
- any suggested remediation, if known.

Do not open a public issue or disclose the vulnerability publicly before a fix is available. The maintainer will acknowledge the report, investigate it, and coordinate remediation and disclosure with the reporter. If the report is accepted, a fix and security advisory will be prepared before public disclosure; if it is declined, the maintainer will explain why through the private report.

Because `resume` reads local coding-agent session data and launches native agent CLIs, avoid including real session transcripts, credentials, tokens, or other unnecessary secrets in a report. Use redacted or synthetic reproduction data whenever possible.

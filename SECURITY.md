# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** using GitHub's
[private vulnerability reporting](https://github.com/mericstam/pwsh-autoupdate/security/advisories/new)
for this repository. Do not open a public issue for a security report.

Include, where possible:

- the affected version (`pwsh-autoupdate --version`) and OS,
- a description of the issue and its impact,
- steps to reproduce (a minimal command line is ideal).

You can expect an acknowledgement within a few days. Once a fix is available it
will be released and the advisory published.

## Scope

`pwsh-autoupdate` detects how PowerShell was installed and updates (or, when
absent, installs) it through the owning package manager. It shells out to host
package managers and reads release metadata over HTTPS from official sources.
Reports of particular interest include:

- command construction that could run an unintended program or arguments,
- parsing of upstream release metadata that could be abused,
- privilege handling (the tool must never self-elevate).

## Supported versions

The latest released version receives security fixes.

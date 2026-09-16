# Security

HMux grants terminal access to the connected Home machine. Deploy it only for
trusted users. Separate web accounts isolate authentication and tab profiles, not
operating-system files, shells or provider credentials. It is not a multi-tenant
sandbox or a public terminal hosting service.

Use HTTPS, trusted SSH host keys and unique account credentials. Keep TOTP enabled
unless you understand the implications. Do not commit production configuration,
credential/session files, signing keys or terminal transcripts.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on the repository Security tab:
https://github.com/codemoo/hmux/security/advisories/new

Do not put credentials, exploitable details or real terminal content in a public
issue. Include the affected revision, a synthetic reproduction and expected impact.
Security fixes target the current main branch; there is no promised long-term
support window for older builds.

See [the threat model](docs/THREAT_MODEL.md) for trust boundaries and limitations.

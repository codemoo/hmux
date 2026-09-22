# Documentation

HMux keeps Codex, Claude and shells on one host, accessed exclusively through web/PWA.

| Document | Purpose |
| --- | --- |
| [Product/security specification](../CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md) | Requirements and boundaries |
| [Architecture](ARCHITECTURE.md) | Gateway, Home services and session lifecycle |
| [Web HMux](WEB.md) | Web/PWA behavior, authentication, input, usage and deployment |
| [Operations](OPERATIONS.md) | Build, install and administer gateway/Home |
| [Contribution guide](../CONTRIBUTING.md) / [Tests](../tests/README.md) | Development and verification |
| [Security](../SECURITY.md) / [Threat model](THREAT_MODEL.md) | Trust boundaries and private reports |
| [Releasing](RELEASING.md) | Source publication and web deployment |
| [Migration](MIGRATION.md) / [Rollback](ROLLBACK.md) | Existing Home config and safe upgrades |
| [Recovery](RECOVERY.md) | Verified tmux/provider reboot recovery |
| [Codex workflows](CODEX_WORKFLOWS.md) | Optional Home hooks and retention |
| [iOS input](IOS_INPUT.md) | Accepted input/Paste behavior and device limitations |
| [Troubleshooting](TROUBLESHOOTING.md) | Connection, quota, upload and browser diagnostics |
| [Validation](VALIDATION.md) | Dated evidence and deployment status |

`archive/` contains superseded browser experiments, not current instructions or
permission to deploy. Retired desktop/terminal UI history remains in Git history.

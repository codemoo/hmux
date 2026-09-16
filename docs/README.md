# Documentation

HMux keeps Codex, Claude and shells on one host with remote web/PWA access.
The macOS app is optional; the standalone terminal UI is archived.

| Document | Purpose |
| --- | --- |
| [Product/security specification](../CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md) | Requirements and boundaries |
| [Architecture](ARCHITECTURE.md) | Ownership, protocols and session lifecycle |
| [Contribution guide](../CONTRIBUTING.md) / [Security](../SECURITY.md) | Development checks and private vulnerability reports |
| [Release procedure](RELEASING.md) | Source publishing and native binary readiness |
| [Operations](OPERATIONS.md) | Setup, builds and administration |
| [Flexoki Dark theme](THEME.md) | Color system, terminal palette and icon |
| [Archived terminal UI](../archive/terminal/README.md) | Legacy code and compatibility boundary |
| [Desktop app](../macos/HMux/README.md) | Workspace, conversation, cswap usage, build/install and native development |
| [CLI compatibility](CLI_COMPATIBILITY.md) | fzf, framed tabs and CLI keys |
| [External provisioning](../scripts/EXTERNAL_PROVISIONING_README.md) | Adding a remote Mac |
| [Codex workflows](CODEX_WORKFLOWS.md) | Optional hooks and retention |
| [Termius](TERMIUS_INTEGRATION.md) | Mobile setup and validation levels |
| [Android engine review](ANDROID_ENGINE_REVIEW.md) | Candidate assessment and prototype gates; not an implemented app |
| [Web HMux](WEB.md) | Desktop/mobile web and PWA installation, browser support, authentication and deployment |
| [iOS terminal input](IOS_INPUT.md) | Accepted Korean input/Paste, keyboard-dismissal limitation and diagnostic procedure |
| [Threat model](THREAT_MODEL.md) | Defenses and residual risks |
| [Troubleshooting](TROUBLESHOOTING.md) | Native/remote diagnostics, PWA installation and mobile clipboard regressions |
| [Migration](MIGRATION.md) / [Rollback](ROLLBACK.md) | Controlled changes and recovery |
| [Validation](VALIDATION.md) | Reproducible gates and current evidence |

`archive/` contains historical validation, rejected input/clipboard experiments
and an unimplemented DMZ logind proposal. These are not current instructions or
deployment permission. Current platform observations and accepted limitations
belong in `WEB.md`/`IOS_INPUT.md`; deployment/check evidence belongs in `VALIDATION.md`.

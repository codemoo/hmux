# Documentation

HMux's purpose is **"low memory, web terminal for ai agents"**. It keeps Codex,
Claude and shells on one host, accessed exclusively through web/PWA.

| Document | Purpose |
| --- | --- |
| [Product/security specification](../CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md) | Requirements and boundaries |
| [Architecture](ARCHITECTURE.md) | Gateway, Home services and session lifecycle |
| [Web HMux](WEB.md) | Web/PWA behavior, authentication and continuity |
| [Browser input](BROWSER_INPUT.md) / [iOS input](IOS_INPUT.md) | Input/Paste contracts and device limitations |
| [Providers](PROVIDERS.md) / [Push](PUSH.md) | CLI setup, usage sources and notifications |
| [Operations](OPERATIONS.md) | Build, install and administer gateway/Home |
| [Contribution guide](../CONTRIBUTING.md) / [Tests](../tests/README.md) | Development and verification |
| [Security](../SECURITY.md) / [Threat model](THREAT_MODEL.md) | Trust boundaries and private reports |
| [Releasing](RELEASING.md) | Source publication and web deployment |
| [Migration](MIGRATION.md) / [Rollback](ROLLBACK.md) | Existing Home config and safe upgrades |
| [Recovery](RECOVERY.md) | Verified tmux/provider reboot recovery |
| [Codex workflows](CODEX_WORKFLOWS.md) | Optional Home hooks and retention |
| [Troubleshooting](TROUBLESHOOTING.md) | Connection, quota, upload and browser diagnostics |
| [Validation](VALIDATION.md) | Dated evidence and deployment status |

[Archive](archive/README.md) preserves dated validation evidence and superseded
browser experiments; it is not current operational instruction or permission to deploy. Retired desktop/terminal UI history remains in Git history.

## Reading order and ownership

Start at the root README, then choose a reference here. Contributors read
`AGENTS.md` and `CONTRIBUTING.md`; browser changes additionally read `web/AGENTS.md`.
`VALIDATION.md` alone summarizes the maintained deployment, completed checks and
remaining validation gaps. Operations owns installation/service/diagnostic commands;
Web owns current product behavior; scoped references expand their subjects.
Do not append deployment diaries to behavior or operating instructions.

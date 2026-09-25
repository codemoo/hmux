# Operations

Gateway runs on Linux with systemd; Home runs on macOS or Linux. Install both on
one Linux machine or split them across hosts. Browsers/PWAs are the only clients. Use your own credentials and domain.
Detailed gateway/authentication settings are in [WEB.md](WEB.md).

## Build and install

Install the pinned Rust toolchain, Node.js 22+, Python 3, tmux and the desired provider CLIs.
Contributor checks additionally require ShellCheck and jq; see [CONTRIBUTING.md](../CONTRIBUTING.md).
Authenticate the providers as the Home user, then build:

```sh
make build
```

The host bundle is `dist/web-<platform>/`, such as `dist/web-linux-amd64/` or
`dist/web-darwin-arm64/`; it contains the Gateway, Home connector, helper, web assets,
notices and a SHA-256 manifest. Build does not deploy. To build a non-host supported
target, set `HMUX_RUST_TARGETS` only after installing its Rust target, linker and any
required platform SDK. The target's binary is not portable across platforms.

Start the unified installer from the bundle built for the machine running it:

```sh
./dist/web-darwin-arm64/hmux-web install
```

The interactive guide first offers **English** (the default) or **한국어**.
Pass `--lang en` or `--lang ko` to skip that choice. `HMUX_LANG=en|ko` provides
an environment default; an explicit flag takes precedence. These language options
also apply to `install-home`, `install-gateway` and `init-web`, and are forwarded
through Gateway privilege elevation and SSH installation. Other OS/provider tools
retain their own output language. CLI flags, machine output, paths and
configuration values are not translated.

Choose a role and an installation target. Only the selected role is installed:

| Role | Target | Setup performed |
| --- | --- | --- |
| Gateway + Home | Linux/systemd | Gateway first, then Home as the current SSH/local user |
| Gateway | Linux/systemd | HTTPS entry point and unprivileged Gateway service |
| Home | macOS or Linux | Host configuration, native binaries and optional user service |

The target can be this machine or an SSH server. Run Home/combined installation
as the account that owns tmux and provider authentication, without `sudo`; the
installer elevates only Gateway provisioning. Gateway-only installation may run
as root. Provider CLIs, authentication and workspaces remain on the Home host.

### Gateway and HTTPS

Gateway setup asks for a domain and one of two HTTPS configurations:

- **Managed Nginx + Let's Encrypt:** checks the domain inputs, optionally installs
  missing Nginx/Certbot packages with apt, configures only HMux's include, obtains
  a certificate and enables renewal. The domain must resolve to the server and
  ports 80/443 must be reachable. ACME terms acceptance is explicit.
- **Existing HTTPS proxy:** installs the Gateway on `127.0.0.1:8088` without
  changing the proxy or installing proxy packages. Configure the existing proxy
  for HTTP and WebSocket upgrades, including `/connect`, using
  [the reverse-proxy requirements](#linuxnginx-deployment).

The Gateway uses a dedicated unprivileged `hmux-web` service account. Releases
live below `/opt/hmux-web/releases/`, with an atomic `current` link; private state
is in `/var/lib/hmux-web`. The installer owns marked HMux service/proxy files,
backs up replaced files privately, and refuses unmanaged collisions. Failed
activation restores its prior configuration and release pointer; it never rolls
back credentials or user data. Packages, issued certificates and newly created
private setup files are retained for recovery.

**Create the first account in the browser.** Open the configured HTTPS site and
enter the one-time setup token from the private
`/var/lib/hmux-web/credentials.json.bootstrap` file (read it with administrative
access, for example `sudo cat /var/lib/hmux-web/credentials.json.bootstrap`).
Choose a username/password and set up TOTP, or skip TOTP and enable it later in
account settings. The setup token is distinct from the Home connector token;
it cannot be used to log in after account creation. It is never in a URL, command
argument or application log. Existing installations keep their accounts and do
not reopen setup. A partial or unsafe private state fails closed.

For same-host installation, Home receives the Gateway address/token automatically.
For split hosts, Gateway setup writes a private connection JSON; transfer only
that file over a trusted channel. It contains connector authority, never a login
password or TOTP secret. Select Home and supply the file:

```sh
./dist/web-darwin-arm64/hmux-web install --local --role home \
  --connection-file /PRIVATE/home-connection.json
```

### Remote installation

The installer uses the system SSH/SCP client with host-key verification and agent
forwarding disabled. First verify the server's key with your usual SSH client;
configure nondefault ports, identities or jump hosts in SSH config. Use an alias
or `user@hostname` in the installer:

```sh
./dist/web-darwin-arm64/hmux-web install --role gateway --remote server-alias
```

It checks the target OS/CPU, selects a matching extracted bundle beside the local
bundle (for example `web-linux-amd64`) or asks for its local path, verifies its
manifest, uploads to a private temporary directory, verifies it again and runs
setup over an SSH terminal. Cross-platform installation needs the target's built
bundle; it does not run a macOS executable on Linux or silently download binaries.
The remote server needs SSH/SFTP and SHA-256 verification (`sha256sum` or `shasum`).

Gateway connection files are retrieved into private local storage after remote
setup. They can be passed to a separate local or remote Home installation. Remote
Home setup runs under the SSH account and follows the same opt-in user-service
rules. On macOS the user's GUI login domain must be available for a LaunchAgent;
Linux needs a usable systemd user manager. SSH does not change login/sleep policy.
Successful transfer staging is removed. On failure the installer reports the
retained private staging directory for inspection/retry; it does not remove
installed roles or original tmux/provider work.

### Home configuration

Home asks for the workspace (default `~/.hmux`), checks local tmux/provider tools,
and offers automatic startup with a default of **No**. With a connection file,
there is no need to type the address/token again. Otherwise enter an existing
Gateway HTTPS address and a private connector token path, or connect later.
Token contents are never printed. The Home-only guide does not provision HTTPS,
install provider CLIs or authenticate providers.

Existing workspace paths are preserved unless `--workspace-dir` is explicit.
`install` and `install-home --guided` require a terminal. For scripts use
`install-home` or `install-gateway` with explicit flags; see each command's
`--help`. Automatic startup registration is reported separately from a verified
Gateway connection. `NO_COLOR` and `TERM=dumb` select plain terminal output.

The native installer preflights both source/target binaries, rejects symlinks and
unsafe ownership/permissions, configures Home through `hmux-agent setup-home`, and
uses a recoverable atomic replacement transaction with private timestamped backups.
The optional Python
compatibility wrapper accepts an explicit `--source-dir`; its default selects the
host bundle and is not required for new installations.

On a new interactive installation, it asks for the new-session base directory;
press Enter for `~/.hmux`. Noninteractive new installations use that default.
To choose a path explicitly:

```sh
./dist/web-darwin-arm64/hmux-web install-home --workspace-dir "$HOME/projects"
```

Reinstalling without `--workspace-dir` preserves every existing profile directory,
including custom paths. An explicit `--workspace-dir` updates all profile bases
with a timestamped inventory backup, preserving other settings and legacy fields.
Existing `client.toml` remains authoritative when `home.toml` is absent; `state_dir`
and provider command arguments are preserved. The installer starts a service only
with `--enable-service` or an explicit Yes in the guide; otherwise running
connectors are untouched. `--guided --enable-service` retains the explicit service
options and, when none are supplied, the existing running-connector adoption flow.
`--binaries-only` skips configuration entirely and cannot enable a service.
`--source-dir`, `--bin-dir` and `--config-dir` select alternative locations.
Coordinate connector restart separately; restore binary backup bytes with executable
mode if rolling back. Configuration errors stop before normal binary activation; fix
the configuration error and rerun.

Each inventory profile's `default_directory` is the **base**, not the session CWD.
A new session creates a private child directory derived from the entered name:
letters (including Korean), numbers, hyphens and underscores remain; spaces and
other punctuation become hyphens. Empty names use the profile ID. Names are bounded
for filesystem/tmux limits. Existing folders, files and symlinks are never reused;
a short random suffix distinguishes a repeated name. The tmux name combines the
actual child folder, a bounded profile ID and a random suffix. Two creates with
the same input always create separate workspaces and sessions. `create --dry-run`
only validates and prints the proposed folder slug; it reserves nothing.

For example, `My Project` with the Codex profile creates `<base>/My-Project` and
`My-Project-codex-<suffix>` in tmux. The base is created on first use if missing.
Codex and Claude profiles whose command starts with `codex` or `claude` return to
an interactive Home shell in the same directory after normal exit, failure or
Ctrl+C. The same lifecycle applies to recovered provider sessions; ordinary shell
profiles retain their normal exit behavior. Existing running panes are not rewritten.
A regular recovery checkpoint clears the resume reference after a provider exits;
until that checkpoint is saved, the preceding recovery snapshot may still resume it.

Commands are argument arrays selected by profile ID. Existing configuration remains
compatible; follow [MIGRATION.md](MIGRATION.md) before replacing it. Keep the same
`state_dir` to retain aliases, hidden state, workflows and recovery.

## Start the gateway and connector

Use the unified installer above to configure Gateway and Home. For manual service
administration, see [gateway deployment](#linuxnginx-deployment). Copy only the
connector token/connection file to private Home storage over a trusted channel.

```sh
~/.local/bin/hmux-web connect --url wss://YOUR_HOST/connect --token-file /PRIVATE/connector.token
```

The connector remains running while web access is needed. For automatic startup
and restart, install the native Home user service below. Recovery runs before the
first catalog publication. Stopping a connector disconnects web views without
ending the original tmux/provider processes.

## Automatic Home startup (macOS and Linux)

Run as the account that owns the tmux sessions and provider CLI authentication,
from a terminal where those CLIs work. Do not use `sudo` for service installation.
After building the Home binaries, an existing foreground connector can be migrated
with one installation command:

```sh
./dist/web-darwin-arm64/hmux-web install-home --enable-service
```

It installs the binaries, preserves existing Home/workspace configuration, then
adopts the **sole connector owned by this user**. Adoption reads its existing URL,
token-file path and config arguments, rechecks PID/arguments/start time, sends
SIGTERM only to that exact connector, and starts the managed replacement. A changed,
ambiguous or relative-path command is refused; no process group is signalled.
For an already installed service-capable binary, the equivalent command is:

```sh
~/.local/bin/hmux-web service install --from-running
```

For a new Home with configured profiles and an existing private connector token:

```sh
~/.local/bin/hmux-web service install \
  --url wss://YOUR_HOST/connect --token-file /PRIVATE/connector.token
```

`--config /PRIVATE/home.toml` selects an explicit existing Home config. Otherwise
installation pins the existing `home.toml`, falling back to `client.toml`; missing
configuration is an error. The installer also accepts `--enable-service --url ...`
`--token-file ...` for this setup. A Linux host uses the `hmux-web` binary in its
built `dist/web-linux-<arch>/` bundle for `service install`.
The service command installs its executable at `~/.local/bin/hmux-web` by default
(`--binary /ABSOLUTE/bin/hmux-web` overrides it; the installer preserves `--bin-dir`) and backs up
previous binary/service files before replacement. It does not regenerate credentials
or alter inventory workspace paths. Private token bytes are never put in the unit.

The service captures the installation terminal's absolute PATH, including Homebrew
and version-manager directories. With `--from-running`, it instead preserves the
verified connector's allowlisted environment, even when the installer runs from a
different terminal. HOME, SHELL, locale and explicitly configured provider/XDG paths
are allowlisted; USER and LOGNAME are derived from the current OS account.
Arbitrary environment variables, API keys and temporary SSH-agent sockets are not
copied. Provider authentication should use the
existing persistent provider configuration. After moving/upgrading a versioned
Node or CLI path, stop the service and reinstall with explicit URL/token/config
from a terminal with the corrected PATH; `--from-running` preserves the old process
environment. `/usr/bin/env -i` clears the manager's environment and execs the connector without a resident
wrapper. The connector itself reconnects after network outages; the OS manager
restarts an exited process with a ten-second delay.

| Platform | Registration | Automatic startup boundary |
| --- | --- | --- |
| macOS | `~/Library/LaunchAgents/io.github.codemoo.hmux.home.plist` | Starts after GUI login, independent of Terminal; the Mac must be awake |
| Linux | `~/.config/systemd/user/hmux-home.service` (honors `XDG_CONFIG_HOME`) | Starts with the user's systemd manager; requires systemd |

On Linux, an administrator can run `sudo loginctl enable-linger USERNAME` if the
Home should start at boot and remain available after logout. Check with
`loginctl show-user USERNAME -p Linger`. HMux does not change lingering, FileVault,
automatic login or sleep settings. A macOS LaunchAgent does not run before login
and cannot make a sleeping Mac reachable.

```sh
~/.local/bin/hmux-web service status
~/.local/bin/hmux-web service restart
~/.local/bin/hmux-web service stop
~/.local/bin/hmux-web service start
~/.local/bin/hmux-web service uninstall
```

`stop` disables automatic startup until `start` enables it again. `uninstall`
removes the installed service definition while retaining timestamped backups,
binaries, private state, credentials and workspaces. macOS uses `AbandonProcessGroup`; Linux uses
`KillMode=process`, so service management does not terminate original tmux/provider
processes. OS logout/shutdown policies can still end user processes.

`status` reports manager/process state, not proof of a live gateway connection.
The Rust connector writes fixed lifecycle states and Home operation diagnostics to
`<state_dir>/home-service.log`, with one rotated `.1` file; each is limited to
1 MiB and mode 0600. `Connected` establishes the WSS connection, not catalog or
browser readiness. `home stage=catalog operation=none reason=published` records
the first successful catalog publication for a connection. Check fresh Gateway
state and a successful operation as well.
Logs contain no terminal output, tokens or raw network errors. Failure categories
describe the observed failure, not proof of its underlying network cause.
`home stage=view-cleanup operation=none reason=quarantined` means cleanup of an
owned disposable view could not be confirmed. That view's capacity stays reserved
until the Home process exits; other views and requests keep working. For an open
terminal on a live connection, Home sends `view-cleanup-failed` in its exit. Diagnose the tmux
command failure and inspect disposable-view ownership before a controlled Home
restart; restarting alone does not remove leftover views. Never remove original
tmux/provider sessions to recover view capacity.
Usage collection runs inside Home, not a separate helper daemon. Its bounded
diagnostics use `stage=usage`: `published` means the first valid snapshot pair
was assembled (not that both providers supplied quota), `encoding` withdraws
invalid data while collection continues, and `recovered` marks the next valid
pair. `unavailable` marks a collector startup/run failure. These records contain
no account names, tokens, raw provider errors or usage values. Inspect provider
status in the usage panel separately from collector health.
One connector can hold each state directory's private process-lifetime lock; stop
an older manual connector before starting a service, or use the verified adoption
command. Do not delete an active lock file to bypass the singleton.

## Administration

`hmux-agent` provides `setup-home`, `doctor`, `catalog`, `recovery`, `workflow`, `workflow-hook`,
`workflow-report`, `conversation`, `workspace`, `create`, `alias-set`, `hidden-set`,
`terminate`, `metadata-migrate` and `version`. These are headless administration
operations, not an alternate user interface. Destructive termination requires
both `--confirmed` and `--created-at`; ordinary browser tab closing never terminates work.
Optional workflow hooks use [CODEX_WORKFLOWS.md](CODEX_WORKFLOWS.md).

Keep credential/session/profile/push stores and Home state private and outside
release directories. Inspect bounded frontend diagnostics through Settings;
never include tokens, transcripts or production topology in public reports.

### Gateway transport diagnostics

The gateway writes a private `credentials-file.transport.log` beside its credentials
file. The log retains at most 1 MiB plus one 1 MiB previous file, with mode 0600.
It records process-local connection numbers, Home connect/disconnect, request/open
duration and fixed failure categories. HTTP action failures add `stage=action-failed`
and `http_status`, separating Home offline, admission busy, remote operation,
transport/request, invalid response, workspace and deadline failures. A remote
operation error is not classified as malformed protocol. Pair Gateway records with
Home diagnostics and browser request status to identify which boundary failed.

Logs omit tokens, addresses, account/session identifiers, arbitrary operation strings, terminal
content and arbitrary error text. Correlate UTC timestamps with browser diagnostics.
Browser request cancellation does not cancel an in-flight shared transport write;
queue waits honor caller cancellation and have a 5-second bound, while admitted
writes have their own 5-second bound. Neither a log entry nor a socket handshake
alone establishes that an authenticated browser terminal is usable.

### macOS service-manager access errors

Run Home installation/update commands in the host account's normal Terminal.
An agent's restricted command runner may not have the same launchd access, even
when the user is logged in. Failure to query `gui/UID` does not by itself prove
that the Mac is logged out. Domain availability checks discard the service listing
and use its exit status, avoiding failures caused by a large GUI-domain listing.
Agent approval timeouts are controlled by the agent runtime, not HMux; HMux does
not disable those controls or introduce a privileged updater to bypass them.

### Catalog collection cadence

Initial recovery synchronization remains mandatory before catalog publication.
Resume identity checks do not scan transcript model/state information. Host metrics
and recovery checkpoints run as shared bounded workers, not per-browser collectors.
Catalog reads use the latest completed metrics sample with its original timestamp
and atomically committed recovery mapping.

On macOS, CPU sampling uses the second, one-second interval reading from the
built-in `iostat` CPU-only report instead of enumerating processes with `top`.
Metric commands share a three-second budget; a failed CPU or GPU command omits
that field without skipping filesystem statistics. Disk uses `statfs` in the same
single admitted blocking worker. Failed fields do not reuse older observations.

Transient tmux catalog or metadata failures retry after five seconds without
terminating the connected peer or its live views. Retries retain the last published
catalog timestamp; repeated failure can expire the Gateway's 40-second freshness
lease. Failed reads never publish an empty success. Catalog, completion and recovery
inspection share at most one of two scan permits, retaining interactive capacity;
completion is enqueued after catalog annotation releases its permit.

### Latency diagnostics

Gateway request completion records include an allowlisted `operation`, total
`duration_ms` and `send_ms` (shared-writer queue plus frame transmission). The
remaining time includes transport and Home processing; it is not a pure network
measurement. Home records `stage=action` or `stage=catalog`, fixed `reason` and
`duration_ms`. Categories distinguish admission pressure, query timeout/failure,
metadata/parse/worker errors, cancellation and recovery. Successful actions of at
least 500 ms are recorded as slow. Repeated identical catalog failures and
consecutive slow catalog samples are suppressed until their state changes.

These logs use one bounded 64-record nonblocking queue per process; saturated logs
report dropped records. There is no per-tab logger or added resident process.
Operation names are allowlisted; identifiers, paths, payloads and raw error messages
are excluded. Historical Go log-stage names in archived reports are not current
Rust instrumentation.

## Build and local provisioning

```sh
make check
make integration
make build
```

`make build` packages web assets and the native host bundle. The deployed server
needs only its bundle binary, assets, notices, manifest and private configuration;
it does not need a Rust toolchain. `HMUX_RUST_TARGETS` requires the selected targets'
linkers and SDKs. `npm audit --prefix web` checks frontend dependencies; it is not a
full security guarantee.

For manual provisioning, prepare private first-login setup with:

```sh
hmux-web init-web --credentials /PRIVATE/credentials.json --token-file /PRIVATE/connector.token
```

This prepares distinct private connector and one-time setup tokens, without asking
for a password or TOTP in the terminal. Start the Gateway with these paths, then
finish account setup in the browser. The legacy `init` command still supports
interactive terminal enrollment when explicitly chosen. Never publish credential,
connector or bootstrap files.


## Linux/Nginx deployment

Use `deploy/web/hmux-web.service` for the dedicated unprivileged `hmux-web` user.
Place immutable releases under `/opt/hmux-web/releases/`, then atomically update
`/opt/hmux-web/current`; retain the old release for rollback. Private runtime files
live in `/var/lib/hmux-web` (0700 directory, 0600 files, service-user owned).
`/etc/hmux-web/runtime.env` defines `HMUX_WEB_ORIGIN=https://YOUR_HOST`.

Render `deploy/web/nginx.conf.example` with the public hostname and local certificate
directory. Install it as a separate site, keeping timestamped backups of prior HMux
site/service files. Do not replace global Nginx configuration. First expose only
`/.well-known/acme-challenge/` on HTTP and return 503 elsewhere, then issue:

```sh
sudo certbot certonly --webroot -w /var/www/hmux-acme -d YOUR_HOST
```

Enable the HTTPS site only after successful issuance and `nginx -t`; reload Nginx,
not unrelated applications. Retain the HTTP ACME location for renewals. A Certbot
deploy hook should run `nginx -t` and reload Nginx when this certificate renews.
The native gateway refuses public bind addresses and cleartext public origins.

Systemd restricts writable paths, capabilities, devices and Home-directory access,
with a 256 MiB gateway memory limit. Do not run the gateway as root. Gateway logs
contain startup/error categories, not terminal bytes, prompts or credentials.

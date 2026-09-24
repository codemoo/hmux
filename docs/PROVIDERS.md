# Provider setup and usage

Scoped reference for the [web/PWA interface](WEB.md). Repository and web input
contracts remain authoritative; device evidence is distinct from synthetic checks.

## Usage after connecting

The embedded usage collector re-reads CLI credentials once a minute. When an API
key is saved or cleared, or a connect/update job finishes, the Home connector
restarts its usage collector, which reads the credentials immediately; the new
login is reflected in about a second instead of up to a minute.

## Usage source fallback

Usage preferences default to the pooled sources (`cswap`, `codex-lb`). When the
selected pooled source is unavailable on this Home (non-`ok` state without a
valid measurement) and the CLI source has a valid measurement, the footer and
usage dialog show the CLI source instead and label it as such. A working pooled
source is always used as chosen.

## AI provider setup

Settings → AI 연결 lists Codex, Claude Code and Gemini on Home: installed version,
connection state (`account`, `api-key` or none) and whether the new-session menu
already launches the CLI. Status comes from each CLI's own files and commands
(`codex login status`, `claude auth status --json`, `~/.claude/settings.json`,
`~/.gemini/.env`, `~/.gemini/oauth_creds.json`), each bounded by a five-second
timeout.

- **연결하기** runs `crates/hmux-home/src/provider_setup.sh connect` for that provider in a
  private tmux server (`tmux -L hmux-setup`, session `connect-<provider>`), so it
  never appears in the session list. It installs the CLI when missing and then
  starts the CLI's own login flow: `codex login --device-auth`, `claude auth login`
  and `gemini` with Google login preselected in `~/.gemini/settings.json` (only
  when no other non-key method is configured). HMux does not implement or proxy
  OAuth. Settings polls the job once per second and shows its state, an "open
  login page" button, a Codex device code, and an input that relays a pasted
  authorization code with `tmux send-keys -l`. After input, raw pane lines are
  withheld so an echoed authorization code cannot return in the browser log.
  Login URLs are offered only for
  https URLs on known login hosts printed after the login phase starts; pasted
  codes must be 1–2048 printable ASCII bytes. The job ends on the script's exit
  marker. Gemini completes only when a usable OAuth credential differs from the
  one recorded when that login job began; an empty, corrupt, expired or
  pre-existing stale file is never treated as success. Completed jobs are closed.
  Reopening Settings reattaches to a job still running on Home; 취소 kills it.
  Progress is read from a private state file (`~/.local/state/hmux-setup`), not
  the pane, because CLIs such as Gemini clear the screen. After a successful
  Claude login, and when a Claude key is saved, HMux records Claude Code's
  first-run state in `~/.claude.json` (`hasCompletedOnboarding`,
  `lastOnboardingVersion`, and the key's approval suffix) so the first session
  opens ready instead of repeating onboarding and login.
- **Install/update** (`setup.sh update`) installs only under `~/.local` and never
  uses sudo: Codex from the latest `openai/codex` GitHub release binary, Claude
  Code from `claude.ai/install.sh`, Gemini from npm `@google/gemini-cli`. When
  Node.js 20+ is missing, Gemini first installs the latest Node.js 22 tarball to
  `~/.local/share/hmux/node` after verifying `SHASUMS256.txt`. Saving an API key
  for a provider that is not installed installs it first.
- **API keys** are sent once to Home and written into the CLI's own storage:
  `codex login --with-api-key` (stdin, never argv), `env.ANTHROPIC_API_KEY` in
  `~/.claude/settings.json`, and `GEMINI_API_KEY` in `~/.gemini/.env`. Other
  settings are preserved; an unparsable Claude settings file is left untouched.
  Previous files are kept as timestamped `*.hmux-backup-*` copies (0600). Keys
  are validated to `[A-Za-z0-9._-]{16,512}`, cleared from the input after submit
  and never returned; responses carry only a `…abcd` suffix hint. Clearing a Codex
  key logs out only an API-key login, never a ChatGPT account. Saving a Gemini key
  also selects `gemini-api-key` auth. Gemini reads `~/.gemini/.env` only in
  folders the user has trusted in Gemini's first-run prompt.
- **Launch profiles**: after an explicit install, login or API-key save succeeds,
  an installed CLI with no launch profile is appended to the Home inventory after
  a timestamped backup. It inherits the configured workspace base (`~/.hmux` for
  a new install, or the administrator's chosen path); status checks are read-only
  and concurrent additions are serialized. Existing profiles are never rewritten.
  Connected providers show **시작**, which creates and opens a session with that
  profile.

The empty workspace shows a provider card. Until one provider is connected it
reads "AI 연결이 필요합니다" and opens Settings directly on AI 연결 ("나중에" hides
it on that device). Once a provider is connected it offers "<provider> 시작"
buttons instead. The card is refreshed once per login, when Settings closes and
when the last tab closes.

Provider credentials are Home-wide: every web account on this Home uses the same
CLI logins and keys. The feature requires the Home role. Gemini tabs are ordinary
sessions; provider-specific recovery, usage and conversation reading remain
Codex/Claude only.

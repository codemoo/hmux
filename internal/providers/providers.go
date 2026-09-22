// Package providers inspects and configures the Codex, Claude Code and Gemini
// CLIs for the Home user. Credentials stay in each CLI's own Home-owned files;
// nothing here returns a stored key to callers except a short suffix hint.
package providers

import (
	"bytes"
	"context"
	"crypto/sha256"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/safeexec"
)

//go:embed setup.sh
var setupScript string

const (
	AuthNone    = "none"
	AuthAccount = "account"
	AuthAPIKey  = "api-key"
)

type Provider struct {
	ID      string
	Label   string
	Command string
	// KeyName is the environment variable the CLI reads for API-key auth.
	KeyName string
}

var Known = []Provider{
	{ID: "codex", Label: "Codex", Command: "codex", KeyName: "OPENAI_API_KEY"},
	{ID: "claude", Label: "Claude Code", Command: "claude", KeyName: "ANTHROPIC_API_KEY"},
	{ID: "gemini", Label: "Gemini", Command: "gemini", KeyName: "GEMINI_API_KEY"},
}

func Lookup(id string) (Provider, bool) {
	for _, p := range Known {
		if p.ID == id {
			return p, true
		}
	}
	return Provider{}, false
}

type Status struct {
	ID        string `json:"id"`
	Label     string `json:"label"`
	Installed bool   `json:"installed"`
	Version   string `json:"version,omitempty"`
	Auth      string `json:"auth"`
	KeyHint   string `json:"key_hint,omitempty"`
}

// Env describes the Home user environment. Tests replace every field.
type Env struct {
	Home string
	// Path is prepended with ~/.local/bin when running provider commands.
	Path    string
	Timeout time.Duration
	// SystemDirs overrides the fallback directories when non-nil (tests).
	SystemDirs []string
}

func DefaultEnv() (Env, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return Env{}, err
	}
	return Env{Home: home, Path: os.Getenv("PATH"), Timeout: 5 * time.Second}, nil
}

// systemDirs mirrors the fallback directories used when launching profiles.
var systemDirs = []string{"/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"}

func (e Env) binDir() string { return filepath.Join(e.Home, ".local", "bin") }

func (e Env) searchPath() string {
	parts := []string{e.binDir()}
	if e.Path != "" {
		parts = append(parts, e.Path)
	}
	if e.SystemDirs != nil {
		parts = append(parts, e.SystemDirs...)
	} else {
		parts = append(parts, systemDirs...)
	}
	return strings.Join(parts, string(os.PathListSeparator))
}

// Executable finds a provider CLI the same way new tmux sessions will.
func (e Env) Executable(name string) (string, bool) {
	for _, dir := range filepath.SplitList(e.searchPath()) {
		if dir == "" {
			continue
		}
		path := filepath.Join(dir, name)
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path, true
		}
	}
	return "", false
}

func (e Env) command(ctx context.Context, path string, args ...string) *exec.Cmd {
	cmd := exec.CommandContext(ctx, path, args...)
	cmd.Dir = e.Home
	cmd.Env = append(os.Environ(), "HOME="+e.Home, "PATH="+e.searchPath(), "NO_COLOR=1", "NO_BROWSER=true")
	cmd.Stdin = nil
	return cmd
}

func (e Env) output(ctx context.Context, path string, args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, e.Timeout)
	defer cancel()
	return safeexec.Output(e.command(ctx, path, args...), 64*1024)
}

func (e Env) combinedOutput(ctx context.Context, path string, args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, e.Timeout)
	defer cancel()
	var out bytes.Buffer
	cmd := e.command(ctx, path, args...)
	cmd.Stdout = &out
	cmd.Stderr = &out
	err := cmd.Run()
	if out.Len() > 64*1024 {
		out.Truncate(64 * 1024)
	}
	return out.Bytes(), err
}

// Statuses reports every known provider. Checks run concurrently and each is
// bounded by Env.Timeout so one slow CLI cannot stall the others.
func Statuses(ctx context.Context, env Env) []Status {
	out := make([]Status, len(Known))
	var wg sync.WaitGroup
	for i, p := range Known {
		wg.Add(1)
		go func() {
			defer wg.Done()
			out[i] = status(ctx, env, p)
		}()
	}
	wg.Wait()
	return out
}

func status(ctx context.Context, env Env, p Provider) Status {
	s := Status{ID: p.ID, Label: p.Label, Auth: AuthNone}
	path, ok := env.Executable(p.Command)
	s.Installed = ok
	if ok {
		if raw, err := env.output(ctx, path, "--version"); err == nil {
			s.Version = cleanVersion(raw)
		}
	}
	switch p.ID {
	case "codex":
		if ok {
			// `codex login status` prints to stderr and exits non-zero when logged out.
			if raw, err := env.combinedOutput(ctx, path, "login", "status"); err == nil {
				text := string(raw)
				if strings.Contains(text, "API key") {
					s.Auth = AuthAPIKey
					s.KeyHint = codexKeyHint(env)
				} else if strings.Contains(text, "Logged in") {
					s.Auth = AuthAccount
				}
			}
		}
	case "claude":
		if key := claudeSettingsKey(env); key != "" {
			s.Auth, s.KeyHint = AuthAPIKey, Hint(key)
		} else if ok {
			if raw, err := env.output(ctx, path, "auth", "status", "--json"); err == nil {
				var v struct {
					LoggedIn   bool   `json:"loggedIn"`
					AuthMethod string `json:"authMethod"`
				}
				if json.Unmarshal(bytes.TrimSpace(raw), &v) == nil && v.LoggedIn {
					if v.AuthMethod == "api_key" {
						s.Auth = AuthAPIKey
					} else {
						s.Auth = AuthAccount
					}
				}
			}
		}
	case "gemini":
		if key := dotenvValue(env.geminiEnvPath(), p.KeyName); key != "" {
			s.Auth, s.KeyHint = AuthAPIKey, Hint(key)
		} else if _, ok := geminiOAuthFingerprint(env, time.Now()); ok {
			s.Auth = AuthAccount
		}
	}
	return s
}

var versionPattern = regexp.MustCompile(`[0-9]+\.[0-9]+(\.[0-9]+)?([-+.][0-9A-Za-z.]+)?`)

func cleanVersion(raw []byte) string {
	line, _, _ := strings.Cut(strings.TrimSpace(string(raw)), "\n")
	if v := versionPattern.FindString(line); v != "" {
		return v
	}
	return ""
}

var keyPattern = regexp.MustCompile(`^[A-Za-z0-9._-]{16,512}$`)

// ValidKey accepts the character set used by OpenAI, Anthropic and Google keys.
// It excludes whitespace, quotes and '=' so a key can never alter .env or JSON.
func ValidKey(key string) bool { return keyPattern.MatchString(key) }

func Hint(key string) string {
	if len(key) < 8 {
		return ""
	}
	return "…" + key[len(key)-4:]
}

// SetKey stores (or, with an empty key, removes) the provider's API key.
func SetKey(ctx context.Context, env Env, id, key string) error {
	p, ok := Lookup(id)
	if !ok {
		return errors.New("unknown provider")
	}
	if key != "" && !ValidKey(key) {
		return errors.New("API 키 형식이 올바르지 않습니다")
	}
	switch id {
	case "codex":
		path, ok := env.Executable("codex")
		if !ok {
			return errors.New("Codex를 먼저 설치하세요")
		}
		if key == "" {
			// Logging out would also drop a ChatGPT login; only clear API keys.
			if s := status(ctx, env, p); s.Auth != AuthAPIKey {
				return nil
			}
			_, err := env.output(ctx, path, "logout")
			return err
		}
		ctx, cancel := context.WithTimeout(ctx, env.Timeout)
		defer cancel()
		cmd := env.command(ctx, path, "login", "--with-api-key")
		cmd.Stdin = strings.NewReader(key + "\n")
		cmd.Stdout, cmd.Stderr = io.Discard, io.Discard
		if err := cmd.Run(); err != nil {
			return fmt.Errorf("codex login: %w", err)
		}
		return nil
	case "claude":
		if err := setClaudeKey(env, p.KeyName, key); err != nil || key == "" {
			return err
		}
		return markClaudeReady(ctx, env, key)
	case "gemini":
		if err := setDotenv(env.geminiEnvPath(), p.KeyName, key); err != nil || key == "" {
			return err
		}
		// A saved key should be used even if Google login was selected before.
		return setGeminiAuth(env, "gemini-api-key", func(string) bool { return true })
	}
	return errors.New("unknown provider")
}

func codexKeyHint(env Env) string {
	raw, err := readSmall(filepath.Join(env.Home, ".codex", "auth.json"))
	if err != nil {
		return ""
	}
	var v struct {
		Key string `json:"OPENAI_API_KEY"`
	}
	if json.Unmarshal(raw, &v) != nil {
		return ""
	}
	return Hint(v.Key)
}

func (e Env) claudeSettingsPath() string {
	return filepath.Join(e.Home, ".claude", "settings.json")
}

func (e Env) geminiEnvPath() string { return filepath.Join(e.Home, ".gemini", ".env") }

func (e Env) geminiOAuthPath() string {
	return filepath.Join(e.Home, ".gemini", "oauth_creds.json")
}

// geminiOAuthFingerprint accepts only a credential document that can still be
// used: a refresh token, or an access token whose millisecond expiry is in the
// future. Existence alone is not authentication; Gemini can leave an empty,
// corrupt or expired file behind after a failed or revoked login.
func geminiOAuthFingerprint(env Env, now time.Time) (string, bool) {
	raw, err := readSmall(env.geminiOAuthPath())
	if err != nil || len(bytes.TrimSpace(raw)) == 0 {
		return "", false
	}
	var credential struct {
		AccessToken  string `json:"access_token"`
		RefreshToken string `json:"refresh_token"`
		ExpiryDate   int64  `json:"expiry_date"`
	}
	if json.Unmarshal(raw, &credential) != nil {
		return "", false
	}
	usable := validOAuthToken(credential.RefreshToken)
	if !usable && validOAuthToken(credential.AccessToken) {
		usable = credential.ExpiryDate > now.Add(30*time.Second).UnixMilli()
	}
	if !usable {
		return "", false
	}
	// A refresh token identifies the durable login. Access tokens and expiry
	// fields rotate during ordinary refreshes, and JSON formatting can change;
	// neither must look like a new interactive login. An access-only credential
	// still reports account auth, but all such files share one conservative
	// identity so token rotation cannot complete a reconnect job.
	if validOAuthToken(credential.RefreshToken) {
		return fmt.Sprintf("refresh:%x", sha256.Sum256([]byte(credential.RefreshToken))), true
	}
	return "access-only", true
}

func validOAuthToken(value string) bool {
	value = strings.TrimSpace(value)
	return len(value) >= 8 && len(value) <= 16*1024 && strings.IndexFunc(value, func(r rune) bool {
		return r < 0x21 || r == 0x7f
	}) < 0
}

func claudeSettingsKey(env Env) string {
	raw, err := readSmall(env.claudeSettingsPath())
	if err != nil {
		return ""
	}
	var v struct {
		Env map[string]any `json:"env"`
	}
	if json.Unmarshal(raw, &v) != nil {
		return ""
	}
	key, _ := v.Env["ANTHROPIC_API_KEY"].(string)
	return key
}

// setClaudeKey edits only env.<name> in ~/.claude/settings.json and keeps every
// other setting. An unparsable file is left untouched rather than replaced.
func setClaudeKey(env Env, name, key string) error {
	path := env.claudeSettingsPath()
	settings := map[string]any{}
	raw, err := readSmall(path)
	switch {
	case err == nil:
		if len(bytes.TrimSpace(raw)) > 0 {
			if err := json.Unmarshal(raw, &settings); err != nil {
				return errors.New("~/.claude/settings.json을 해석할 수 없어 수정하지 않았습니다")
			}
		}
	case errors.Is(err, os.ErrNotExist):
		if key == "" {
			return nil
		}
	default:
		return err
	}
	envBlock, _ := settings["env"].(map[string]any)
	if envBlock == nil {
		if _, exists := settings["env"]; exists {
			return errors.New("~/.claude/settings.json의 env 항목이 객체가 아닙니다")
		}
		envBlock = map[string]any{}
	}
	if key == "" {
		if _, exists := envBlock[name]; !exists {
			return nil
		}
		delete(envBlock, name)
	} else {
		envBlock[name] = key
	}
	if len(envBlock) == 0 {
		delete(settings, "env")
	} else {
		settings["env"] = envBlock
	}
	out, err := json.MarshalIndent(settings, "", "  ")
	if err != nil {
		return err
	}
	return replaceFile(path, append(out, '\n'), raw)
}

func dotenvValue(path, name string) string {
	raw, err := readSmall(path)
	if err != nil {
		return ""
	}
	for _, line := range strings.Split(string(raw), "\n") {
		k, v, ok := strings.Cut(strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line), "export ")), "=")
		if ok && strings.TrimSpace(k) == name {
			return strings.Trim(strings.TrimSpace(v), `"'`)
		}
	}
	return ""
}

// setDotenv replaces or removes one NAME= line and preserves all other lines.
func setDotenv(path, name, value string) error {
	raw, err := readSmall(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if errors.Is(err, os.ErrNotExist) && value == "" {
		return nil
	}
	var lines []string
	found := false
	if len(raw) > 0 {
		for _, line := range strings.Split(strings.TrimRight(string(raw), "\n"), "\n") {
			k, _, ok := strings.Cut(strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line), "export ")), "=")
			if ok && strings.TrimSpace(k) == name {
				if value != "" && !found {
					lines = append(lines, name+"="+value)
				}
				found = true
				continue
			}
			lines = append(lines, line)
		}
	}
	if !found {
		if value == "" {
			return nil
		}
		lines = append(lines, name+"="+value)
	}
	out := ""
	if len(lines) > 0 {
		out = strings.Join(lines, "\n") + "\n"
	}
	return replaceFile(path, []byte(out), raw)
}

func readSmall(path string) ([]byte, error) { return readLimited(path, 1<<20) }

func readLimited(path string, limit int64) ([]byte, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("%s is not a regular file", path)
	}
	if info.Size() > limit {
		return nil, fmt.Errorf("%s is too large", path)
	}
	return os.ReadFile(path)
}

// replaceFile keeps a timestamped private backup of any previous content and
// atomically installs the new owner-only file.
func replaceFile(path string, data, previous []byte) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	if info, err := os.Lstat(dir); err != nil {
		return err
	} else if !info.IsDir() {
		return fmt.Errorf("%s is not a directory", dir)
	}
	if previous != nil {
		backup := fmt.Sprintf("%s.hmux-backup-%s", path, time.Now().UTC().Format("20060102T150405.000000000Z"))
		if err := os.WriteFile(backup, previous, 0o600); err != nil {
			return fmt.Errorf("backup %s: %w", filepath.Base(path), err)
		}
	}
	tmp, err := os.CreateTemp(dir, ".hmux-*")
	if err != nil {
		return err
	}
	name := tmp.Name()
	defer os.Remove(name)
	if err := tmp.Chmod(0o600); err != nil {
		_ = tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(name, path)
}

// markClaudeReady records what Claude Code's first-run screens would ask, so a
// session started right after HMux connected it opens ready instead of asking
// to log in again: onboarding completed and, for an API key, approval of that
// key (Claude stores the last 20 characters). Other state is preserved.
func markClaudeReady(ctx context.Context, env Env, key string) error {
	path := filepath.Join(env.Home, ".claude.json")
	state := map[string]any{}
	raw, err := readLimited(path, 64<<20)
	switch {
	case err == nil:
		if len(bytes.TrimSpace(raw)) > 0 && json.Unmarshal(raw, &state) != nil {
			return errors.New("~/.claude.json을 해석할 수 없어 수정하지 않았습니다")
		}
	case !errors.Is(err, os.ErrNotExist):
		return err
	}
	changed := false
	if done, _ := state["hasCompletedOnboarding"].(bool); !done {
		state["hasCompletedOnboarding"] = true
		changed = true
	}
	if _, ok := state["lastOnboardingVersion"].(string); !ok {
		if path, ok := env.Executable("claude"); ok {
			if out, err := env.output(ctx, path, "--version"); err == nil {
				if version := cleanVersion(out); version != "" {
					state["lastOnboardingVersion"] = version
					changed = true
				}
			}
		}
	}
	if key != "" {
		suffix := key
		if len(suffix) > 20 {
			suffix = suffix[len(suffix)-20:]
		}
		responses, _ := state["customApiKeyResponses"].(map[string]any)
		if responses == nil {
			responses = map[string]any{}
		}
		approved, _ := responses["approved"].([]any)
		found := false
		for _, value := range approved {
			if value == suffix {
				found = true
			}
		}
		if !found {
			responses["approved"] = append(approved, suffix)
			state["customApiKeyResponses"] = responses
			changed = true
		}
	}
	if !changed {
		return nil
	}
	out, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return err
	}
	return replaceFile(path, append(out, '\n'), raw)
}

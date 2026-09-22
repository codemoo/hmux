package providers

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/codemoo/hmux/internal/safeexec"
)

// Connect jobs run setup.sh in a private tmux server (-L hmux-setup) so they
// never appear in the session catalog. The web UI polls JobStatus, which reads
// the pane, and relays a pasted authorization code with JobInput.
// jobSocket is a variable so tests can use an isolated tmux server.
var jobSocket = "hmux-setup"

const (
	JobNone      = "none"
	JobInstall   = "installing"
	JobLogin     = "login"
	JobConnected = "connected"
	JobDone      = "done"
	JobFailed    = "failed"
)

type JobStatus struct {
	State string `json:"state"`
	// URL is a login page printed by the CLI, restricted to known auth hosts.
	URL string `json:"url,omitempty"`
	// Code is a device code the user types on the login page (Codex).
	Code string `json:"code,omitempty"`
	// NeedsInput is true while the CLI waits for a pasted authorization code.
	NeedsInput bool     `json:"needs_input,omitempty"`
	Log        []string `json:"log,omitempty"`
}

var jobActions = map[string]bool{"connect": true, "update": true}

// tmuxPath is replaced by tests.
var tmuxPath = func() (string, error) {
	if path, err := exec.LookPath("tmux"); err == nil {
		return path, nil
	}
	for _, path := range []string{"/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"} {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path, nil
		}
	}
	return "", errors.New("tmux executable not found")
}

func jobSession(id string) string { return "connect-" + id }

func (e Env) jobStatePath(id string) string {
	return filepath.Join(e.Home, ".local", "state", "hmux-setup", jobSession(id))
}

// jobPane targets the job session's current pane; '=' forces an exact session
// match and the trailing ':' makes it valid where tmux expects a pane target.
func jobPane(id string) string { return "=" + jobSession(id) + ":" }

func (e Env) tmux(ctx context.Context, args ...string) ([]byte, error) {
	path, err := tmuxPath()
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, e.Timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, path, append([]string{"-L", jobSocket}, args...)...)
	cmd.Dir = e.Home
	cmd.Env = append(os.Environ(), "HOME="+e.Home, "PATH="+e.searchPath())
	return safeexec.Output(cmd, 1<<20)
}

// StartJob replaces any previous job for the provider and starts a new one.
func StartJob(ctx context.Context, env Env, action, id string) error {
	if !jobActions[action] {
		return errors.New("unknown setup action")
	}
	if _, ok := Lookup(id); !ok {
		return errors.New("unknown provider")
	}
	if id == "gemini" && action == "connect" {
		// Choose Google login unless another non-key method is configured.
		if err := setGeminiAuth(env, "oauth-personal", func(current string) bool {
			return current == "" || current == "gemini-api-key"
		}); err != nil {
			return err
		}
	}
	_ = CancelJob(ctx, env, id)
	state := env.jobStatePath(id)
	if err := os.MkdirAll(filepath.Dir(state), 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(state, nil, 0o600); err != nil {
		return err
	}
	script := strings.Join(shellQuote([]string{"bash", "-c", setupScript, "hmux-setup", action, id}), " ")
	// A wide pane keeps long OAuth URLs on one line.
	_, err := env.tmux(ctx, "new-session", "-d", "-x", "400", "-y", "60", "-s", jobSession(id), "-c", env.Home,
		"-e", "HOME="+env.Home, "-e", "PATH="+env.searchPath(), "-e", "HMUX_JOB_STATE="+state, script)
	return err
}

func CancelJob(ctx context.Context, env Env, id string) error {
	if _, ok := Lookup(id); !ok {
		return errors.New("unknown provider")
	}
	_ = os.Remove(env.jobStatePath(id))
	_, err := env.tmux(ctx, "kill-session", "-t", "="+jobSession(id))
	return err
}

var inputPattern = regexp.MustCompile(`^[\x21-\x7e]+$`)

// JobInput types one pasted authorization code into a waiting login flow.
func JobInput(ctx context.Context, env Env, id, text string) error {
	if _, ok := Lookup(id); !ok {
		return errors.New("unknown provider")
	}
	text = strings.TrimSpace(text)
	if len(text) > 2048 || !inputPattern.MatchString(text) {
		return errors.New("코드 형식이 올바르지 않습니다")
	}
	if _, err := env.tmux(ctx, "send-keys", "-t", jobPane(id), "-l", text); err != nil {
		return errors.New("진행 중인 로그인이 없습니다")
	}
	_, err := env.tmux(ctx, "send-keys", "-t", jobPane(id), "Enter")
	return err
}

func GetJob(ctx context.Context, env Env, id string) (JobStatus, error) {
	if _, ok := Lookup(id); !ok {
		return JobStatus{}, errors.New("unknown provider")
	}
	raw, err := env.tmux(ctx, "capture-pane", "-p", "-J", "-S", "-300", "-t", jobPane(id))
	if err != nil {
		return JobStatus{State: JobNone}, nil
	}
	phase, _ := os.ReadFile(env.jobStatePath(id))
	status := parseJob(strings.TrimSpace(string(phase)), string(raw))
	// Gemini stays in its interactive UI after login; its credential file is
	// the completion signal.
	if id == "gemini" && status.State == JobLogin {
		if info, err := os.Stat(filepath.Join(env.Home, ".gemini", "oauth_creds.json")); err == nil && info.Mode().IsRegular() {
			status = JobStatus{State: JobConnected, Log: status.Log}
		}
	}
	if status.State == JobConnected || status.State == JobDone || status.State == JobFailed {
		_ = CancelJob(ctx, env, id)
	}
	return status, nil
}

var (
	urlPattern    = regexp.MustCompile(`https://[^\s"'<>]+`)
	codePattern   = regexp.MustCompile(`\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b`)
	promptPattern = regexp.MustCompile(`(?i)(paste code here|authorization code:)`)
	loginHosts    = map[string]bool{
		"auth.openai.com": true, "claude.ai": true, "claude.com": true, "platform.claude.com": true,
		"console.anthropic.com": true, "accounts.google.com": true,
	}
)

// parseJob combines the phase written by setup.sh ("install", "login" or
// "done:<exit>:<last phase>") with the visible pane, which supplies login URLs
// and codes.
func parseJob(phase, pane string) JobStatus {
	status := JobStatus{State: JobInstall}
	switch {
	case phase == "login":
		status.State = JobLogin
	case strings.HasPrefix(phase, "done:0:"):
		status.State = JobDone
	case strings.HasPrefix(phase, "done:"):
		status.State = JobFailed
	}
	var lines []string
	for _, line := range strings.Split(pane, "\n") {
		if line = strings.TrimRight(cleanLine(line), " "); strings.TrimSpace(line) != "" {
			lines = append(lines, line)
		}
	}
	if status.State == JobLogin {
		for _, line := range lines {
			for _, candidate := range urlPattern.FindAllString(line, -1) {
				if u, err := url.Parse(candidate); err == nil && u.Scheme == "https" && loginHosts[u.Hostname()] && len(candidate) <= 4096 {
					status.URL = candidate
				}
			}
			if code := codePattern.FindString(line); code != "" {
				status.Code = code
			}
		}
		if len(lines) > 0 {
			status.NeedsInput = promptPattern.MatchString(lines[len(lines)-1])
		}
	}
	if len(lines) > 12 {
		lines = lines[len(lines)-12:]
	}
	for i, line := range lines {
		if len(line) > 300 {
			lines[i] = line[:300] + "…"
		}
	}
	status.Log = lines
	return status
}

func cleanLine(line string) string {
	return strings.Map(func(r rune) rune {
		if r == '\t' {
			return ' '
		}
		if r < 0x20 || r == 0x7f {
			return -1
		}
		return r
	}, line)
}

func shellQuote(args []string) []string {
	out := make([]string, len(args))
	for i, arg := range args {
		out[i] = "'" + strings.ReplaceAll(arg, "'", `'"'"'`) + "'"
	}
	return out
}

// setGeminiAuth records Gemini's auth type when replace reports that the current
// value should be replaced, so the CLI skips its interactive auth menu.
func setGeminiAuth(env Env, want string, replace func(current string) bool) error {
	path := filepath.Join(env.Home, ".gemini", "settings.json")
	settings := map[string]any{}
	raw, err := readSmall(path)
	if err == nil && len(strings.TrimSpace(string(raw))) > 0 {
		if json.Unmarshal(raw, &settings) != nil {
			return errors.New("~/.gemini/settings.json을 해석할 수 없어 수정하지 않았습니다")
		}
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	security, _ := settings["security"].(map[string]any)
	if security == nil {
		security = map[string]any{}
	}
	auth, _ := security["auth"].(map[string]any)
	if auth == nil {
		auth = map[string]any{}
	}
	current, _ := auth["selectedType"].(string)
	if current == want || !replace(current) {
		return nil
	}
	auth["selectedType"] = want
	security["auth"] = auth
	settings["security"] = security
	out, err := json.MarshalIndent(settings, "", "  ")
	if err != nil {
		return err
	}
	return replaceFile(path, append(out, '\n'), raw)
}

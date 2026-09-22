package providers

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestParseJobCodexDeviceLogin(t *testing.T) {
	status := parseJob("login", `== Codex 다운로드
codex-cli 0.155.1
Follow these steps to sign in with ChatGPT using device code authorization:
1. Open this link in your browser and sign in to your account
   https://auth.openai.com/codex/device
2. Enter this one-time code (expires in 15 minutes)
   ABCD-12345
`)
	if status.State != JobLogin || status.URL != "https://auth.openai.com/codex/device" || status.Code != "ABCD-12345" || status.NeedsInput {
		t.Fatalf("status = %+v", status)
	}
}

func TestParseJobPromptAndUntrustedURLs(t *testing.T) {
	status := parseJob("login", `visit https://evil.example/phish and https://claude.com/cai/oauth/authorize?code=true&state=x
Paste code here if prompted >
`)
	if status.URL != "https://claude.com/cai/oauth/authorize?code=true&state=x" || !status.NeedsInput {
		t.Fatalf("status = %+v", status)
	}
	// URLs printed during install (before the login phase) are never offered.
	if s := parseJob("install", "see https://auth.openai.com/x\n"); s.State != JobInstall || s.URL != "" {
		t.Fatalf("install status = %+v", s)
	}
	if s := parseJob("login", "https://accounts.google.com.evil.example/o\n"); s.URL != "" {
		t.Fatalf("lookalike host accepted: %+v", s)
	}
}

func TestParseJobCompletionMarkers(t *testing.T) {
	if s := parseJob("done:0:login", ""); s.State != JobDone {
		t.Fatalf("done = %+v", s)
	}
	if s := parseJob("done:1:install", "error\n"); s.State != JobFailed || s.Log[0] != "error" {
		t.Fatalf("failed = %+v", s)
	}
}

func TestGeminiOAuthMustChangeDuringLogin(t *testing.T) {
	env := testEnv(t)
	path := env.geminiOAuthPath()
	writeFile(t, path, `{"refresh_token":"old-refresh-token"}`)
	baseline, ok := geminiOAuthFingerprint(env, time.Now())
	if !ok {
		t.Fatal("valid baseline rejected")
	}
	writeFile(t, env.jobMarkerPath("gemini", "oauth-baseline"), baseline)
	if geminiOAuthChanged(env, time.Now()) {
		t.Fatal("unchanged credential completed login")
	}
	writeFile(t, path, "{\n  \"expiry_date\": 9999999999999,\n  \"access_token\": \"rotated-access-token\",\n  \"refresh_token\": \"old-refresh-token\"\n}\n")
	if geminiOAuthChanged(env, time.Now()) {
		t.Fatal("access-token refresh completed interactive login")
	}
	writeFile(t, path, `{"refresh_token":"new-refresh-token"}`)
	if !geminiOAuthChanged(env, time.Now()) {
		t.Fatal("new credential did not complete login")
	}
	writeFile(t, path, `{}`)
	if geminiOAuthChanged(env, time.Now()) {
		t.Fatal("invalid replacement completed login")
	}
}

func TestGeminiAuthSelection(t *testing.T) {
	env := testEnv(t)
	path := filepath.Join(env.Home, ".gemini", "settings.json")
	writeFile(t, path, `{"ui":{"theme":"dark"}}`)
	selected := func() string {
		var v struct {
			UI       map[string]any `json:"ui"`
			Security struct {
				Auth struct {
					SelectedType string `json:"selectedType"`
				} `json:"auth"`
			} `json:"security"`
		}
		raw, _ := os.ReadFile(path)
		if err := json.Unmarshal(raw, &v); err != nil || v.UI["theme"] != "dark" {
			t.Fatalf("settings = %s", raw)
		}
		return v.Security.Auth.SelectedType
	}
	if err := SetKey(context.Background(), env, "gemini", fakeGeminiKey); err != nil {
		t.Fatal(err)
	}
	if got := selected(); got != "gemini-api-key" {
		t.Fatalf("after key = %q", got)
	}
	if err := setGeminiAuth(env, "oauth-personal", func(c string) bool { return c == "" || c == "gemini-api-key" }); err != nil {
		t.Fatal(err)
	}
	if got := selected(); got != "oauth-personal" {
		t.Fatalf("after login = %q", got)
	}
	writeFile(t, path, `{"ui":{"theme":"dark"},"security":{"auth":{"selectedType":"vertex-ai"}}}`)
	if err := setGeminiAuth(env, "oauth-personal", func(c string) bool { return c == "" || c == "gemini-api-key" }); err != nil {
		t.Fatal(err)
	}
	if got := selected(); got != "vertex-ai" {
		t.Fatalf("overrode an explicit auth type: %q", got)
	}
}

func TestJobInputValidation(t *testing.T) {
	env := testEnv(t)
	for _, text := range []string{"", "has space", "line\nbreak", strings.Repeat("x", 2049)} {
		if err := JobInput(context.Background(), env, "claude", text); err == nil {
			t.Fatalf("accepted %q", text)
		}
	}
}

// Drives a real, isolated tmux server: a fake Codex login prints a device code,
// waits for a pasted code and exits, like the real CLI.
func TestJobLifecycleWithTmux(t *testing.T) {
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	env := testEnv(t)
	oldSocket := jobSocket
	jobSocket = fmt.Sprintf("hmux-e2e-%d", time.Now().UnixNano())
	t.Cleanup(func() {
		_, _ = env.tmux(context.Background(), "kill-server")
		jobSocket = oldSocket
	})
	record := filepath.Join(env.Home, "pasted")
	// Clear the screen first, as Gemini does: progress must not depend on it.
	fakeCLI(t, env, "codex", `printf '\033[2J\033[H'
echo "   https://auth.openai.com/codex/device"
echo "   WXYZ-98765"
printf 'Paste code here if prompted > '
read -r code
echo "$code" > "`+record+`"
`)
	ctx := context.Background()
	if err := StartJob(ctx, env, "connect", "codex"); err != nil {
		t.Fatal(err)
	}
	var status JobStatus
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(200 * time.Millisecond) {
		status, _ = GetJob(ctx, env, "codex")
		if status.NeedsInput {
			break
		}
	}
	if status.State != JobLogin || status.URL != "https://auth.openai.com/codex/device" || status.Code != "WXYZ-98765" || !status.NeedsInput {
		t.Fatalf("login status = %+v", status)
	}
	if err := JobInput(ctx, env, "codex", "ABCD-EFGH"); err != nil {
		t.Fatal(err)
	}
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(200 * time.Millisecond) {
		status, _ = GetJob(ctx, env, "codex")
		if status.State == JobDone {
			break
		}
	}
	if status.State != JobDone {
		t.Fatalf("final status = %+v", status)
	}
	if status.URL != "" || status.Code != "" || status.NeedsInput {
		t.Fatalf("pane-derived login fields survived input: %+v", status)
	}
	for _, line := range status.Log {
		if strings.Contains(line, "ABCD-EFGH") {
			t.Fatalf("authorization input returned in job log: %#v", status.Log)
		}
	}
	if raw, _ := os.ReadFile(record); strings.TrimSpace(string(raw)) != "ABCD-EFGH" {
		t.Fatalf("pasted = %q", raw)
	}
	// A finished job is closed after its result is read.
	if status, _ = GetJob(ctx, env, "codex"); status.State != JobNone {
		t.Fatalf("job left running: %+v", status)
	}
}

func TestClaudeReadyAfterLoginAndKey(t *testing.T) {
	env := testEnv(t)
	fakeCLI(t, env, "claude", `echo "2.1.278 (Claude Code)"`)
	path := filepath.Join(env.Home, ".claude.json")
	writeFile(t, path, `{"projects":{"/w":{"x":1}},"theme":"dark"}`)
	if err := markClaudeReady(context.Background(), env, ""); err != nil {
		t.Fatal(err)
	}
	if err := SetKey(context.Background(), env, "claude", fakeAnthropicKey); err != nil {
		t.Fatal(err)
	}
	var state struct {
		Projects        map[string]any `json:"projects"`
		Theme           string         `json:"theme"`
		Onboarded       bool           `json:"hasCompletedOnboarding"`
		Version         string         `json:"lastOnboardingVersion"`
		CustomResponses struct {
			Approved []string `json:"approved"`
		} `json:"customApiKeyResponses"`
	}
	raw, _ := os.ReadFile(path)
	if err := json.Unmarshal(raw, &state); err != nil {
		t.Fatal(err)
	}
	suffix := fakeAnthropicKey[len(fakeAnthropicKey)-20:]
	if !state.Onboarded || state.Version != "2.1.278" || state.Theme != "dark" || state.Projects["/w"] == nil ||
		len(state.CustomResponses.Approved) != 1 || state.CustomResponses.Approved[0] != suffix {
		t.Fatalf("state = %s", raw)
	}
	// Idempotent: saving the same key again adds nothing.
	if err := markClaudeReady(context.Background(), env, fakeAnthropicKey); err != nil {
		t.Fatal(err)
	}
	if again, _ := os.ReadFile(path); string(again) != string(raw) {
		t.Fatalf("rewrote unchanged state: %s", again)
	}
}

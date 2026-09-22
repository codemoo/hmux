package providers

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const (
	fakeOpenAIKey    = "sk-test-openai-000000000001"
	fakeAnthropicKey = "sk-ant-test-0000000000002"
	fakeGeminiKey    = "AIzaTest00000000000003"
)

func testEnv(t *testing.T) Env {
	t.Helper()
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, ".local", "bin"), 0o700); err != nil {
		t.Fatal(err)
	}
	return Env{Home: home, Path: "/usr/bin:/bin", Timeout: 5 * time.Second, SystemDirs: []string{}}
}

func fakeCLI(t *testing.T, env Env, name, script string) {
	t.Helper()
	path := filepath.Join(env.Home, ".local", "bin", name)
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"+script), 0o700); err != nil {
		t.Fatal(err)
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

func byID(statuses []Status) map[string]Status {
	out := map[string]Status{}
	for _, s := range statuses {
		out[s.ID] = s
	}
	return out
}

func TestStatusesWithoutAnyProvider(t *testing.T) {
	env := testEnv(t)
	for _, s := range Statuses(context.Background(), env) {
		if s.Installed || s.Auth != AuthNone || s.Version != "" || s.KeyHint != "" {
			t.Fatalf("empty Home reported %+v", s)
		}
	}
}

func TestStatusesDetectVersionsAndAuth(t *testing.T) {
	env := testEnv(t)
	fakeCLI(t, env, "codex", `case "$1 $2" in
"--version ") echo "codex-cli 0.155.1" ;;
"login status") echo "Logged in using an API key - sk-test-***00001" >&2 ;;
esac
`)
	writeFile(t, filepath.Join(env.Home, ".codex", "auth.json"), `{"auth_mode":"apikey","OPENAI_API_KEY":"`+fakeOpenAIKey+`"}`)
	fakeCLI(t, env, "claude", `case "$1" in
--version) echo "2.1.278 (Claude Code)" ;;
auth) echo '{"loggedIn": true, "authMethod": "claude.ai"}' ;;
esac
`)
	fakeCLI(t, env, "gemini", `echo 0.60.0`)
	writeFile(t, filepath.Join(env.Home, ".gemini", ".env"), "OTHER=1\nGEMINI_API_KEY="+fakeGeminiKey+"\n")

	got := byID(Statuses(context.Background(), env))
	if s := got["codex"]; !s.Installed || s.Version != "0.155.1" || s.Auth != AuthAPIKey || s.KeyHint != "…0001" {
		t.Fatalf("codex = %+v", s)
	}
	if s := got["claude"]; !s.Installed || s.Version != "2.1.278" || s.Auth != AuthAccount || s.KeyHint != "" {
		t.Fatalf("claude = %+v", s)
	}
	if s := got["gemini"]; !s.Installed || s.Version != "0.60.0" || s.Auth != AuthAPIKey || s.KeyHint != "…0003" {
		t.Fatalf("gemini = %+v", s)
	}
	raw, _ := json.Marshal(got)
	for _, key := range []string{fakeOpenAIKey, fakeGeminiKey} {
		if strings.Contains(string(raw), key) {
			t.Fatal("status must never contain a stored key")
		}
	}
}

func TestStatusesRejectInvalidGeminiOAuth(t *testing.T) {
	env := testEnv(t)
	fakeCLI(t, env, "codex", `[ "$1" = login ] && { echo "Not logged in" >&2; exit 1; }; echo "codex-cli 1.0.0"`)
	path := filepath.Join(env.Home, ".gemini", "oauth_creds.json")
	writeFile(t, path, `{}`)
	got := byID(Statuses(context.Background(), env))
	if got["codex"].Auth != AuthNone {
		t.Fatalf("logged-out codex = %+v", got["codex"])
	}
	if s := got["gemini"]; s.Installed || s.Auth != AuthNone {
		t.Fatalf("empty gemini oauth = %+v", s)
	}
	writeFile(t, path, `{not-json`)
	if s := byID(Statuses(context.Background(), env))["gemini"]; s.Auth != AuthNone {
		t.Fatalf("corrupt gemini oauth = %+v", s)
	}
	writeFile(t, path, `{"access_token":"access-token-123","expiry_date":1}`)
	if s := byID(Statuses(context.Background(), env))["gemini"]; s.Auth != AuthNone {
		t.Fatalf("expired gemini oauth = %+v", s)
	}
	writeFile(t, path, `{"refresh_token":"refresh-token-123"}`)
	if s := byID(Statuses(context.Background(), env))["gemini"]; s.Auth != AuthAccount {
		t.Fatalf("usable gemini oauth = %+v", s)
	}
}

func TestValidKeyRejectsInjection(t *testing.T) {
	for _, key := range []string{fakeOpenAIKey, fakeAnthropicKey, fakeGeminiKey} {
		if !ValidKey(key) {
			t.Fatalf("rejected %q", key)
		}
	}
	for _, key := range []string{"", "short", "sk-test-000000000000\nX=1", `sk-test-"0000000000000`, "sk-test 000000000000000", "sk=test-00000000000000"} {
		if ValidKey(key) {
			t.Fatalf("accepted %q", key)
		}
	}
	env := testEnv(t)
	if err := SetKey(context.Background(), env, "gemini", "bad key\nEVIL=1"); err == nil {
		t.Fatal("invalid key was stored")
	}
	if _, err := os.Stat(env.geminiEnvPath()); !os.IsNotExist(err) {
		t.Fatal("invalid key created a file")
	}
}

func TestClaudeKeyPreservesSettingsAndBacksUp(t *testing.T) {
	env := testEnv(t)
	path := env.claudeSettingsPath()
	writeFile(t, path, `{"model":"opus","env":{"FOO":"bar"}}`)
	if err := SetKey(context.Background(), env, "claude", fakeAnthropicKey); err != nil {
		t.Fatal(err)
	}
	var settings map[string]any
	raw, _ := os.ReadFile(path)
	if err := json.Unmarshal(raw, &settings); err != nil {
		t.Fatal(err)
	}
	envBlock := settings["env"].(map[string]any)
	if settings["model"] != "opus" || envBlock["FOO"] != "bar" || envBlock["ANTHROPIC_API_KEY"] != fakeAnthropicKey {
		t.Fatalf("settings = %s", raw)
	}
	if info, _ := os.Stat(path); info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %v", info.Mode().Perm())
	}
	backups, _ := filepath.Glob(path + ".hmux-backup-*")
	if len(backups) != 1 {
		t.Fatalf("backups = %v", backups)
	}
	if s := byID(Statuses(context.Background(), env))["claude"]; s.Auth != AuthAPIKey || s.KeyHint != "…0002" {
		t.Fatalf("claude status = %+v", s)
	}

	if err := SetKey(context.Background(), env, "claude", ""); err != nil {
		t.Fatal(err)
	}
	raw, _ = os.ReadFile(path)
	if strings.Contains(string(raw), "ANTHROPIC_API_KEY") || !strings.Contains(string(raw), `"FOO"`) {
		t.Fatalf("after clear = %s", raw)
	}
}

func TestClaudeKeyRefusesUnparsableSettings(t *testing.T) {
	env := testEnv(t)
	path := env.claudeSettingsPath()
	writeFile(t, path, `{not json`)
	if err := SetKey(context.Background(), env, "claude", fakeAnthropicKey); err == nil {
		t.Fatal("overwrote unparsable settings")
	}
	if raw, _ := os.ReadFile(path); string(raw) != `{not json` {
		t.Fatalf("settings changed: %s", raw)
	}
}

func TestGeminiDotenvReplacesOnlyItsKey(t *testing.T) {
	env := testEnv(t)
	path := env.geminiEnvPath()
	writeFile(t, path, "# comment\nexport GEMINI_API_KEY=old-value-000000000\nOTHER=keep\n")
	if err := SetKey(context.Background(), env, "gemini", fakeGeminiKey); err != nil {
		t.Fatal(err)
	}
	raw, _ := os.ReadFile(path)
	if string(raw) != "# comment\nGEMINI_API_KEY="+fakeGeminiKey+"\nOTHER=keep\n" {
		t.Fatalf("dotenv = %q", raw)
	}
	if err := SetKey(context.Background(), env, "gemini", ""); err != nil {
		t.Fatal(err)
	}
	raw, _ = os.ReadFile(path)
	if string(raw) != "# comment\nOTHER=keep\n" {
		t.Fatalf("after clear = %q", raw)
	}
}

func TestCodexKeyUsesStdinNotArguments(t *testing.T) {
	env := testEnv(t)
	record := filepath.Join(env.Home, "record")
	fakeCLI(t, env, "codex", `echo "args:$*" >> "`+record+`"
if [ "$2" = "--with-api-key" ]; then cat >> "`+record+`"; fi
`)
	if err := SetKey(context.Background(), env, "codex", fakeOpenAIKey); err != nil {
		t.Fatal(err)
	}
	raw, _ := os.ReadFile(record)
	text := string(raw)
	if !strings.Contains(text, "args:login --with-api-key\n"+fakeOpenAIKey) {
		t.Fatalf("record = %q", text)
	}
	if strings.Contains(strings.Split(text, "\n")[0], fakeOpenAIKey) {
		t.Fatal("key appeared in command arguments")
	}
}

func TestCodexClearKeepsAccountLogin(t *testing.T) {
	env := testEnv(t)
	record := filepath.Join(env.Home, "record")
	fakeCLI(t, env, "codex", `echo "$*" >> "`+record+`"
[ "$1 $2" = "login status" ] && echo "Logged in using ChatGPT" >&2
`)
	if err := SetKey(context.Background(), env, "codex", ""); err != nil {
		t.Fatal(err)
	}
	raw, _ := os.ReadFile(record)
	if strings.Contains(string(raw), "logout") {
		t.Fatal("clearing an API key logged out a ChatGPT account")
	}
}

func TestCodexKeyRequiresInstall(t *testing.T) {
	env := testEnv(t)
	if err := SetKey(context.Background(), env, "codex", fakeOpenAIKey); err == nil {
		t.Fatal("stored a Codex key without the CLI")
	}
}

func TestSetupScriptSyntaxAndArgumentGuard(t *testing.T) {
	if err := exec.Command("bash", "-n", "-c", setupScript).Run(); err != nil {
		t.Fatalf("setup.sh syntax: %v", err)
	}
	out, err := exec.Command("bash", "-c", setupScript, "hmux-setup", "connect", "unknown").CombinedOutput()
	if err == nil || !strings.Contains(string(out), "unknown provider") {
		t.Fatalf("unknown provider: %v %s", err, out)
	}
}

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestAppCapabilitiesEnvelope(t *testing.T) {
	var output bytes.Buffer
	err := runApp(context.Background(), config.ClientConfig{}, []string{"capabilities"}, strings.NewReader(""), &output, func(string) string { return "" })
	if err != nil {
		t.Fatal(err)
	}
	var envelope appEnvelope
	if err := json.Unmarshal(output.Bytes(), &envelope); err != nil {
		t.Fatal(err)
	}
	if !envelope.OK || envelope.AppProtocolVersion != 1 || envelope.Error != nil {
		t.Fatalf("unexpected envelope: %#v", envelope)
	}
	if !strings.Contains(output.String(), `"hidden-set"`) {
		t.Fatalf("hidden-set capability is absent: %s", output.String())
	}
	if !strings.Contains(output.String(), `"signed-native-update"`) {
		t.Fatalf("signed native update capability is absent: %s", output.String())
	}
	if !strings.Contains(output.String(), `"catalog-stream"`) {
		t.Fatalf("catalog stream capability is absent: %s", output.String())
	}
	if !strings.Contains(output.String(), `"version":"`+version+`"`) {
		t.Fatalf("backend version is absent: %s", output.String())
	}
}

func appTestWorkspace(t *testing.T) (config.ClientConfig, func(string) string) {
	t.Helper()
	cfg := config.DefaultClientConfig()
	cfg.Role = "home"
	cfg.StateDir = t.TempDir()
	key, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	return cfg, func(name string) string {
		if name == appWorkspaceSourceEnvironment {
			return key
		}
		return ""
	}
}

func TestAppRequiresMatchingWorkspaceBeforeSessionActions(t *testing.T) {
	for _, binding := range []string{"missing", "matching", "changed"} {
		t.Run(binding, func(t *testing.T) {
			cfg, getenv := appTestWorkspace(t)
			switch binding {
			case "missing":
				getenv = func(string) string { return "" }
			case "changed":
				cfg.StateDir = t.TempDir()
			}
			for _, command := range []string{"terminal", "create", "alias-set", "hidden-set", "terminate", "file-stage", "conversation"} {
				t.Run(command, func(t *testing.T) {
					// Deliberately invalid input proves the matching binding reaches
					// request validation without ever accessing tmux or inventory.
					var output bytes.Buffer
					err := runApp(t.Context(), cfg, []string{command}, strings.NewReader(`{"unexpected":true}`), &output, getenv)
					if err == nil {
						t.Fatal("request with invalid input was accepted")
					}
					if command == "terminal" {
						switch binding {
						case "missing":
							if !strings.Contains(err.Error(), "source key is required") {
								t.Fatalf("missing binding error=%v", err)
							}
						case "changed":
							if !strings.Contains(err.Error(), "configuration changed") {
								t.Fatalf("changed binding error=%v", err)
							}
						case "matching":
							var requestError appHandlerError
							if !errors.As(err, &requestError) || requestError.code != "invalid_request" {
								t.Fatalf("matching binding did not reach identity validation: %v", err)
							}
						}
						return
					}
					wantCode := "workspace_source_changed"
					if binding == "matching" {
						wantCode = "invalid_request"
					}
					var envelope appEnvelope
					if decodeErr := json.Unmarshal(output.Bytes(), &envelope); decodeErr != nil {
						t.Fatal(decodeErr)
					}
					if envelope.OK || envelope.Error == nil || envelope.Error.Code != wantCode {
						t.Fatalf("want code %s, got %s", wantCode, output.String())
					}
				})
			}
		})
	}
}

func TestNativeAppEnvironmentIsStrict(t *testing.T) {
	values := map[string]string{
		"HMUX_APP_VERSION":     "0.1.28",
		"HMUX_APP_BUNDLE_PATH": "/Users/test/Applications/HMux.app",
	}
	version, bundlePath, err := nativeAppEnvironment(func(key string) string { return values[key] })
	if err != nil || version != "0.1.28" || bundlePath != values["HMUX_APP_BUNDLE_PATH"] {
		t.Fatalf("version=%q bundle=%q err=%v", version, bundlePath, err)
	}
	for _, unsafe := range []string{"../HMux.app", "/Users/test/Other.app", "/"} {
		values["HMUX_APP_BUNDLE_PATH"] = unsafe
		if _, _, err := nativeAppEnvironment(func(key string) string { return values[key] }); err == nil {
			t.Fatalf("unsafe bundle path %q was accepted", unsafe)
		}
	}
}

func TestNativeAppRollbackBundleDerivesStrictEnclosingBundle(t *testing.T) {
	bundle := filepath.Join(t.TempDir(), "HMux.app")
	helper := filepath.Join(bundle, "Contents", "Helpers", "hmux")
	if err := os.MkdirAll(filepath.Dir(helper), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(helper, []byte("helper"), 0o700); err != nil {
		t.Fatal(err)
	}
	getenv := func(string) string { return "" }
	derived, err := nativeAppRollbackBundle(getenv, func() (string, error) { return helper, nil })
	if err != nil || derived != bundle {
		t.Fatalf("bundle=%q err=%v", derived, err)
	}
	for _, unsafe := range []string{
		filepath.Join(bundle, "Contents", "MacOS", "hmux"),
		filepath.Join(bundle, "Contents", "Helpers", "other"),
		filepath.Join(filepath.Dir(bundle), "Other.app", "Contents", "Helpers", "hmux"),
	} {
		if _, err := nativeAppRollbackBundle(getenv, func() (string, error) { return unsafe, nil }); err == nil {
			t.Fatalf("unsafe helper path %q was accepted", unsafe)
		}
	}
}

func TestNativeAppRollbackBundleRejectsPartialEnvironment(t *testing.T) {
	values := map[string]string{"HMUX_APP_BUNDLE_PATH": "/Users/test/Applications/HMux.app"}
	if _, err := nativeAppRollbackBundle(func(key string) string { return values[key] }, os.Executable); err == nil {
		t.Fatal("partial native app environment was accepted")
	}
}

func TestAppRequestRejectsUnknownAndTrailingFields(t *testing.T) {
	cfg, getenv := appTestWorkspace(t)
	for _, input := range []string{
		`{"session":{"id":"$1","created_at":1},"alias":"x","extra":true}`,
		`{"session":{"id":"$1","created_at":1},"alias":"x"} {}`,
	} {
		var output bytes.Buffer
		err := runApp(context.Background(), cfg, []string{"alias-set"}, strings.NewReader(input), &output, getenv)
		var exit appCommandExitError
		if !errors.As(err, &exit) {
			t.Fatalf("expected app exit for %q, got %v", input, err)
		}
		if !strings.Contains(output.String(), `"code":"invalid_request"`) {
			t.Fatalf("unexpected output: %s", output.String())
		}
	}
}

func TestAppMutationRequestsRequireExplicitFields(t *testing.T) {
	cfg, getenv := appTestWorkspace(t)
	tests := []struct {
		command string
		body    string
	}{
		{"alias-set", `{"session":{"id":"$1","created_at":1}}`},
		{"alias-set", `{"alias":""}`},
		{"hidden-set", `{"session":{"id":"$1","created_at":1}}`},
		{"hidden-set", `{"hidden":false}`},
		{"terminate", `{"session":{"id":"$1","created_at":1}}`},
		{"terminate", `{"confirmed":false}`},
		{"file-stage", `{"request_id":"00112233445566778899aabbccddeeff","paths":["/tmp/file"]}`},
		{"file-stage", `{"session":{"id":"$1","created_at":1},"paths":["/tmp/file"]}`},
		{"file-stage", `{"request_id":"not-hex","session":{"id":"$1","created_at":1},"paths":["/tmp/file"]}`},
		{"file-stage", `{"request_id":"00112233445566778899aabbccddeeff","session":{"id":"$1","created_at":1},"paths":[]}`},
	}
	for _, test := range tests {
		var output bytes.Buffer
		err := runApp(context.Background(), cfg, []string{test.command}, strings.NewReader(test.body), &output, getenv)
		var exit appCommandExitError
		if !errors.As(err, &exit) || !strings.Contains(output.String(), `"code":"invalid_request"`) {
			t.Fatalf("command=%s body=%s err=%v output=%s", test.command, test.body, err, output.String())
		}
	}
}

func TestAppTerminateExplicitFalseRequiresConfirmation(t *testing.T) {
	cfg, getenv := appTestWorkspace(t)
	var output bytes.Buffer
	err := runApp(
		context.Background(), cfg, []string{"terminate"},
		strings.NewReader(`{"session":{"id":"$1","created_at":1},"confirmed":false}`),
		&output, getenv,
	)
	var exit appCommandExitError
	if !errors.As(err, &exit) || !strings.Contains(output.String(), `"code":"confirmation_required"`) {
		t.Fatalf("err=%v output=%s", err, output.String())
	}
}

func TestAppRequestRejectsOversizedBody(t *testing.T) {
	cfg, getenv := appTestWorkspace(t)
	var output bytes.Buffer
	input := strings.Repeat(" ", maxAppRequestBytes+1)
	err := runApp(context.Background(), cfg, []string{"alias-set"}, strings.NewReader(input), &output, getenv)
	var exit appCommandExitError
	if !errors.As(err, &exit) || !strings.Contains(output.String(), `"code":"invalid_request"`) {
		t.Fatalf("err=%v output=%s", err, output.String())
	}
}

func TestTerminalIdentity(t *testing.T) {
	values := map[string]string{"HMUX_SESSION_ID": "$42", "HMUX_SESSION_CREATED_AT": "1700000000"}
	identity, err := terminalIdentity(func(key string) string { return values[key] })
	if err != nil {
		t.Fatal(err)
	}
	if identity.ID != "$42" || identity.CreatedAt != 1700000000 {
		t.Fatalf("identity=%#v", identity)
	}

	values["HMUX_SESSION_ID"] = "$42;kill-session"
	if _, err := terminalIdentity(func(key string) string { return values[key] }); err == nil {
		t.Fatal("unsafe session ID was accepted")
	}
}

func TestAppTerminalUsesSharedGroupedView(t *testing.T) {
	if !appTerminalSharedAttach {
		t.Fatal("native grouped tabs must preserve other clients")
	}
}

func TestAppCatalogRejectsDuplicateIdentity(t *testing.T) {
	for _, sessions := range [][]model.Session{
		{
			{ID: "$7", CreatedAt: 1700000000},
			{ID: "$7", CreatedAt: 1700000000},
		},
		{
			{ID: "$7", CreatedAt: 1700000000},
			{ID: "$7", CreatedAt: 1700000001},
		},
	} {
		value := model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: sessions}
		if err := validateAppCatalog(value); err == nil {
			t.Fatal("duplicate session ID was accepted")
		}
	}
}

func TestAppEnvelopeBoundIncludesNewline(t *testing.T) {
	envelope := appEnvelope{AppProtocolVersion: appProtocolVersion, OK: true, Data: "\x00<&"}
	var full bytes.Buffer
	if err := writeAppEnvelope(&full, envelope); err != nil {
		t.Fatal(err)
	}
	var bounded bytes.Buffer
	if err := writeAppEnvelopeBounded(&bounded, envelope, full.Len()-1); err == nil || bounded.Len() != 0 {
		t.Fatal("oversized envelope was emitted")
	}
	if err := writeAppEnvelopeBounded(&bounded, envelope, full.Len()); err != nil || bounded.String() != full.String() {
		t.Fatal("exact envelope bound rejected")
	}
}

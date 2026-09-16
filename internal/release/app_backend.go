package release

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

const CurrentAppProtocolVersion = 1

var CurrentAppFeatures = []string{
	"catalog",
	"workspace",
	"catalog-stream",
	"usage-stream",
	"conversation",
	"profiles",
	"create",
	"alias-set",
	"hidden-set",
	"terminate",
	"file-stage",
	"terminal",
	"signed-native-update",
}

type AppBackendRequirement struct {
	Protocol int
	Features []string
}

type appCapabilitiesEnvelope struct {
	AppProtocolVersion int  `json:"app_protocol_version"`
	OK                 bool `json:"ok"`
	Data               struct {
		BackendProtocolVersion int      `json:"backend_protocol_version"`
		Version                string   `json:"version,omitempty"`
		Features               []string `json:"features"`
	} `json:"data"`
	Error *struct {
		Code    string `json:"code"`
		Message string `json:"message"`
	} `json:"error,omitempty"`
}

func CurrentAppBackendRequirement() AppBackendRequirement {
	return AppBackendRequirement{
		Protocol: CurrentAppProtocolVersion,
		Features: append([]string(nil), CurrentAppFeatures...),
	}
}

// ValidateAppBackend ensures a signed helper can serve the already-running
// Swift UI before that helper becomes visible through current or HMux.app.
func ValidateAppBackend(ctx context.Context, executable string, requirement AppBackendRequirement) error {
	if requirement.Protocol < 1 || len(requirement.Features) > 64 {
		return errors.New("invalid app backend requirement")
	}
	if !filepath.IsAbs(executable) {
		return errors.New("app backend path must be absolute")
	}
	info, err := os.Lstat(executable)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return errors.New("app backend must be an executable regular file, not a symlink")
	}
	validationCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	command := exec.CommandContext(validationCtx, executable, "--no-update-check", "app", "capabilities")
	command.Env = appBackendValidationEnvironment()
	output, err := safeexec.Output(command, 64*1024)
	if err != nil {
		return fmt.Errorf("run app backend capabilities: %w", err)
	}
	var envelope appCapabilitiesEnvelope
	decoder := json.NewDecoder(bytes.NewReader(output))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&envelope); err != nil {
		return fmt.Errorf("decode app backend capabilities: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return errors.New("app backend capabilities contain trailing data")
	}
	if !envelope.OK {
		return errors.New("app backend rejected the capabilities request")
	}
	if envelope.AppProtocolVersion != requirement.Protocol {
		return fmt.Errorf("app protocol %d is incompatible with required protocol %d", envelope.AppProtocolVersion, requirement.Protocol)
	}
	if envelope.Data.BackendProtocolVersion != model.ProtocolVersion {
		return fmt.Errorf("backend protocol %d is incompatible with required protocol %d", envelope.Data.BackendProtocolVersion, model.ProtocolVersion)
	}
	if envelope.Data.Version != "" && !ValidVersion(envelope.Data.Version) {
		return errors.New("app backend version is invalid")
	}
	if len(envelope.Data.Features) > 128 {
		return errors.New("app backend feature count exceeds limit")
	}
	available := make(map[string]struct{}, len(envelope.Data.Features))
	for _, feature := range envelope.Data.Features {
		if feature == "" || len(feature) > 128 || strings.ContainsAny(feature, " \t\r\n") {
			return errors.New("app backend contains an invalid feature")
		}
		available[feature] = struct{}{}
	}
	for _, required := range requirement.Features {
		if required == "" || len(required) > 128 || strings.ContainsAny(required, " \t\r\n") {
			return errors.New("invalid required app backend feature")
		}
		if _, ok := available[required]; !ok {
			return fmt.Errorf("app backend is missing required feature %q", required)
		}
	}
	return nil
}

func appBackendValidationEnvironment() []string {
	result := make([]string, 0, len(os.Environ()))
	for _, value := range os.Environ() {
		key, _, _ := strings.Cut(value, "=")
		switch key {
		case "TMUX", "TMUX_PANE", "HMUX_LAUNCHER", "HMUX_LAUNCHER_ID", "HMUX_SESSION_ID", "HMUX_SESSION_CREATED_AT":
			continue
		default:
			result = append(result, value)
		}
	}
	return result
}

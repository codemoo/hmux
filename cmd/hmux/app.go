package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/release"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

const (
	appProtocolVersion      = release.CurrentAppProtocolVersion
	maxAppRequestBytes      = 64 * 1024
	maxAppResponseBytes     = 32 * 1024 * 1024
	appTerminalSharedAttach = true
)

type appCommandExitError struct{}

func (appCommandExitError) Error() string { return "app command failed" }

type appEnvelope struct {
	AppProtocolVersion int       `json:"app_protocol_version"`
	OK                 bool      `json:"ok"`
	Data               any       `json:"data,omitempty"`
	Error              *appError `json:"error,omitempty"`
}

type appError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type appSessionIdentity struct {
	ID        string `json:"id"`
	CreatedAt int64  `json:"created_at"`
}

type appProfile struct {
	ID    string   `json:"id"`
	Label string   `json:"label"`
	Tags  []string `json:"tags,omitempty"`
}

type appCatalogData struct {
	model.Catalog
	AppUpdate          *release.AppUpdateStatus `json:"app_update,omitempty"`
	WorkspaceSourceKey string                   `json:"workspace_source_key,omitempty"`
}

type appCreateRequest struct {
	ProfileID string `json:"profile_id"`
	Name      string `json:"name,omitempty"`
}

type appAliasRequest struct {
	Session *appSessionIdentity `json:"session"`
	Alias   *string             `json:"alias"`
}

type appHiddenRequest struct {
	Session *appSessionIdentity `json:"session"`
	Hidden  *bool               `json:"hidden"`
}

type appTerminateRequest struct {
	Session   *appSessionIdentity `json:"session"`
	Confirmed *bool               `json:"confirmed"`
}

type appFileStageRequest struct {
	RequestID string              `json:"request_id"`
	Session   *appSessionIdentity `json:"session"`
	Paths     []string            `json:"paths"`
}

type appHandlerError struct {
	code    string
	message string
}

func (e appHandlerError) Error() string { return e.message }

func runApp(
	ctx context.Context,
	cfg config.ClientConfig,
	args []string,
	reader io.Reader,
	writer io.Writer,
	getenv func(string) string,
) error {
	if len(args) != 1 {
		return writeAppFailure(writer, "invalid_request", "usage: hmux app <capabilities|catalog|catalog-stream|usage-stream|profiles|create|alias-set|hidden-set|terminate|file-stage|terminal|update-native|rollback-native>")
	}
	switch args[0] {
	case "terminal", "create", "alias-set", "hidden-set", "terminate", "file-stage", "conversation", "workspace":
		if err := requireAppWorkspaceSource(ctx, cfg, getenv); err != nil {
			if args[0] == "terminal" {
				return err
			}
			return writeAppFailure(writer, "workspace_source_changed", err.Error())
		}
	}

	// terminal replaces the current process with tmux or ssh. It intentionally
	// does not use the JSON envelope because its stdout is the terminal PTY.
	if args[0] == "terminal" {
		identity, err := terminalIdentity(getenv)
		if err != nil {
			return err
		}
		// This precheck provides a safe compatibility path for older remote
		// agents. Updated Home agents repeat it atomically inside tmux.
		if _, err := requireCurrentSession(ctx, cfg, identity); err != nil {
			return err
		}
		for _, name := range []string{"TMUX", "TMUX_PANE", "HMUX_LAUNCHER", "HMUX_LAUNCHER_ID"} {
			_ = os.Unsetenv(name)
		}
		return client.AttachExpectedAppView(cfg, identity.ID, identity.CreatedAt, appTerminalSharedAttach)
	}

	var data any
	var err error
	switch args[0] {
	case "capabilities":
		data = map[string]any{
			"backend_protocol_version": model.ProtocolVersion,
			"version":                  version,
			"features":                 append([]string(nil), release.CurrentAppFeatures...),
		}
	case "catalog":
		sourceKey, _ := appWorkspaceSourceKey(ctx, cfg)
		var value model.Catalog
		value, err = client.Catalog(ctx, cfg)
		if err == nil {
			err = validateAppCatalog(value)
		}
		catalogData := appCatalogData{Catalog: value, WorkspaceSourceKey: sourceKey}
		if appVersion, _, environmentErr := nativeAppEnvironment(getenv); environmentErr == nil {
			catalogData.AppUpdate, _ = release.ReadAppUpdateStatus(cfg.CacheDir, appVersion)
		}
		data = catalogData
	case "workspace":
		var request struct {
			Change *sharedworkspace.Change `json:"change"`
		}
		if err = decodeAppRequest(reader, &request); err == nil {
			data, err = client.SharedWorkspace(ctx, cfg, request.Change)
		}
	case "conversation":
		var request struct {
			Session *appSessionIdentity `json:"session"`
		}
		if err = decodeAppRequest(reader, &request); err == nil {
			if request.Session == nil || model.ValidateSessionID(request.Session.ID) != nil || request.Session.CreatedAt < 1 {
				err = appHandlerError{code: "invalid_request", message: "A valid session identity is required."}
			} else {
				data, err = client.Conversation(ctx, cfg, request.Session.ID, request.Session.CreatedAt)
				if err != nil {
					err = appHandlerError{code: "conversation_unavailable", message: "Conversation unavailable. Verify that Codex is running and the Home agent is current."}
				}
				if err == nil {
					err = requireAppWorkspaceSource(ctx, cfg, getenv)
				}
			}
		}
	case "profiles":
		var inventory model.Inventory
		inventory, err = config.LoadInventory(cfg.InventoryPath)
		if err == nil {
			profiles := make([]appProfile, 0, len(inventory.Profiles))
			for _, profile := range inventory.Profiles {
				profiles = append(profiles, appProfile{ID: profile.ID, Label: profile.Label, Tags: append([]string(nil), profile.Tags...)})
			}
			data = map[string]any{"profiles": profiles}
		}
	case "create":
		var request appCreateRequest
		if err = decodeAppRequest(reader, &request); err == nil {
			data, err = appCreate(ctx, cfg, request)
		}
	case "alias-set":
		var request appAliasRequest
		if err = decodeAppRequest(reader, &request); err == nil {
			if request.Session == nil || request.Alias == nil {
				err = appHandlerError{code: "invalid_request", message: "Session and alias fields are required."}
			} else {
				_, err = requireCurrentSession(ctx, cfg, *request.Session)
				if err == nil {
					err = client.SetAliasExpected(ctx, cfg, request.Session.ID, request.Session.CreatedAt, *request.Alias)
				}
			}
			data = map[string]any{"session": request.Session}
		}
	case "hidden-set":
		var request appHiddenRequest
		if err = decodeAppRequest(reader, &request); err == nil {
			if request.Session == nil || request.Hidden == nil {
				err = appHandlerError{code: "invalid_request", message: "Session and hidden fields are required."}
			} else {
				_, err = requireCurrentSession(ctx, cfg, *request.Session)
				if err == nil {
					err = client.SetHiddenExpected(ctx, cfg, request.Session.ID, request.Session.CreatedAt, *request.Hidden)
				}
			}
			data = map[string]any{"session": request.Session, "hidden": request.Hidden}
		}
	case "terminate":
		var request appTerminateRequest
		if err = decodeAppRequest(reader, &request); err == nil {
			if request.Session == nil || request.Confirmed == nil {
				err = appHandlerError{code: "invalid_request", message: "Session and confirmed fields are required."}
			} else if !*request.Confirmed {
				err = appHandlerError{code: "confirmation_required", message: "Session termination requires confirmed=true."}
			} else if _, err = requireCurrentSession(ctx, cfg, *request.Session); err == nil {
				err = client.TerminateSessionExpected(ctx, cfg, request.Session.ID, request.Session.CreatedAt)
			}
			data = map[string]any{"session": request.Session}
		}
	case "file-stage":
		var request appFileStageRequest
		if err = decodeAppRequest(reader, &request); err == nil {
			if request.Session == nil || !filestage.ValidRequestID(request.RequestID) ||
				len(request.Paths) < 1 || len(request.Paths) > filestage.MaximumFiles {
				err = appHandlerError{code: "invalid_request", message: "A valid request, session, and 1-16 local files are required."}
			} else if _, err = requireCurrentSession(ctx, cfg, *request.Session); err == nil {
				identity := filestage.SessionIdentity{ID: request.Session.ID, CreatedAt: request.Session.CreatedAt}
				data, err = client.StageFiles(ctx, cfg, identity, request.RequestID, request.Paths)
				if err == nil {
					_, err = requireCurrentSession(ctx, cfg, *request.Session)
				}
			}
		}
	case "update-native":
		var appVersion, bundlePath string
		appVersion, bundlePath, err = nativeAppEnvironment(getenv)
		if err == nil {
			now := time.Now()
			installed := false
			if release.NativeAppUpdateCheckDue(cfg.CacheDir, now, 6*time.Hour) {
				updateCtx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
				installed, err = release.UpdateInstalledApp(updateCtx, cfg, appVersion, bundlePath)
				cancel()
				recordErr := release.RecordNativeAppUpdateCheck(cfg.CacheDir, now)
				if err == nil {
					err = recordErr
				}
			}
			status, statusErr := release.ReadAppUpdateStatus(cfg.CacheDir, appVersion)
			if statusErr == nil && status == nil && installed {
				status, statusErr = release.DetectInstalledAppUpdate(ctx, bundlePath, appVersion)
			}
			if err == nil {
				err = statusErr
			}
			data = map[string]any{"installed": installed, "app_update": status}
		}
	case "rollback-native":
		var bundlePath string
		bundlePath, err = nativeAppRollbackBundle(getenv, os.Executable)
		if err == nil {
			rollbackCtx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
			var rolledBackTo string
			rolledBackTo, err = release.RollbackInstalledApp(rollbackCtx, cfg, bundlePath)
			cancel()
			data = map[string]any{"rolled_back": err == nil, "version": rolledBackTo}
		}
	default:
		err = appHandlerError{code: "invalid_request", message: "Unknown hmux app command."}
	}
	if err != nil {
		var handlerErr appHandlerError
		if errors.As(err, &handlerErr) {
			return writeAppFailure(writer, handlerErr.code, handlerErr.message)
		}
		if errors.Is(err, catalog.ErrSessionChanged) {
			return writeAppFailure(writer, "session_changed", "Session identity no longer matches.")
		}
		return writeAppFailure(writer, "backend_error", model.SafeText(err.Error(), 1024))
	}
	envelope := appEnvelope{AppProtocolVersion: appProtocolVersion, OK: true, Data: data}
	if args[0] == "conversation" {
		return writeAppEnvelopeBounded(writer, envelope, 2<<20)
	}
	return writeAppEnvelope(writer, envelope)
}

func nativeAppEnvironment(getenv func(string) string) (string, string, error) {
	version := strings.TrimSpace(getenv("HMUX_APP_VERSION"))
	bundlePath := filepath.Clean(getenv("HMUX_APP_BUNDLE_PATH"))
	if !release.ValidVersion(version) {
		return "", "", appHandlerError{code: "invalid_request", message: "HMUX_APP_VERSION is invalid."}
	}
	if !filepath.IsAbs(bundlePath) || filepath.Base(bundlePath) != "HMux.app" {
		return "", "", appHandlerError{code: "invalid_request", message: "HMUX_APP_BUNDLE_PATH is invalid."}
	}
	return version, bundlePath, nil
}

func nativeAppRollbackBundle(
	getenv func(string) string,
	executable func() (string, error),
) (string, error) {
	if strings.TrimSpace(getenv("HMUX_APP_VERSION")) != "" ||
		strings.TrimSpace(getenv("HMUX_APP_BUNDLE_PATH")) != "" {
		_, bundlePath, err := nativeAppEnvironment(getenv)
		return bundlePath, err
	}
	path, err := executable()
	if err != nil {
		return "", appHandlerError{code: "invalid_request", message: "HMux.app could not be derived from the helper path."}
	}
	path = filepath.Clean(path)
	contentsPath := filepath.Dir(filepath.Dir(path))
	bundlePath := filepath.Dir(contentsPath)
	if !filepath.IsAbs(path) || filepath.Base(path) != "hmux" ||
		filepath.Base(filepath.Dir(path)) != "Helpers" ||
		filepath.Base(contentsPath) != "Contents" || filepath.Base(bundlePath) != "HMux.app" ||
		path != filepath.Join(bundlePath, "Contents", "Helpers", "hmux") {
		return "", appHandlerError{code: "invalid_request", message: "HMux.app could not be derived from the helper path."}
	}
	info, err := os.Lstat(path)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return "", appHandlerError{code: "invalid_request", message: "The bundled HMux helper path is invalid."}
	}
	return bundlePath, nil
}

func appCreate(ctx context.Context, cfg config.ClientConfig, request appCreateRequest) (any, error) {
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		return nil, err
	}
	created, err := client.CreateSession(ctx, cfg, inventory, request.ProfileID, request.Name)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"session": appSessionIdentity{ID: created.ID, CreatedAt: created.CreatedAt},
		"reused":  created.Reused,
	}, nil
}

func terminalIdentity(getenv func(string) string) (appSessionIdentity, error) {
	identity := appSessionIdentity{ID: getenv("HMUX_SESSION_ID")}
	if err := model.ValidateSessionID(identity.ID); err != nil {
		return identity, appHandlerError{code: "invalid_request", message: "HMUX_SESSION_ID is invalid."}
	}
	createdAt, err := strconv.ParseInt(getenv("HMUX_SESSION_CREATED_AT"), 10, 64)
	if err != nil || createdAt < 1 {
		return identity, appHandlerError{code: "invalid_request", message: "HMUX_SESSION_CREATED_AT is invalid."}
	}
	identity.CreatedAt = createdAt
	return identity, nil
}

func requireCurrentSession(ctx context.Context, cfg config.ClientConfig, identity appSessionIdentity) (model.Session, error) {
	if err := model.ValidateSessionID(identity.ID); err != nil || identity.CreatedAt < 1 {
		return model.Session{}, appHandlerError{code: "invalid_request", message: "Session identity is invalid."}
	}
	value, err := client.Catalog(ctx, cfg)
	if err != nil {
		return model.Session{}, err
	}
	for _, session := range value.Sessions {
		if session.ID != identity.ID {
			continue
		}
		if session.CreatedAt != identity.CreatedAt {
			return model.Session{}, appHandlerError{code: "session_changed", message: "Session identity no longer matches."}
		}
		return session, nil
	}
	return model.Session{}, appHandlerError{code: "session_changed", message: "Session no longer exists."}
}

func decodeAppRequest(reader io.Reader, target any) error {
	data, err := io.ReadAll(io.LimitReader(reader, maxAppRequestBytes+1))
	if err != nil || len(data) > maxAppRequestBytes {
		return appHandlerError{code: "invalid_request", message: "Request body exceeds the size limit."}
	}
	decoder := json.NewDecoder(strings.NewReader(string(data)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return appHandlerError{code: "invalid_request", message: "Request body is not valid JSON."}
	}
	if err := decoder.Decode(&struct{}{}); err != io.EOF {
		return appHandlerError{code: "invalid_request", message: "Request body contains trailing data or exceeds the size limit."}
	}
	return nil
}

func validateAppCatalog(value model.Catalog) error {
	if value.ProtocolVersion != model.ProtocolVersion {
		return errors.New("catalog protocol version mismatch")
	}
	if len(value.Sessions) > 10000 {
		return errors.New("catalog session count exceeds limit")
	}
	identities := make(map[appSessionIdentity]struct{}, len(value.Sessions))
	ids := make(map[string]struct{}, len(value.Sessions))
	for _, session := range value.Sessions {
		if err := model.ValidateSessionID(session.ID); err != nil || session.CreatedAt < 1 {
			return errors.New("catalog contains an invalid session identity")
		}
		if len(session.WindowNames) > 10000 || len(session.Tags) > 64 || len(session.Workflows) > 256 {
			return errors.New("catalog field count exceeds limit")
		}
		identity := appSessionIdentity{ID: session.ID, CreatedAt: session.CreatedAt}
		if _, duplicate := identities[identity]; duplicate {
			return errors.New("catalog contains a duplicate session identity")
		}
		if _, duplicate := ids[session.ID]; duplicate {
			return errors.New("catalog contains a duplicate session ID")
		}
		identities[identity] = struct{}{}
		ids[session.ID] = struct{}{}
	}
	return nil
}

func writeAppFailure(writer io.Writer, code, message string) error {
	if strings.TrimSpace(message) == "" {
		message = "HMux app request failed."
	}
	if err := writeAppEnvelope(writer, appEnvelope{
		AppProtocolVersion: appProtocolVersion,
		OK:                 false,
		Error:              &appError{Code: code, Message: model.SafeText(message, 1024)},
	}); err != nil {
		return err
	}
	return appCommandExitError{}
}

func writeAppEnvelope(writer io.Writer, envelope appEnvelope) error {
	return writeAppEnvelopeBounded(writer, envelope, maxAppResponseBytes)
}

func writeAppEnvelopeBounded(writer io.Writer, envelope appEnvelope, limit int) error {
	data, err := json.Marshal(envelope)
	if err != nil {
		return err
	}
	if len(data)+1 > limit {
		return fmt.Errorf("app response exceeds %d bytes", limit)
	}
	data = append(data, '\n')
	_, err = writer.Write(data)
	return err
}

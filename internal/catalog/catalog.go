package catalog

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

// tmux strips C0 control separators from format output. This printable,
// protocol-versioned token contains ':' which tmux forbids in session names.
// If it appears in another field the row is rejected instead of misparsed.
const separator = "|:hmux-sep-v1:|"

// Retain the wire marker and name prefix so live views from earlier web
// connectors remain hidden during rolling upgrades.
const terminalViewOption = "@hmux_app_view"

var ErrSessionChanged = errors.New("session identity changed")

type Runner interface {
	Output(ctx context.Context, args ...string) ([]byte, error)
}

type TmuxRunner struct {
	Path string
	Env  []string
}

func (r TmuxRunner) Output(ctx context.Context, args ...string) ([]byte, error) {
	path := r.Path
	if path == "" {
		var err error
		path, err = TmuxPath()
		if err != nil {
			return nil, err
		}
	}
	command := exec.CommandContext(ctx, path, args...)
	if r.Env != nil {
		command.Env = r.Env
	}
	return safeexec.Output(command, 32*1024*1024)
}

func TmuxPath() (string, error) {
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

func Read(ctx context.Context, runner Runner) (model.Catalog, error) {
	return read(ctx, runner, true)
}

func ReadBasic(ctx context.Context, runner Runner) (model.Catalog, error) {
	return read(ctx, runner, false)
}

func read(ctx context.Context, runner Runner, inspectProcessState bool) (model.Catalog, error) {
	sessionFormat := strings.Join([]string{
		"#{session_id}", "#{session_name}", "#{session_created}", "#{session_activity}",
		"#{session_attached}", "#{session_windows}", "#{@hmux_app_view}",
		"#{session_group_attached}",
	}, separator)
	rawSessions, err := runner.Output(ctx, "list-sessions", "-F", sessionFormat)
	if err != nil {
		if noTmuxSessions(err) {
			return model.Catalog{
				ProtocolVersion: model.ProtocolVersion,
				GeneratedAt:     time.Now().UTC(),
				Sessions:        []model.Session{},
			}, nil
		}
		return model.Catalog{}, fmt.Errorf("tmux list-sessions: %w", err)
	}
	if len(rawSessions) > 16*1024*1024 {
		return model.Catalog{}, errors.New("tmux session output exceeds 16 MiB")
	}
	windowFormat := strings.Join([]string{
		"#{session_id}", "#{window_name}", "#{window_active}",
		"#{pane_current_path}", "#{pane_current_command}", "#{window_width}",
		"#{window_height}", "#{pane_pid}",
	}, separator)
	rawWindows, err := runner.Output(ctx, "list-windows", "-a", "-F", windowFormat)
	if err != nil {
		return model.Catalog{}, fmt.Errorf("tmux list-windows: %w", err)
	}
	if len(rawWindows) > 32*1024*1024 {
		return model.Catalog{}, errors.New("tmux window output exceeds 32 MiB")
	}

	byID := map[string]*model.Session{}
	sessionLines := lines(rawSessions)
	if len(sessionLines) > 10000 {
		return model.Catalog{}, errors.New("tmux session count exceeds limit")
	}
	for _, line := range sessionLines {
		fields := strings.Split(line, separator)
		if len(fields) != 8 {
			return model.Catalog{}, fmt.Errorf("malformed tmux session row: got %d fields", len(fields))
		}
		if err := model.ValidateSessionID(fields[0]); err != nil {
			return model.Catalog{}, err
		}
		// Browser terminal views are short-lived grouped sessions. They
		// share only the target's windows and keep independent session options,
		// so hiding one here never hides or mutates the user's real session.
		if fields[6] == "1" {
			continue
		}
		if byID[fields[0]] != nil {
			return model.Catalog{}, fmt.Errorf("duplicate tmux session id %q", fields[0])
		}
		createdAt, err := parseInteger(fields[2], "session_created")
		if err != nil {
			return model.Catalog{}, err
		}
		activityAt, err := parseInteger(fields[3], "session_activity")
		if err != nil {
			return model.Catalog{}, err
		}
		attached, err := parseInteger(fields[4], "session_attached")
		if err != nil || attached < 0 || attached > 10000 {
			return model.Catalog{}, errors.New("invalid session_attached")
		}
		// A browser terminal attaches to a hidden grouped sibling rather than
		// directly to the catalog session. tmux's per-session count therefore
		// stays zero; the group count is the attachment state of the shared
		// windows represented by the visible session.
		if fields[7] != "" {
			attached, err = parseInteger(fields[7], "session_group_attached")
			if err != nil || attached < 0 || attached > 10000 {
				return model.Catalog{}, errors.New("invalid session_group_attached")
			}
		}
		windowCount, err := parseInteger(fields[5], "session_windows")
		if err != nil || windowCount < 0 || windowCount > 10000 {
			return model.Catalog{}, errors.New("invalid session_windows")
		}
		session := model.Session{
			ID:          fields[0],
			Name:        model.SafeText(fields[1], 512),
			CreatedAt:   createdAt,
			ActivityAt:  activityAt,
			Attached:    int(attached),
			WindowCount: int(windowCount),
		}
		byID[session.ID] = &session
	}
	windowLines := lines(rawWindows)
	if len(windowLines) > 100000 {
		return model.Catalog{}, errors.New("tmux window count exceeds limit")
	}
	for _, line := range windowLines {
		fields := strings.Split(line, separator)
		if len(fields) != 8 {
			return model.Catalog{}, fmt.Errorf("malformed tmux window row: got %d fields", len(fields))
		}
		session := byID[fields[0]]
		if session == nil {
			continue
		}
		name := model.SafeText(fields[1], 256)
		session.WindowNames = append(session.WindowNames, name)
		if fields[2] == "1" {
			width, err := parseDimension(fields[5], "window_width")
			if err != nil {
				return model.Catalog{}, err
			}
			height, err := parseDimension(fields[6], "window_height")
			if err != nil {
				return model.Catalog{}, err
			}
			panePID, err := parseProcessID(fields[7])
			if err != nil {
				return model.Catalog{}, err
			}
			session.ActiveWindow = name
			session.CurrentPath = model.SafeText(fields[3], 2048)
			session.CurrentCommand = model.SafeText(fields[4], 256)
			session.Width = width
			session.Height = height
			session.PanePID = panePID
		}
	}
	var metadata map[int]processMetadata
	if inspectProcessState {
		metadata = inspectProcesses(ctx, byID)
	}
	result := model.Catalog{ProtocolVersion: model.ProtocolVersion, GeneratedAt: time.Now().UTC(), Sessions: make([]model.Session, 0, len(byID))}
	for _, session := range byID {
		classify(session, metadata[session.PanePID])
		result.Sessions = append(result.Sessions, *session)
	}
	sort.Slice(result.Sessions, func(i, j int) bool {
		if result.Sessions[i].ActivityAt == result.Sessions[j].ActivityAt {
			return result.Sessions[i].ID < result.Sessions[j].ID
		}
		return result.Sessions[i].ActivityAt > result.Sessions[j].ActivityAt
	})
	return result, nil
}

func noTmuxSessions(err error) bool {
	message := strings.ToLower(model.SafeText(safeexec.Stderr(err), 4096))
	return strings.Contains(message, "no server running on ") ||
		(strings.Contains(message, "error connecting to ") &&
			strings.Contains(message, "no such file or directory"))
}

// TerminalViewCreateArgs creates a temporary grouped session that shares the
// target's windows while retaining independent session options. The marker
// keeps the implementation-only view out of the HMux catalog and status off
// affects only that temporary view, never the pre-existing target session.
func TerminalViewCreateArgs(id, viewName string) ([]string, error) {
	if err := model.ValidateSessionID(id); err != nil {
		return nil, err
	}
	if !validTerminalViewName(viewName) {
		return nil, errors.New("invalid terminal view name")
	}
	return []string{
		"new-session", "-d", "-s", viewName, "-t", id,
		";", "set-option", "-t", viewName, terminalViewOption, "1",
		";", "set-option", "-t", viewName, "status", "off",
	}, nil
}

func TerminalViewAttachArgs(viewName string, shared bool) ([]string, error) {
	if !validTerminalViewName(viewName) {
		return nil, errors.New("invalid terminal view name")
	}
	args := []string{"attach-session"}
	if !shared {
		args = append(args, "-d")
	}
	return append(args, "-t", viewName), nil
}

func TerminalViewKillArgs(viewName string) ([]string, error) {
	if !validTerminalViewName(viewName) {
		return nil, errors.New("invalid terminal view name")
	}
	return []string{"kill-session", "-t", viewName}, nil
}

func validTerminalViewName(value string) bool {
	if !strings.HasPrefix(value, "hmux-app-view-") || len(value) > 63 {
		return false
	}
	for _, character := range value {
		if (character < 'a' || character > 'z') &&
			(character < '0' || character > '9') && character != '-' {
			return false
		}
	}
	return true
}

func ReadLegacyMetadata(ctx context.Context, runner Runner) ([]model.Session, error) {
	format := strings.Join([]string{
		"#{session_id}", "#{session_name}", "#{session_created}",
		"#{@hmux_profile}", "#{@hmux_tags}", "#{@hmux_label}", "#{@hmux_alias}",
	}, separator)
	raw, err := runner.Output(ctx, "list-sessions", "-F", format)
	if err != nil {
		return nil, fmt.Errorf("tmux legacy metadata: %w", err)
	}
	var sessions []model.Session
	for _, line := range lines(raw) {
		fields := strings.Split(line, separator)
		if len(fields) != 7 {
			return nil, fmt.Errorf("malformed legacy tmux metadata row: got %d fields", len(fields))
		}
		if err := model.ValidateSessionID(fields[0]); err != nil {
			return nil, err
		}
		createdAt, err := parseInteger(fields[2], "session_created")
		if err != nil {
			return nil, err
		}
		session := model.Session{
			ID: fields[0], Name: model.SafeText(fields[1], 512), CreatedAt: createdAt,
			Profile: model.SafeText(fields[3], 128), Tags: splitTags(fields[4]),
			Label: model.SafeText(fields[5], 256), Alias: model.SafeText(fields[6], 128),
		}
		sessions = append(sessions, session)
	}
	return sessions, nil
}

func ClearLegacyMetadata(ctx context.Context, runner Runner, sessions []model.Session) error {
	for _, session := range sessions {
		if err := model.ValidateSessionID(session.ID); err != nil {
			return err
		}
		for _, option := range []string{
			"@hmux_profile", "@hmux_tags", "@hmux_label", "@hmux_alias",
		} {
			if _, err := runner.Output(ctx, "set-option", "-u", "-t", session.ID, option); err != nil {
				return fmt.Errorf("clear legacy tmux metadata %s: %w", option, err)
			}
		}
	}
	return nil
}

func TerminateSession(ctx context.Context, runner Runner, sessionID string) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if _, err := runner.Output(ctx, "kill-session", "-t", sessionID); err != nil {
		return fmt.Errorf("tmux terminate session: %w", err)
	}
	return nil
}

// TerminateSessionExpected checks the creation timestamp and kills the session
// inside a single tmux command queue so a recycled stable ID cannot be killed.
func TerminateSessionExpected(ctx context.Context, runner Runner, sessionID string, createdAt int64) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	condition := fmt.Sprintf("#{==:#{session_created},%d}", createdAt)
	output, err := runner.Output(
		ctx, "if-shell", "-F", "-t", sessionID, condition,
		"kill-session -t "+sessionID,
		"display-message -p hmux-session-changed",
	)
	if err != nil {
		return fmt.Errorf("tmux terminate expected session: %w", err)
	}
	if strings.TrimSpace(string(output)) != "" {
		return ErrSessionChanged
	}
	return nil
}

func lines(data []byte) []string {
	text := strings.TrimSuffix(string(data), "\n")
	if text == "" {
		return nil
	}
	return strings.Split(text, "\n")
}

func parseInteger(value, field string) (int64, error) {
	n, err := strconv.ParseInt(value, 10, 64)
	if err != nil || n < 0 {
		return 0, fmt.Errorf("invalid tmux %s", field)
	}
	return n, nil
}

func parseDimension(value, field string) (int, error) {
	if value == "" {
		return 0, nil
	}
	n, err := parseInteger(value, field)
	if err != nil || n > 100000 {
		return 0, fmt.Errorf("invalid tmux %s", field)
	}
	return int(n), nil
}

func parseProcessID(value string) (int, error) {
	if value == "" {
		return 0, nil
	}
	n, err := parseInteger(value, "pane_pid")
	if err != nil || n < 1 || n > 1<<30 {
		return 0, errors.New("invalid tmux pane_pid")
	}
	return int(n), nil
}

func splitTags(value string) []string {
	var result []string
	for _, tag := range strings.Split(value, ",") {
		tag = model.SafeText(tag, 64)
		if tag != "" {
			result = append(result, tag)
		}
	}
	return result
}

func classify(session *model.Session, metadata processMetadata) {
	session.Runtime = metadata.Runtime
	session.Model = metadata.Model
	session.State = metadata.State
	session.Process = metadata.Process
	session.WorkingSince = metadata.WorkingSince
	if session.Runtime == "" {
		session.Runtime = "process"
	}
	if session.Process == "" {
		session.Process = model.SafeText(session.CurrentCommand, 128)
	}
	if session.Process == "" {
		session.Process = "shell"
	}
	if session.State == "" {
		if session.Runtime == "process" {
			session.State = "running"
		} else {
			session.State = "unknown"
		}
	}
	switch session.Runtime {
	case "codex", "claude":
		session.Kind = session.Runtime
	default:
		session.Runtime = "process"
		session.Kind = "shell"
		session.Model = ""
		session.State = "running"
	}
}

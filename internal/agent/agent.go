package agent

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/recovery"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/sessionstate"
	"github.com/codemoo/hmux/internal/workflow"
)

func Catalog(ctx context.Context) (model.Catalog, error) {
	return CatalogAt(ctx, config.DefaultClientConfig().StateDir)
}

func CatalogAt(ctx context.Context, stateDir string) (model.Catalog, error) {
	value, err := catalog.Read(ctx, catalog.TmuxRunner{})
	if err != nil {
		return value, err
	}
	if err := (sessionstate.Store{StateDir: stateDir}).Apply(&value); err != nil {
		return value, fmt.Errorf("session metadata: %w", err)
	}
	if err := (sessionstate.Store{StateDir: stateDir}).ApplyVisibility(&value); err != nil {
		return value, fmt.Errorf("session visibility: %w", err)
	}
	_ = (recovery.Store{StateDir: stateDir}).Apply(&value)
	// Workflow state is an optional observability overlay. A corrupt or
	// unavailable store must not take down the session catalog or attach path.
	_ = (workflow.Store{StateDir: stateDir}).Apply(&value)
	for index := range value.Sessions {
		value.Sessions[index].HostAlias = "hmux-home"
	}
	return value, nil
}

func Preview(ctx context.Context, id string) (string, error) {
	if err := model.ValidateSessionID(id); err != nil {
		return "", err
	}
	value, err := Catalog(ctx)
	if err != nil {
		return "", err
	}
	for _, session := range value.Sessions {
		if session.ID == id {
			return FormatPreview(session), nil
		}
	}
	return "", fmt.Errorf("session %s does not exist", id)
}

func FormatPreview(s model.Session) string {
	preview := fmt.Sprintf(
		"Session: %s\nID: %s\nRuntime: %s\nModel: %s\nState: %s\nProcess: %s\nProfile: %s\nTags: %s\nPath: %s\nCommand: %s\nWindows: %s\nAttached: %d\nSize: %dx%d\nActivity: %s\n",
		model.SafeText(s.Name, 512), model.SafeText(s.ID, 32),
		emptyDash(model.SafeText(s.Runtime, 128)), emptyDash(model.SafeText(s.Model, 128)),
		emptyDash(model.SafeText(s.State, 64)), emptyDash(model.SafeText(s.Process, 128)),
		emptyDash(model.SafeText(s.Profile, 128)),
		emptyDash(model.SafeText(strings.Join(s.Tags, ", "), 4096)),
		emptyDash(model.SafeText(s.CurrentPath, 4096)),
		emptyDash(model.SafeText(s.CurrentCommand, 256)),
		emptyDash(model.SafeText(strings.Join(s.WindowNames, ", "), 4096)),
		s.Attached, s.Width, s.Height, time.Unix(s.ActivityAt, 0).Format(time.RFC3339),
	)
	if badge := workflow.SummaryBadge(s.Workflow); badge != "" {
		preview += "Workflow: " + badge + "\n"
	}
	return preview
}

type CreateResult struct {
	ID        string `json:"id"`
	CreatedAt int64  `json:"created_at"`
	Reused    bool   `json:"reused"`
}

func Create(ctx context.Context, inventory model.Inventory, profileID, requestedName, stateDir string) (string, error) {
	result, err := CreateSession(ctx, inventory, profileID, requestedName, stateDir)
	return result.ID, err
}

func CreateSession(ctx context.Context, inventory model.Inventory, profileID, requestedName, stateDir string) (CreateResult, error) {
	profile, name, directory, commandPath, err := prepareCreate(inventory, profileID, requestedName)
	if err != nil {
		return CreateResult{}, err
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return CreateResult{}, err
	}
	const identityFormat = "#{session_id} #{session_created}"
	if requestedName != "" && exec.CommandContext(ctx, tmuxPath, "has-session", "-t", "="+name).Run() == nil {
		// display-message takes a pane target; ':' resolves the exact session's
		// current window instead of treating its name as a pane/window name.
		output, err := safeexec.Output(exec.CommandContext(ctx, tmuxPath, "display-message", "-p", "-t", "="+name+":", identityFormat), 4096)
		if err != nil {
			return CreateResult{}, fmt.Errorf("resolve existing session: %w", err)
		}
		result, err := parseCreatedIdentity(output)
		result.Reused = true
		// Reusing a name must not relabel an existing session with a different profile.
		return result, err
	}
	args := []string{"new-session", "-d", "-P", "-F", identityFormat, "-s", name, "-c", directory}
	command := append([]string{commandPath}, profile.Command[1:]...)
	args = append(args, shellCommand(command))
	output, err := safeexec.Output(exec.CommandContext(ctx, tmuxPath, args...), 4096)
	if err != nil {
		return CreateResult{}, fmt.Errorf("tmux new-session: %w", err)
	}
	result, err := parseCreatedIdentity(output)
	if err != nil {
		return CreateResult{}, fmt.Errorf("new tmux session %q was left running; %w", name, err)
	}
	// tmux reports identity in the create command itself. Never resolve it by
	// name again, which could select a replacement created after this command.
	session := model.Session{ID: result.ID, CreatedAt: result.CreatedAt, Name: name}
	if err := (sessionstate.Store{StateDir: stateDir}).SetProfile(session, *profile); err != nil {
		return CreateResult{}, fmt.Errorf("new tmux session %q was left running; save profile metadata: %w", name, err)
	}
	return result, nil
}

func parseCreatedIdentity(output []byte) (CreateResult, error) {
	fields := strings.Fields(string(output))
	if len(fields) != 2 {
		return CreateResult{}, errors.New("invalid tmux create identity")
	}
	if err := model.ValidateSessionID(fields[0]); err != nil {
		return CreateResult{}, err
	}
	createdAt, err := strconv.ParseInt(fields[1], 10, 64)
	if err != nil || createdAt < 1 {
		return CreateResult{}, errors.New("invalid tmux creation time")
	}
	return CreateResult{ID: fields[0], CreatedAt: createdAt}, nil
}

func SetAlias(ctx context.Context, stateDir, id, alias string) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	return (sessionstate.Store{StateDir: stateDir}).SetAliasCurrent(ctx, id, alias, sessionByID)
}

func SetAliasExpected(ctx context.Context, stateDir, id string, createdAt int64, alias string) error {
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	err := (sessionstate.Store{StateDir: stateDir}).SetAliasExpected(ctx, id, createdAt, alias, sessionByID)
	if errors.Is(err, sessionstate.ErrSessionChanged) {
		return catalog.ErrSessionChanged
	}
	return err
}

func SetHiddenExpected(ctx context.Context, stateDir, id string, createdAt int64, hidden bool) error {
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	err := (sessionstate.Store{StateDir: stateDir}).SetHiddenExpected(ctx, id, createdAt, hidden, sessionByID)
	if errors.Is(err, sessionstate.ErrSessionChanged) {
		return catalog.ErrSessionChanged
	}
	return err
}

func MigrateLegacyMetadata(ctx context.Context, stateDir string, clear bool) (int, error) {
	runner := catalog.TmuxRunner{}
	sessions, err := catalog.ReadLegacyMetadata(ctx, runner)
	if err != nil {
		return 0, err
	}
	if err := (sessionstate.Store{StateDir: stateDir}).Import(sessions); err != nil {
		return 0, err
	}
	if clear {
		if err := catalog.ClearLegacyMetadata(ctx, runner, sessions); err != nil {
			return 0, err
		}
	}
	return len(sessions), nil
}

func ValidateCreate(inventory model.Inventory, profileID, requestedName string) (string, error) {
	_, name, _, _, err := prepareCreate(inventory, profileID, requestedName)
	return name, err
}

func prepareCreate(inventory model.Inventory, profileID, requestedName string) (*model.Profile, string, string, string, error) {
	var profile *model.Profile
	for index := range inventory.Profiles {
		if inventory.Profiles[index].ID == profileID {
			profile = &inventory.Profiles[index]
			break
		}
	}
	if profile == nil {
		return nil, "", "", "", fmt.Errorf("unknown profile %q", profileID)
	}
	name := requestedName
	if name == "" {
		var suffix [8]byte
		if _, err := rand.Read(suffix[:]); err != nil {
			return nil, "", "", "", fmt.Errorf("generate session name: %w", err)
		}
		// A profile ID is at most 63 ASCII characters, so the generated name
		// remains within tmux's HMux limit of 80. Never reuse automatic names.
		name = fmt.Sprintf("%s-%x", profile.ID, suffix)
	}
	if !validSessionName(name) {
		return nil, "", "", "", errors.New("session name must be 1-80 safe letters, numbers, spaces, '_' or '-' and cannot contain ':' or '.'")
	}
	directory, err := expandHome(profile.DefaultDirectory)
	if err != nil {
		return nil, "", "", "", err
	}
	info, err := os.Stat(directory)
	if err != nil {
		return nil, "", "", "", fmt.Errorf("profile directory: %w", err)
	}
	if !info.IsDir() {
		return nil, "", "", "", fmt.Errorf("profile path %q is not a directory", directory)
	}
	if len(profile.Command) == 0 {
		return nil, "", "", "", errors.New("profile command is empty")
	}
	commandPath, err := executablePath(profile.Command[0])
	if err != nil {
		return nil, "", "", "", fmt.Errorf("profile command %q: %w", profile.Command[0], err)
	}
	return profile, name, directory, commandPath, nil
}

func validSessionName(name string) bool {
	count := utf8.RuneCountInString(name)
	if count < 1 || count > 80 {
		return false
	}
	for _, r := range name {
		if unicode.IsLetter(r) || unicode.IsNumber(r) || r == ' ' || r == '_' || r == '-' {
			continue
		}
		return false
	}
	return true
}

func shellCommand(args []string) string {
	quoted := make([]string, 0, len(args))
	for _, arg := range args {
		quoted = append(quoted, "'"+strings.ReplaceAll(arg, "'", "'\"'\"'")+"'")
	}
	return strings.Join(quoted, " ")
}

func sessionByID(ctx context.Context, id string) (model.Session, error) {
	value, err := catalog.ReadBasic(ctx, catalog.TmuxRunner{})
	if err != nil {
		return model.Session{}, err
	}
	for _, session := range value.Sessions {
		if session.ID == id {
			return session, nil
		}
	}
	return model.Session{}, fmt.Errorf("session %s does not exist: %w", id, catalog.ErrSessionChanged)
}

func executablePath(name string) (string, error) {
	if strings.ContainsRune(name, os.PathSeparator) {
		return "", errors.New("profile executable must be a command name, not a path")
	}
	if path, err := exec.LookPath(name); err == nil {
		return path, nil
	}
	home, _ := os.UserHomeDir()
	for _, directory := range []string{filepath.Join(home, ".local", "bin"), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"} {
		path := filepath.Join(directory, name)
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path, nil
		}
	}
	return "", fmt.Errorf("executable not found")
}

func expandHome(path string) (string, error) {
	if path == "~" || strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		if path == "~" {
			return home, nil
		}
		return filepath.Join(home, strings.TrimPrefix(path, "~/")), nil
	}
	if !filepath.IsAbs(path) {
		return "", fmt.Errorf("profile path must be absolute or start with ~/")
	}
	return path, nil
}

func emptyDash(value string) string {
	if value == "" {
		return "-"
	}
	return value
}

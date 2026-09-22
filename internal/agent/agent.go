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
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/recovery"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/sessionlaunch"
	"github.com/codemoo/hmux/internal/sessionstate"
	"github.com/codemoo/hmux/internal/workflow"
)

func Catalog(ctx context.Context) (model.Catalog, error) {
	return CatalogAt(ctx, config.DefaultHomeConfig().StateDir)
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
	return value, nil
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
	profile, folder, baseDirectory, commandPath, err := prepareCreate(inventory, profileID, requestedName)
	if err != nil {
		return CreateResult{}, err
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return CreateResult{}, err
	}
	directory, name, err := allocateWorkspace(baseDirectory, folder, profile.ID)
	if err != nil {
		return CreateResult{}, err
	}
	const identityFormat = "#{session_id} #{session_created}"
	args := []string{"new-session", "-d", "-P", "-F", identityFormat, "-s", name, "-c", directory}
	command := append([]string{commandPath}, profile.Command[1:]...)
	if profile.Command[0] == "codex" || profile.Command[0] == "claude" {
		command = sessionlaunch.Provider(command)
	}
	// Multiple argv make tmux exec directly, without its default-shell parser.
	// A single executable needs an explicit exec wrapper to avoid shell expansion.
	if len(command) == 1 {
		command = []string{"/bin/sh", "-c", `exec "$1"`, "hmux-launch", command[0]}
	}
	args = append(args, command...)
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
		name = profile.ID
	}
	if !utf8.ValidString(name) || utf8.RuneCountInString(name) > 80 {
		return nil, "", "", "", errors.New("session name must contain at most 80 characters")
	}
	for _, r := range name {
		if unicode.IsControl(r) {
			return nil, "", "", "", errors.New("session name cannot contain control characters")
		}
	}
	name = workspaceSlug(name, 36)
	directory, err := expandHome(profile.DefaultDirectory)
	if err != nil {
		return nil, "", "", "", err
	}
	directory = filepath.Clean(directory)
	if directory == string(os.PathSeparator) {
		return nil, "", "", "", errors.New("workspace base cannot be the filesystem root")
	}
	// Validation has no side effects. A missing base is created only on actual create.
	if info, err := os.Stat(directory); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, "", "", "", fmt.Errorf("workspace base: %w", err)
	} else if err == nil && !info.IsDir() {
		return nil, "", "", "", errors.New("workspace base is not a directory")
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

// workspaceSlug converts display text to one bounded filesystem component.
// No separators, dot segments, shell syntax or leading option characters survive.
func workspaceSlug(value string, limit int) string {
	var result []rune
	for _, r := range strings.TrimSpace(value) {
		if unicode.IsLetter(r) || unicode.IsNumber(r) || r == '_' || r == '-' {
			result = append(result, r)
		} else if len(result) > 0 && result[len(result)-1] != '-' {
			result = append(result, '-')
		}
		if len(result) == limit {
			break
		}
	}
	slug := strings.Trim(string(result), "-_")
	if slug == "" {
		return "session"
	}
	return slug
}

func allocateWorkspace(base, folder, profile string) (string, string, error) {
	if err := os.MkdirAll(base, 0700); err != nil {
		return "", "", fmt.Errorf("create workspace base: %w", err)
	}
	// Pin the administrator-selected base. Root.Mkdir never follows a child
	// symlink; existing directories/files/links receive a different name.
	root, err := os.OpenRoot(base)
	if err != nil {
		return "", "", err
	}
	defer root.Close()
	for attempt := 0; attempt < 32; attempt++ {
		var random [6]byte
		if _, err := rand.Read(random[:]); err != nil {
			return "", "", err
		}
		suffix := fmt.Sprintf("%x", random)
		child := folder
		if attempt > 0 {
			child += "-" + suffix
		}
		if err := root.Mkdir(child, 0700); errors.Is(err, os.ErrExist) {
			continue
		} else if err != nil {
			return "", "", fmt.Errorf("create session directory: %w", err)
		}
		return filepath.Join(base, child), child + "-" + workspaceSlug(profile, 16) + "-" + suffix, nil
	}
	return "", "", errors.New("could not allocate a unique session directory")
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

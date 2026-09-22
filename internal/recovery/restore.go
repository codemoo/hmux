package recovery

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sessionlaunch"
	"github.com/codemoo/hmux/internal/sessionstate"
)

var createdSessionFormat = strings.Join([]string{
	"#{session_id}", "#{window_id}", "#{pane_id}", "#{session_created}", "#{window_index}",
}, recoverySeparator)

var createdWindowFormat = strings.Join([]string{
	"#{window_id}", "#{pane_id}", "#{window_index}",
}, recoverySeparator)

func (s Store) restoreSnapshot(ctx context.Context, current *diskState) error {
	live, err := catalog.ReadBasic(ctx, s.runner())
	if err != nil {
		return err
	}
	byName := make(map[string]model.Session, len(live.Sessions))
	byIdentity := make(map[string]model.Session, len(live.Sessions))
	for _, session := range live.Sessions {
		byName[session.Name] = session
		byIdentity[identityKey(model.SessionIdentity{ID: session.ID, CreatedAt: session.CreatedAt})] = session
	}
	for _, saved := range current.Pending.Snapshot.Sessions {
		if err := ctx.Err(); err != nil {
			return err
		}
		key := identityKey(saved.Identity)
		if _, exists := byIdentity[key]; exists {
			continue // the original lifetime is already present
		}
		if completed, ok := current.Pending.Completed[key]; ok {
			// Once a session was durably completed, its deletion during the same
			// boot is intentional from the recovery engine's point of view.
			if session, exists := byIdentity[identityKey(completed.To)]; exists && session.Name != completed.Name && session.Name != temporarySessionName(completed.Gate) {
				return errors.New("restored tmux identity changed unexpectedly")
			}
			if _, exists := byIdentity[identityKey(completed.To)]; !exists {
				return errors.New("partially restored tmux session disappeared")
			}
			if err := s.launchProviders(ctx, current, saved, completed); err != nil {
				return fmt.Errorf("finish restored tmux session %q: %w", saved.Name, err)
			}
			continue
		}
		if _, exists := byName[saved.Name]; exists {
			continue
		}
		if current.Pending.Intents == nil {
			current.Pending.Intents = map[string]string{}
		}
		gate := current.Pending.Intents[key]
		if gate == "" {
			dir, err := os.MkdirTemp(s.root(), "launch-")
			if err != nil {
				return err
			}
			gate = filepath.Join(dir, "ready")
			current.Pending.Intents[key] = gate
			if err := s.writeState(*current); err != nil {
				return err
			}
		}
		if err := s.validateGate(gate); err != nil {
			return err
		}
		temporary := temporarySessionName(gate)
		if orphan, exists := byName[temporary]; exists {
			raw, err := s.runner().Output(ctx, "display-message", "-p", "-t", orphan.ID, "#{pane_start_command}")
			if err != nil || !strings.Contains(string(raw), gate) {
				return errors.New("recovery temporary session ownership changed")
			}
			if err := catalog.TerminateSessionExpected(ctx, s.runner(), orphan.ID, orphan.CreatedAt); err != nil {
				return err
			}
		}
		mapping, err := s.createSession(ctx, saved, gate)
		if err != nil {
			return fmt.Errorf("restore tmux session %q: %w", saved.Name, err)
		}
		current.Pending.Completed[key] = mapping
		if err := s.writeState(*current); err != nil {
			_ = catalog.TerminateSessionExpected(context.Background(), s.runner(), mapping.To.ID, mapping.To.CreatedAt)
			delete(current.Pending.Completed, key)
			return fmt.Errorf("persist restored tmux identity: %w", err)
		}
		if err := s.launchProviders(ctx, current, saved, mapping); err != nil {
			return fmt.Errorf("launch restored providers in %q: %w", saved.Name, err)
		}
		byName[saved.Name] = model.Session{ID: mapping.To.ID, Name: mapping.Name, CreatedAt: mapping.To.CreatedAt}
		byIdentity[identityKey(mapping.To)] = byName[saved.Name]
	}
	return nil
}

func (s Store) createSession(ctx context.Context, saved savedSession, gate string) (mapping restoredIdentity, resultErr error) {
	runner := s.runner()
	executables, err := providerExecutables(saved)
	if err != nil {
		return mapping, err
	}

	firstWindow := saved.Windows[0]
	firstPane := firstWindow.Panes[0]
	args := []string{"new-session", "-d", "-P", "-F", createdSessionFormat,
		"-s", temporarySessionName(gate), "-n", firstWindow.Name, "-c", firstPane.Cwd}
	args = appendGatedPaneCommand(args, firstPane, executables, gate)
	raw, err := runner.Output(ctx, args...)
	if err != nil {
		return mapping, err
	}
	fields := strings.Split(strings.TrimSpace(string(raw)), recoverySeparator)
	if len(fields) != 5 || !validTmuxID(fields[0], '$') || !validTmuxID(fields[1], '@') || !validTmuxID(fields[2], '%') {
		return mapping, errors.New("tmux returned an invalid new-session identity")
	}
	createdAt, err := strconv.ParseInt(fields[3], 10, 64)
	if err != nil || createdAt < 1 {
		return mapping, errors.New("tmux returned an invalid session creation time")
	}
	initialIndex, err := parseNonnegative(fields[4], "created window index")
	if err != nil {
		return mapping, err
	}
	sessionID := fields[0]
	created := true
	defer func() {
		if resultErr != nil && created {
			_ = catalog.TerminateSessionExpected(context.Background(), runner, sessionID, createdAt)
		}
	}()

	type restoredWindow struct {
		id      string
		paneIDs []string
		value   savedWindow
	}
	restored := make([]restoredWindow, 0, len(saved.Windows))
	first := restoredWindow{id: fields[1], paneIDs: []string{fields[2]}, value: firstWindow}
	if initialIndex != firstWindow.Index {
		if _, err := runner.Output(ctx, "move-window", "-s", first.id, "-t", fmt.Sprintf("%s:%d", sessionID, firstWindow.Index)); err != nil {
			return mapping, err
		}
	}
	restored = append(restored, first)

	for index := 1; index < len(saved.Windows); index++ {
		window := saved.Windows[index]
		pane := window.Panes[0]
		args := []string{"new-window", "-d", "-P", "-F", createdWindowFormat,
			"-t", fmt.Sprintf("%s:%d", sessionID, window.Index), "-n", window.Name, "-c", pane.Cwd}
		args = appendGatedPaneCommand(args, pane, executables, gate)
		raw, err := runner.Output(ctx, args...)
		if err != nil {
			return mapping, err
		}
		fields := strings.Split(strings.TrimSpace(string(raw)), recoverySeparator)
		if len(fields) != 3 || !validTmuxID(fields[0], '@') || !validTmuxID(fields[1], '%') || fields[2] != strconv.Itoa(window.Index) {
			return mapping, errors.New("tmux returned an invalid new-window identity")
		}
		restored = append(restored, restoredWindow{id: fields[0], paneIDs: []string{fields[1]}, value: window})
	}

	for wi := range restored {
		window := &restored[wi]
		for paneIndex := 1; paneIndex < len(window.value.Panes); paneIndex++ {
			pane := window.value.Panes[paneIndex]
			args := []string{"split-window", "-d", "-P", "-F", "#{pane_id}",
				"-t", window.paneIDs[len(window.paneIDs)-1], "-c", pane.Cwd}
			args = appendGatedPaneCommand(args, pane, executables, gate)
			raw, err := runner.Output(ctx, args...)
			if err != nil {
				return mapping, err
			}
			paneID := strings.TrimSpace(string(raw))
			if !validTmuxID(paneID, '%') {
				return mapping, errors.New("tmux returned an invalid split pane identity")
			}
			window.paneIDs = append(window.paneIDs, paneID)
			if _, err := runner.Output(ctx, "select-layout", "-t", window.id, "tiled"); err != nil {
				return mapping, err
			}
		}
		if _, err := runner.Output(ctx, "select-layout", "-t", window.id, window.value.Layout); err != nil {
			return mapping, err
		}
		for index, pane := range window.value.Panes {
			if pane.Active {
				if _, err := runner.Output(ctx, "select-pane", "-t", window.paneIDs[index]); err != nil {
					return mapping, err
				}
				break
			}
		}
	}
	for _, window := range restored {
		if window.value.Active {
			if _, err := runner.Output(ctx, "select-window", "-t", window.id); err != nil {
				return mapping, err
			}
			break
		}
	}

	verifyFormat := strings.Join([]string{"#{session_id}", "#{session_name}", "#{session_created}"}, recoverySeparator)
	raw, err = runner.Output(ctx, "display-message", "-p", "-t", sessionID, verifyFormat)
	if err != nil {
		return mapping, err
	}
	verified := strings.Split(strings.TrimSpace(string(raw)), recoverySeparator)
	if len(verified) != 3 || verified[0] != sessionID || verified[1] != temporarySessionName(gate) || verified[2] != strconv.FormatInt(createdAt, 10) {
		return mapping, errors.New("created tmux session identity did not verify")
	}

	newSession := model.Session{
		ID: sessionID, Name: saved.Name, CreatedAt: createdAt, Alias: saved.Alias,
		Profile: saved.Profile, Label: saved.Label, Tags: append([]string(nil), saved.Tags...),
	}
	metadata := sessionstate.Store{StateDir: s.StateDir}
	if err := metadata.Import([]model.Session{newSession}); err != nil {
		return mapping, fmt.Errorf("restore session metadata: %w", err)
	}
	if saved.Hidden {
		if err := metadata.SetHidden(newSession, true); err != nil {
			return mapping, fmt.Errorf("restore session visibility: %w", err)
		}
	}
	mapping = restoredIdentity{
		From: saved.Identity,
		To:   model.SessionIdentity{ID: sessionID, CreatedAt: createdAt},
		Name: saved.Name, Panes: map[string]string{}, Gate: gate,
	}
	for _, window := range restored {
		for index, pane := range window.value.Panes {
			mapping.Panes[positionKey(window.value.Index, pane.Index)] = window.paneIDs[index]
		}
	}
	created = false
	return mapping, nil
}

// Releasing a private gate is idempotent: each already-created pane execs its
// fixed resume argv exactly once. Retrying never respawns or kills live work.
func (s Store) launchProviders(ctx context.Context, current *diskState, saved savedSession, mapping restoredIdentity) error {
	if mapping.Gate == "" {
		return nil
	}
	if err := s.validateGate(mapping.Gate); err != nil {
		return err
	}
	if err := s.verifyRestoredPanes(ctx, mapping); err != nil {
		return err
	}
	raw, err := s.runner().Output(ctx, "display-message", "-p", "-t", mapping.To.ID, "#{session_name}")
	if err != nil {
		return err
	}
	name := strings.TrimSpace(string(raw))
	if name == temporarySessionName(mapping.Gate) {
		if _, err := s.runner().Output(ctx, "rename-session", "-t", mapping.To.ID, mapping.Name); err != nil {
			return err
		}
	} else if name != mapping.Name {
		return catalog.ErrSessionChanged
	}

	return config.AtomicWrite(mapping.Gate, []byte("ready\n"), 0600)
}

const providerGateScript = `while [ ! -f "$1" ]; do /bin/sleep 1; done; shift; exec "$@"`

func appendGatedPaneCommand(args []string, pane savedPane, executables map[string]string, gate string) []string {

	// Obtain only fixed environment/argv from the shared command builder.
	command := appendPaneCommand(nil, pane, executables)
	if pane.Resume == nil {
		command = []string{sessionlaunch.Shell(), "-il"}
	}
	split := 0
	for split+1 < len(command) && command[split] == "-e" {
		args = append(args, command[split:split+2]...)
		split += 2
	}
	launch := command[split:]
	if pane.Resume != nil {
		launch = sessionlaunch.Provider(launch)
	}
	args = append(args, "/bin/sh", "-c", providerGateScript, "hmux-recovery", gate)
	return append(args, launch...)
}

func (s Store) validateGate(path string) error {
	if filepath.Base(path) != "ready" || filepath.Dir(filepath.Dir(path)) != s.root() || !strings.HasPrefix(filepath.Base(filepath.Dir(path)), "launch-") {
		return errors.New("invalid recovery gate")
	}
	return ensurePrivateDirectory(filepath.Dir(path))
}

func providerExecutables(saved savedSession) (map[string]string, error) {
	result := map[string]string{}
	for _, window := range saved.Windows {
		for _, pane := range window.Panes {
			if pane.Resume == nil || result[pane.Resume.Provider] != "" {
				continue
			}
			path, err := resolveProviderExecutable(pane.Resume.Provider)
			if err != nil {
				return nil, err
			}
			result[pane.Resume.Provider] = path
		}
	}
	return result, nil
}

func positionKey(window, pane int) string { return fmt.Sprintf("%d/%d", window, pane) }

func appendPaneCommand(args []string, pane savedPane, executables map[string]string) []string {
	if pane.Resume == nil {
		return args
	}
	reference := pane.Resume
	executable := executables[reference.Provider]
	if info, err := os.Stat(filepath.Join(filepath.Dir(executable), "node")); err == nil && info.Mode().IsRegular() && info.Mode()&0111 != 0 {
		args = append(args, "-e", "PATH="+filepath.Dir(executable)+":/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin")
	}
	if reference.Provider == "codex" {
		return append(args, "-e", "CODEX_HOME="+reference.ConfigDir,
			executables[reference.Provider], "resume", reference.SessionID)
	}
	return append(args, "-e", "CLAUDE_CONFIG_DIR="+reference.ConfigDir,
		executables[reference.Provider], "--resume", reference.SessionID)
}

func rebaseSnapshot(value snapshot, completed map[string]restoredIdentity) snapshot {
	result := snapshot{}
	for _, session := range cloneSnapshot(value).Sessions {
		if mapping, ok := completed[identityKey(session.Identity)]; ok {
			session.Identity = mapping.To
			result.Sessions = append(result.Sessions, session)
		}
	}
	return result
}

func resolveProviderExecutable(provider string) (string, error) {
	if provider != "codex" && provider != "claude" {
		return "", errors.New("unsupported recovery provider")
	}
	if path, err := exec.LookPath(provider); err == nil {
		if resolved, err := validateExecutable(path); err == nil {
			return resolved, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	candidates := []string{
		filepath.Join(home, ".local", "bin", provider),
		filepath.Join("/opt/homebrew/bin", provider),
		filepath.Join("/usr/local/bin", provider),
	}
	versionsRoot := filepath.Join(home, ".nvm", "versions", "node")
	entries, readErr := os.ReadDir(versionsRoot)
	if readErr == nil {
		if len(entries) > 128 {
			return "", errors.New("too many Node installations to resolve provider executable")
		}
		for _, entry := range entries {
			if entry.IsDir() && entry.Type()&os.ModeSymlink == 0 {
				candidates = append(candidates, filepath.Join(versionsRoot, entry.Name(), "bin", provider))
			}
		}
	}
	unique := map[string]bool{}
	var found []string
	for _, candidate := range candidates {
		resolved, err := validateExecutable(candidate)
		if err == nil && !unique[resolved] {
			unique[resolved] = true
			found = append(found, resolved)
		}
	}
	sort.Strings(found)
	if len(found) != 1 {
		return "", fmt.Errorf("provider executable %q is unavailable or ambiguous", provider)
	}
	return found[0], nil
}

func validateExecutable(path string) (string, error) {
	if !filepath.IsAbs(path) {
		absolute, err := filepath.Abs(path)
		if err != nil {
			return "", err
		}
		path = absolute
	}
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil || !filepath.IsAbs(resolved) {
		return "", errors.New("provider executable is unavailable")
	}
	info, err := os.Stat(resolved)
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return "", errors.New("provider executable is not an executable regular file")
	}
	return filepath.Clean(path), nil
}

func temporarySessionName(gate string) string {
	return "hmux-recovery-" + strings.TrimPrefix(filepath.Base(filepath.Dir(gate)), "launch-")
}

func (s Store) verifyRestoredPanes(ctx context.Context, mapping restoredIdentity) error {
	raw, err := s.runner().Output(ctx, "display-message", "-p", "-t", mapping.To.ID, "#{session_id}"+recoverySeparator+"#{session_created}")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(raw)) != mapping.To.ID+recoverySeparator+strconv.FormatInt(mapping.To.CreatedAt, 10) {
		return catalog.ErrSessionChanged
	}
	format := strings.Join([]string{"#{pane_id}", "#{window_index}", "#{pane_index}"}, recoverySeparator)
	raw, err = s.runner().Output(ctx, "list-panes", "-s", "-t", mapping.To.ID, "-F", format)
	if err != nil {
		return err
	}
	seen := map[string]bool{}
	for _, line := range strings.Split(strings.TrimSpace(string(raw)), "\n") {
		parts := strings.Split(line, recoverySeparator)
		if len(parts) != 3 {
			return catalog.ErrSessionChanged
		}
		key := parts[1] + "/" + parts[2]
		if mapping.Panes[key] != parts[0] || seen[key] {
			return catalog.ErrSessionChanged
		}
		seen[key] = true
	}
	if len(seen) != len(mapping.Panes) {
		return catalog.ErrSessionChanged
	}
	return nil
}

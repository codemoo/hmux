package tabstate

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
	"unicode"

	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
)

const stateVersion = 2

type Store struct {
	StateDir string
}

type launcherState struct {
	Version     int      `json:"version"`
	LauncherID  string   `json:"launcher_id"`
	Sessions    []string `json:"sessions"`
	CurrentID   string   `json:"current_id,omitempty"`
	ClientName  string   `json:"client_name,omitempty"`
	ControlName string   `json:"control_name,omitempty"`
	UpdatedAt   string   `json:"updated_at"`
}

type clientBinding struct {
	Version    int    `json:"version"`
	ClientName string `json:"client_name"`
	LauncherID string `json:"launcher_id"`
	UpdatedAt  string `json:"updated_at"`
}

type FrameState struct {
	LauncherID  string
	Sessions    []string
	CurrentID   string
	ClientName  string
	ControlName string
}

type LauncherTabs struct {
	LauncherID string   `json:"launcher_id"`
	Sessions   []string `json:"sessions"`
	CurrentID  string   `json:"current_id,omitempty"`
}

func ValidateLauncherID(value string) error {
	if len(value) != 32 {
		return errors.New("launcher ID must contain 32 lowercase hexadecimal characters")
	}
	for _, r := range value {
		if (r < '0' || r > '9') && (r < 'a' || r > 'f') {
			return errors.New("launcher ID must contain 32 lowercase hexadecimal characters")
		}
	}
	return nil
}

func ValidateClientName(value string) error {
	if len(value) < 6 || len(value) > 512 || !strings.HasPrefix(value, "/dev/") {
		return errors.New("invalid tmux client name")
	}
	for _, r := range value {
		if unicode.IsLetter(r) || unicode.IsNumber(r) || strings.ContainsRune("/._-", r) {
			continue
		}
		return errors.New("invalid tmux client name")
	}
	return nil
}

func CurrentTTY() (string, error) {
	path, err := exec.LookPath("tty")
	if err != nil {
		return "", errors.New("tty executable not found")
	}
	command := exec.Command(path)
	command.Stdin = os.Stdin
	output, err := command.Output()
	if err != nil {
		return "", fmt.Errorf("resolve client tty: %w", err)
	}
	if len(output) > 1024 {
		return "", errors.New("client tty output is too large")
	}
	name := strings.TrimSpace(string(output))
	if err := ValidateClientName(name); err != nil {
		return "", err
	}
	return name, nil
}

func (s Store) OpenFrame(launcherID, clientName, controlName, sessionID string) error {
	if err := s.validate(launcherID, clientName); err != nil {
		return err
	}
	if err := ValidateClientName(controlName); err != nil {
		return err
	}
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if err := s.ensureDirs(); err != nil {
		return err
	}
	if err := s.withLauncherLock(launcherID, func() error {
		state, err := s.readLauncher(launcherID)
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		if errors.Is(err, os.ErrNotExist) {
			state = launcherState{Version: stateVersion, LauncherID: launcherID}
		}
		for _, existing := range state.Sessions {
			if existing == sessionID {
				state.Version = stateVersion
				state.CurrentID = sessionID
				state.ClientName = clientName
				state.ControlName = controlName
				state.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
				return s.writeLauncher(state)
			}
		}
		state.Sessions = append(state.Sessions, sessionID)
		state.Version = stateVersion
		state.CurrentID = sessionID
		state.ClientName = clientName
		state.ControlName = controlName
		state.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
		return s.writeLauncher(state)
	}); err != nil {
		return err
	}
	binding := clientBinding{
		Version: stateVersion, ClientName: clientName, LauncherID: launcherID,
		UpdatedAt: time.Now().UTC().Format(time.RFC3339Nano),
	}
	return s.atomicJSON(s.clientPath(clientName), binding)
}

func (s Store) Frame(launcherID string) (FrameState, error) {
	if err := ValidateLauncherID(launcherID); err != nil {
		return FrameState{}, err
	}
	state, err := s.readLauncher(launcherID)
	if err != nil {
		return FrameState{}, err
	}
	if state.ClientName == "" || state.CurrentID == "" {
		return FrameState{}, errors.New("launcher has no active frame client")
	}
	if err := ValidateClientName(state.ClientName); err != nil {
		return FrameState{}, err
	}
	if state.ControlName == "" {
		state.ControlName = state.ClientName
	}
	if err := ValidateClientName(state.ControlName); err != nil {
		return FrameState{}, err
	}
	if !containsSession(state.Sessions, state.CurrentID) {
		return FrameState{}, errors.New("launcher current session is not an open tab")
	}
	return FrameState{
		LauncherID:  state.LauncherID,
		Sessions:    append([]string(nil), state.Sessions...),
		CurrentID:   state.CurrentID,
		ClientName:  state.ClientName,
		ControlName: state.ControlName,
	}, nil
}

// Tabs returns launcher-local visual tabs without requiring an attached frame
// client. This keeps the list screen able to render the launcher's open tabs
// after the disposable frame has closed, including the valid empty-tab state.
func (s Store) Tabs(launcherID string) (LauncherTabs, error) {
	if err := ValidateLauncherID(launcherID); err != nil {
		return LauncherTabs{}, err
	}
	state, err := s.readLauncher(launcherID)
	if err != nil {
		return LauncherTabs{}, err
	}
	if len(state.Sessions) == 0 {
		if state.CurrentID != "" {
			return LauncherTabs{}, errors.New("empty launcher tabs have a current session")
		}
	} else {
		if err := model.ValidateSessionID(state.CurrentID); err != nil {
			return LauncherTabs{}, errors.New("launcher tabs have no valid current session")
		}
		if !containsSession(state.Sessions, state.CurrentID) {
			return LauncherTabs{}, errors.New("launcher current session is not an open tab")
		}
	}
	return LauncherTabs{
		LauncherID: state.LauncherID,
		Sessions:   append([]string(nil), state.Sessions...),
		CurrentID:  state.CurrentID,
	}, nil
}

func (s Store) UpdateFrame(launcherID, clientName string, sessions []string, currentID string) error {
	if err := ValidateLauncherID(launcherID); err != nil {
		return err
	}
	if err := ValidateClientName(clientName); err != nil {
		return err
	}
	if err := validateSessions(sessions); err != nil {
		return err
	}
	if currentID == "" {
		if len(sessions) != 0 {
			return errors.New("a non-empty tab list requires a current session")
		}
	} else {
		if err := model.ValidateSessionID(currentID); err != nil {
			return err
		}
		if !containsSession(sessions, currentID) {
			return errors.New("current session is not an open tab")
		}
	}
	if err := s.ensureDirs(); err != nil {
		return err
	}
	return s.withLauncherLock(launcherID, func() error {
		state, err := s.readLauncher(launcherID)
		if err != nil {
			return err
		}
		if state.ClientName != clientName {
			return errors.New("frame client changed")
		}
		state.Version = stateVersion
		state.Sessions = append([]string{}, sessions...)
		state.CurrentID = currentID
		state.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
		return s.writeLauncher(state)
	})
}

func (s Store) Cleanup(launcherID string) error {
	if err := ValidateLauncherID(launcherID); err != nil {
		return err
	}
	if _, err := os.Lstat(s.root()); errors.Is(err, os.ErrNotExist) {
		return nil
	} else if err != nil {
		return err
	}
	if err := s.ensureDirs(); err != nil {
		return err
	}
	err := s.withLauncherLock(launcherID, func() error {
		path := s.launcherPath(launcherID)
		if info, err := os.Lstat(path); err == nil {
			if !info.Mode().IsRegular() {
				return errors.New("launcher state is not a regular file")
			}
			if err := os.Remove(path); err != nil {
				return err
			}
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		}
		entries, err := os.ReadDir(filepath.Join(s.root(), "clients"))
		if err != nil {
			return err
		}
		for _, entry := range entries {
			if entry.IsDir() {
				continue
			}
			path := filepath.Join(s.root(), "clients", entry.Name())
			var binding clientBinding
			if err := s.readJSON(path, &binding); err != nil {
				continue
			}
			if binding.LauncherID == launcherID {
				if err := os.Remove(path); err != nil {
					return err
				}
			}
		}
		return nil
	})
	if err != nil {
		return err
	}
	// Keep the lock inode. Removing a flock file after unlocking permits a
	// concurrent opener to retain the old inode while a third process locks a
	// newly created one, silently breaking mutual exclusion.
	return nil
}

func (s Store) validate(launcherID, clientName string) error {
	if err := ValidateLauncherID(launcherID); err != nil {
		return err
	}
	return ValidateClientName(clientName)
}

func (s Store) root() string {
	return filepath.Join(filepath.Clean(s.StateDir), "tabs")
}

func (s Store) launcherPath(launcherID string) string {
	return filepath.Join(s.root(), "launchers", launcherID+".json")
}

func (s Store) clientPath(clientName string) string {
	digest := sha256.Sum256([]byte(clientName))
	return filepath.Join(s.root(), "clients", hex.EncodeToString(digest[:])+".json")
}

func (s Store) lockPath(launcherID string) string {
	return filepath.Join(s.root(), "launchers", launcherID+".lock")
}

func (s Store) ensureDirs() error {
	root := filepath.Clean(s.StateDir)
	if !filepath.IsAbs(root) || root == string(os.PathSeparator) {
		return errors.New("tab state directory must be an absolute non-root path")
	}
	for _, dir := range []string{root, s.root(), filepath.Join(s.root(), "launchers"), filepath.Join(s.root(), "clients")} {
		if info, err := os.Lstat(dir); err == nil {
			if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
				return fmt.Errorf("tab state path is not a directory: %s", dir)
			}
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		} else if err := os.Mkdir(dir, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
			return err
		}
		if err := os.Chmod(dir, 0o700); err != nil {
			return err
		}
	}
	return nil
}

func (s Store) withLauncherLock(launcherID string, action func() error) error {
	path := s.lockPath(launcherID)
	if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
		return errors.New("launcher lock is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	lock, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(context.Background(), lock, 2*time.Second); err != nil {
		return fmt.Errorf("launcher state lock: %w", err)
	}
	defer filelock.Unlock(lock)
	return action()
}

func (s Store) readLauncher(launcherID string) (launcherState, error) {
	var state launcherState
	if err := s.readJSON(s.launcherPath(launcherID), &state); err != nil {
		return state, err
	}
	if (state.Version != 1 && state.Version != stateVersion) || state.LauncherID != launcherID {
		return state, errors.New("invalid launcher state")
	}
	if err := validateSessions(state.Sessions); err != nil {
		return state, err
	}
	return state, nil
}

func (s Store) readJSON(path string, destination any) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
		return errors.New("tab state is not a small regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, 1024*1024+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil {
		return err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return errors.New("tab state contains trailing data")
	}
	return nil
}

func (s Store) writeLauncher(state launcherState) error {
	return s.atomicJSON(s.launcherPath(state.LauncherID), state)
}

func (s Store) atomicJSON(path string, value any) error {
	if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
		return errors.New("tab state target is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	dir := filepath.Dir(path)
	temp, err := os.CreateTemp(dir, ".hmux-tabs-*")
	if err != nil {
		return err
	}
	tempPath := temp.Name()
	defer os.Remove(tempPath)
	if err := temp.Chmod(0o600); err != nil {
		return errors.Join(err, temp.Close())
	}
	if _, err := temp.Write(data); err != nil {
		return errors.Join(err, temp.Close())
	}
	if err := temp.Sync(); err != nil {
		return errors.Join(err, temp.Close())
	}
	if err := temp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tempPath, path); err != nil {
		return err
	}
	directory, err := os.Open(dir)
	if err != nil {
		return err
	}
	syncErr := directory.Sync()
	closeErr := directory.Close()
	return errors.Join(syncErr, closeErr)
}

func validateSessions(sessions []string) error {
	seen := make(map[string]struct{}, len(sessions))
	for _, id := range sessions {
		if err := model.ValidateSessionID(id); err != nil {
			return err
		}
		if _, exists := seen[id]; exists {
			return errors.New("duplicate session in launcher state")
		}
		seen[id] = struct{}{}
	}
	if len(sessions) > 256 {
		return errors.New("launcher tab count exceeds limit")
	}
	return nil
}

func containsSession(sessions []string, id string) bool {
	for _, sessionID := range sessions {
		if sessionID == id {
			return true
		}
	}
	return false
}

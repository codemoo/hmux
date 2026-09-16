package recovery

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
)

const (
	stateVersion      = 1
	maximumStateBytes = 16 * 1024 * 1024
	lockWait          = 2 * time.Second
)

type Store struct {
	StateDir string
	Runner   catalog.Runner
	Bind     func(context.Context, []int) (map[int]catalog.ResumeReference, error)
	BootID   func(context.Context) (string, error)
}

type snapshot struct {
	Sessions []savedSession `json:"sessions"`
}

type savedSession struct {
	Identity model.SessionIdentity `json:"identity"`
	Name     string                `json:"name"`
	Alias    string                `json:"alias,omitempty"`
	Hidden   bool                  `json:"hidden,omitempty"`
	Profile  string                `json:"profile,omitempty"`
	Label    string                `json:"label,omitempty"`
	Tags     []string              `json:"tags,omitempty"`
	Windows  []savedWindow         `json:"windows"`
}

type savedWindow struct {
	Index  int         `json:"index"`
	Name   string      `json:"name"`
	Layout string      `json:"layout"`
	Active bool        `json:"active,omitempty"`
	Panes  []savedPane `json:"panes"`
}

type savedPane struct {
	Index  int                      `json:"index"`
	Cwd    string                   `json:"cwd"`
	Active bool                     `json:"active,omitempty"`
	Resume *catalog.ResumeReference `json:"resume,omitempty"`
}

type restoredIdentity struct {
	From  model.SessionIdentity `json:"from"`
	To    model.SessionIdentity `json:"to"`
	Name  string                `json:"name"`
	Panes map[string]string     `json:"panes,omitempty"`
	Gate  string                `json:"gate,omitempty"`
}

type pendingRestore struct {
	BootID    string                      `json:"boot_id"`
	Snapshot  snapshot                    `json:"snapshot"`
	Completed map[string]restoredIdentity `json:"completed"`
	Intents   map[string]string           `json:"intents,omitempty"`
}

type diskState struct {
	Version    int                `json:"version"`
	BootID     string             `json:"boot_id"`
	Checkpoint snapshot           `json:"checkpoint"`
	Pending    *pendingRestore    `json:"pending,omitempty"`
	Mappings   []restoredIdentity `json:"mappings,omitempty"`
	UpdatedAt  string             `json:"updated_at"`
}

func (s Store) Sync(ctx context.Context) error {
	return s.withLock(ctx, func() error {
		bootID, err := s.currentBootID(ctx)
		if err != nil {
			return err
		}
		current, err := s.readState()
		if errors.Is(err, os.ErrNotExist) {
			shot, err := s.capture(ctx)
			if err != nil {
				return err
			}
			return s.writeState(diskState{Version: stateVersion, BootID: bootID, Checkpoint: shot})
		}
		if err != nil {
			return err
		}
		if current.Pending != nil || current.BootID != bootID {
			return s.restoreLocked(ctx, bootID, &current)
		}
		return s.saveLocked(ctx, bootID, &current)
	})
}

func (s Store) Save(ctx context.Context) error {
	return s.withLock(ctx, func() error {
		bootID, err := s.currentBootID(ctx)
		if err != nil {
			return err
		}
		current, err := s.readState()
		if errors.Is(err, os.ErrNotExist) {
			current = diskState{Version: stateVersion, BootID: bootID}
		} else if err != nil {
			return err
		}
		if current.BootID != bootID || current.Pending != nil {
			return errors.New("recovery save requires boot synchronization")
		}
		return s.saveLocked(ctx, bootID, &current)
	})
}

func (s Store) Restore(ctx context.Context) error {
	return s.withLock(ctx, func() error {
		bootID, err := s.currentBootID(ctx)
		if err != nil {
			return err
		}
		current, err := s.readState()
		if err != nil {
			return err
		}
		return s.restoreLocked(ctx, bootID, &current)
	})
}

// Apply annotates only exact restored tmux lifetimes. A same-name session that
// was already present is never entered in Mappings and cannot match here.
func (s Store) Apply(value *model.Catalog) error {
	if value == nil {
		return errors.New("recovery catalog is nil")
	}
	return s.withLock(context.Background(), func() error {
		current, err := s.readState()
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		if err != nil {
			return err
		}
		byTarget := make(map[string]restoredIdentity, len(current.Mappings))
		for _, mapping := range current.Mappings {
			byTarget[identityKey(mapping.To)] = mapping
		}
		for index := range value.Sessions {
			session := &value.Sessions[index]
			mapping, ok := byTarget[identityKey(model.SessionIdentity{ID: session.ID, CreatedAt: session.CreatedAt})]
			if !ok || mapping.Name != session.Name {
				continue
			}
			from := mapping.From
			session.RestoredFrom = &from
		}
		return nil
	})
}

func (s Store) saveLocked(ctx context.Context, bootID string, current *diskState) error {
	shot, err := s.capture(ctx)
	if err != nil {
		return err
	}
	// A stable binding returned by capture is authoritative. An omitted binding
	// may be unavailable or ambiguous, so Save must never carry an old identity
	// forward and accidentally hide a provider switch or /new conversation.
	current.Version = stateVersion
	current.BootID = bootID
	current.Checkpoint = shot
	current.Pending = nil
	return s.writeState(*current)
}

func (s Store) restoreLocked(ctx context.Context, bootID string, current *diskState) error {
	if current.Pending == nil || current.Pending.BootID != bootID {
		current.Pending = &pendingRestore{
			BootID: bootID, Snapshot: cloneSnapshot(current.Checkpoint),
			Completed: map[string]restoredIdentity{},
		}
		if err := s.writeState(*current); err != nil {
			return err
		}
	}
	if current.Pending.Completed == nil {
		current.Pending.Completed = map[string]restoredIdentity{}
	}
	// An empty checkpoint still needs a usable tmux server after a reboot.
	// Same-boot sync/save intentionally leaves deliberate session deletion alone.
	if current.BootID != bootID && len(current.Pending.Snapshot.Sessions) == 0 {
		if err := s.startEmptyServer(ctx); err != nil {
			return err
		}
	}
	if err := s.restoreSnapshot(ctx, current); err != nil {
		// restoreSnapshot persists each completed session. The old checkpoint
		// and pending work intentionally remain intact on any later failure.
		return err
	}
	live, err := s.capture(ctx)
	if err != nil {
		return err
	}
	if len(current.Pending.Snapshot.Sessions) != 0 && len(live.Sessions) == 0 {
		return errors.New("recovery produced an empty tmux catalog")
	}
	for _, mapping := range current.Pending.Completed {
		if err := s.verifyRestoredPanes(ctx, mapping); err != nil {
			return err
		}
	}
	rebased := rebaseSnapshot(current.Pending.Snapshot, current.Pending.Completed)
	mergeMissingResumeReferences(&live, rebased)
	completed := completedMappings(current.Pending.Completed)
	if err := s.rebaseSharedWorkspace(ctx, current.Mappings, completed, live); err != nil {
		return err
	}
	current.Mappings = retainLiveMappings(current.Mappings, completed, live)
	current.Checkpoint = live
	current.BootID = bootID
	current.Pending = nil
	return s.writeState(*current)
}

func (s Store) currentBootID(ctx context.Context) (string, error) {
	if s.BootID != nil {
		value, err := s.BootID(ctx)
		if err != nil {
			return "", err
		}
		return validateBootID(value)
	}
	value, err := systemBootID(ctx)
	if err != nil {
		return "", err
	}
	return validateBootID(value)
}

func (s Store) withLock(ctx context.Context, action func() error) error {
	if ctx == nil {
		return errors.New("recovery store is not configured")
	}
	root, err := s.ensureRoot()
	if err != nil {
		return err
	}
	lockPath := filepath.Join(root, "state.lock")
	if info, err := os.Lstat(lockPath); err == nil && (!info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0) {
		return errors.New("recovery lock is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	lock, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := validatePrivateFile(lock, 0, true); err != nil {
		return fmt.Errorf("recovery lock: %w", err)
	}
	if err := filelock.Acquire(ctx, lock, lockWait); err != nil {
		return fmt.Errorf("recovery lock: %w", err)
	}
	defer filelock.Unlock(lock)
	return action()
}

func (s Store) runner() catalog.Runner {
	if s.Runner != nil {
		return s.Runner
	}
	return catalog.TmuxRunner{}
}

func (s Store) root() string { return filepath.Join(filepath.Clean(s.StateDir), "recovery") }

func (s Store) ensureRoot() (string, error) {
	base := filepath.Clean(s.StateDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return "", errors.New("recovery state directory must be an absolute non-root path")
	}
	if err := os.MkdirAll(base, 0o700); err != nil {
		return "", err
	}
	if err := ensurePrivateDirectory(base); err != nil {
		return "", err
	}
	root := s.root()
	if err := ensurePrivateDirectory(root); err != nil {
		return "", err
	}
	return root, nil
}

func ensurePrivateDirectory(path string) error {
	if err := os.Mkdir(path, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 || info.Mode().Perm()&0o077 != 0 {
		return errors.New("recovery state path is not a private real directory")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("recovery state path is not owned by the current user")
	}
	return nil
}

func (s Store) statePath() string { return filepath.Join(s.root(), "state.json") }

func (s Store) readState() (diskState, error) {
	var current diskState
	path := s.statePath()
	info, err := os.Lstat(path)
	if err != nil {
		return current, err
	}
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 || info.Size() < 1 || info.Size() > maximumStateBytes || info.Mode().Perm()&0o077 != 0 {
		return current, errors.New("recovery state is not a private bounded regular file")
	}
	file, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return current, err
	}
	defer file.Close()
	opened, statErr := file.Stat()
	if statErr != nil || !os.SameFile(info, opened) {
		return current, errors.New("recovery state changed while opening")
	}
	if err := validatePrivateFile(file, maximumStateBytes, false); err != nil {
		return current, err
	}
	decoder := json.NewDecoder(io.LimitReader(file, maximumStateBytes+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&current); err != nil {
		return current, err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return current, errors.New("recovery state contains trailing data")
	}
	if err := validateState(current); err != nil {
		return current, err
	}
	return current, nil
}

func (s Store) writeState(current diskState) error {
	current.Version = stateVersion
	current.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	if err := validateState(current); err != nil {
		return err
	}
	data, err := json.Marshal(current)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	if len(data) > maximumStateBytes {
		return errors.New("recovery state exceeds size limit")
	}
	path := s.statePath()
	if info, err := os.Lstat(path); err == nil && (!info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0) {
		return errors.New("recovery state target is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temp, err := os.CreateTemp(s.root(), ".hmux-recovery-*")
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
	directory, err := os.Open(s.root())
	if err != nil {
		return err
	}
	return errors.Join(directory.Sync(), directory.Close())
}

func validatePrivateFile(file *os.File, maximum int64, allowEmpty bool) error {
	info, err := file.Stat()
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 || (!allowEmpty && info.Size() < 1) || (maximum > 0 && info.Size() > maximum) {
		return errors.New("file is not private, bounded, and regular")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("file is not owned by the current user")
	}
	return nil
}

func identityKey(value model.SessionIdentity) string {
	return fmt.Sprintf("%s/%d", value.ID, value.CreatedAt)
}

func retainLiveMappings(previous, added []restoredIdentity, live snapshot) []restoredIdentity {
	sessions := map[string]string{}
	for _, s := range live.Sessions {
		sessions[identityKey(s.Identity)] = s.Name
	}
	sources, targets := map[string]bool{}, map[string]bool{}
	var result []restoredIdentity
	for _, list := range [][]restoredIdentity{added, previous} {
		for _, m := range list {
			from, to := identityKey(m.From), identityKey(m.To)
			if sessions[to] != m.Name || sources[from] || targets[to] {
				continue
			}
			result = append(result, m)
			sources[from] = true
			targets[to] = true
		}
	}
	return result
}

// Creating a detached shell starts tmux reliably; bare start-server exits again
// under tmux's default exit-empty policy. Existing sessions are never attached.
func (s Store) startEmptyServer(ctx context.Context) error {
	live, err := catalog.ReadBasic(ctx, s.runner())
	if err != nil {
		return err
	}
	if len(live.Sessions) > 0 {
		return nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	shell := "/bin/sh"
	if info, err := os.Stat("/bin/zsh"); err == nil && info.Mode().IsRegular() {
		shell = "/bin/zsh"
	}
	_, createErr := s.runner().Output(ctx, "new-session", "-d", "-s", "hmux", "-c", home, shell, "-l")
	if createErr != nil {
		// A concurrent client may have created the fallback session first.
		live, err = catalog.ReadBasic(ctx, s.runner())
		if err == nil && len(live.Sessions) > 0 {
			return nil
		}
		return fmt.Errorf("start Home tmux: %w", createErr)
	}
	return nil
}

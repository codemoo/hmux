package workflow

import (
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"golang.org/x/sys/unix"
)

const (
	stateVersion          = 1
	maxHookInput          = 256 * 1024
	maxStateSize          = 16 * 1024 * 1024
	maxWorkflows          = 1024
	maxNodesPerWorkflow   = 128
	maxVisibleWorkflows   = 32
	terminalRetention     = 7 * 24 * time.Hour
	staleAfter            = 2 * time.Hour
	defaultLockTimeout    = 750 * time.Millisecond
	lockPollInterval      = 10 * time.Millisecond
	StatusRunning         = "running"
	StatusWaitingApproval = "waiting_approval"
	StatusWaitingInput    = "waiting_input"
	StatusCompleted       = "completed"
	StatusFailed          = "failed"
	StatusInterrupted     = "interrupted"
	StatusStale           = "stale"
)

type Store struct {
	StateDir string
	Now      func() time.Time
	lockWait time.Duration
}

type Binding struct {
	SessionID string
	CreatedAt int64
}

type storedWorkflow struct {
	ID            string                        `json:"id"`
	TMUXSessionID string                        `json:"tmux_session_id"`
	TMUXCreatedAt int64                         `json:"tmux_created_at"`
	Source        string                        `json:"source"`
	SessionID     string                        `json:"session_id,omitempty"`
	TurnID        string                        `json:"turn_id,omitempty"`
	Status        string                        `json:"status"`
	Model         string                        `json:"model,omitempty"`
	StartedAt     int64                         `json:"started_at"`
	UpdatedAt     int64                         `json:"updated_at"`
	EndedAt       int64                         `json:"ended_at,omitempty"`
	Nodes         map[string]model.WorkflowNode `json:"nodes"`
}

type state struct {
	Version   int                       `json:"version"`
	Workflows map[string]storedWorkflow `json:"workflows"`
	UpdatedAt string                    `json:"updated_at"`
}

func (s Store) readPruned() (state, error) {
	current, err := s.read()
	if err != nil {
		return current, err
	}
	now := s.currentTime()
	if !needsPrune(current, now) {
		return current, nil
	}
	lock, err := openLock(filepath.Join(s.root(), "state.lock"))
	if err != nil {
		return state{}, err
	}
	defer lock.Close()
	if err := s.acquireLock(lock); err != nil {
		return state{}, err
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN) //nolint:errcheck
	current, err = s.read()
	if err != nil {
		return state{}, err
	}
	if !prune(&current, now) {
		return current, nil
	}
	current.UpdatedAt = now.UTC().Format(time.RFC3339Nano)
	if err := validateState(current); err != nil {
		return state{}, err
	}
	if err := s.write(current); err != nil {
		return state{}, err
	}
	return current, nil
}

func (s Store) update(action func(*state, time.Time) error) error {
	if err := s.ensureRoot(); err != nil {
		return err
	}
	lock, err := openLock(filepath.Join(s.root(), "state.lock"))
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := s.acquireLock(lock); err != nil {
		return err
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN) //nolint:errcheck

	current, err := s.read()
	if errors.Is(err, os.ErrNotExist) {
		current = state{Version: stateVersion, Workflows: map[string]storedWorkflow{}}
	} else if err != nil {
		return err
	}
	now := s.currentTime()
	if err := action(&current, now); err != nil {
		return err
	}
	prune(&current, now)
	current.Version = stateVersion
	current.UpdatedAt = now.UTC().Format(time.RFC3339Nano)
	if _, err := pruneToSize(&current, maxStateSize); err != nil {
		return err
	}
	if err := validateState(current); err != nil {
		return err
	}
	return s.write(current)
}

func (s Store) acquireLock(lock *os.File) error {
	wait := s.lockWait
	if wait <= 0 {
		wait = defaultLockTimeout
	}
	deadline := time.Now().Add(wait)
	for {
		err := unix.Flock(int(lock.Fd()), unix.LOCK_EX|unix.LOCK_NB)
		if err == nil {
			return nil
		}
		if !errors.Is(err, unix.EWOULDBLOCK) && !errors.Is(err, unix.EAGAIN) {
			return err
		}
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return errors.New("workflow state lock is busy")
		}
		if remaining > lockPollInterval {
			remaining = lockPollInterval
		}
		time.Sleep(remaining)
	}
}

func (s Store) read() (state, error) {
	var current state
	path := filepath.Join(s.root(), "state.json")
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return current, err
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return current, errors.New("open workflow state file")
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return current, err
	}
	if err := validatePrivateFile(info, maxStateSize); err != nil {
		return current, err
	}
	decoder := json.NewDecoder(io.LimitReader(file, maxStateSize+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&current); err != nil {
		return current, err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return current, errors.New("workflow state contains trailing data")
	}
	if err := validateState(current); err != nil {
		return current, err
	}
	return current, nil
}

func (s Store) write(current state) error {
	data, err := json.Marshal(current)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	if len(data) > maxStateSize {
		return errors.New("workflow state exceeds size limit")
	}
	path := filepath.Join(s.root(), "state.json")
	if info, err := os.Lstat(path); err == nil {
		if err := validatePrivateFile(info, maxStateSize); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temp, err := os.CreateTemp(s.root(), ".hmux-workflows-*")
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
	defer directory.Close()
	return directory.Sync()
}

func (s Store) ensureRoot() error {
	root := filepath.Clean(s.StateDir)
	if !filepath.IsAbs(root) || root == string(os.PathSeparator) {
		return errors.New("workflow state directory must be an absolute non-root path")
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	if err := ensureOwnedDirectory(root); err != nil {
		return err
	}
	workflowRoot := s.root()
	if err := os.Mkdir(workflowRoot, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return err
	}
	return ensureOwnedDirectory(workflowRoot)
}

func (s Store) root() string {
	return filepath.Join(filepath.Clean(s.StateDir), "workflows")
}

func (s Store) currentTime() time.Time {
	if s.Now != nil {
		return s.Now().UTC()
	}
	return time.Now().UTC()
}

func ensureOwnedDirectory(path string) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return errors.New("workflow state path is not a directory")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Geteuid() {
		return errors.New("workflow state directory is not owned by the current user")
	}
	return os.Chmod(path, 0o700)
}

func openLock(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_CREAT|unix.O_RDWR|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, err
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return nil, errors.New("create workflow lock file")
	}
	var stat unix.Stat_t
	if err := unix.Fstat(fd, &stat); err != nil {
		_ = file.Close()
		return nil, err
	}
	if stat.Mode&unix.S_IFMT != unix.S_IFREG || int(stat.Uid) != os.Geteuid() {
		_ = file.Close()
		return nil, errors.New("workflow lock is not a current-user regular file")
	}
	if err := unix.Fchmod(fd, 0o600); err != nil {
		_ = file.Close()
		return nil, err
	}
	return file, nil
}

func validatePrivateFile(info os.FileInfo, maximum int64) error {
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 || info.Size() < 1 || info.Size() > maximum {
		return errors.New("workflow state is not a small regular file")
	}
	if info.Mode().Perm()&0o077 != 0 {
		return errors.New("workflow state permissions are not private")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Geteuid() {
		return errors.New("workflow state is not owned by the current user")
	}
	return nil
}

package sessionstate

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

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
)

const visibilityVersion = 1

type visibilityEntry struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	CreatedAt int64  `json:"created_at"`
}

type visibilityState struct {
	Version   int                        `json:"version"`
	Hidden    map[string]visibilityEntry `json:"hidden"`
	UpdatedAt string                     `json:"updated_at"`
}

// ApplyVisibility overlays reversible presentation state without changing
// tmux or the rollback-sensitive sessions.json metadata schema.
func (s Store) ApplyVisibility(value *model.Catalog) error {
	if value == nil {
		return errors.New("catalog is nil")
	}
	current, err := s.readVisibility()
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	for index := range value.Sessions {
		session := &value.Sessions[index]
		entry, exists := current.Hidden[session.ID]
		session.Hidden = exists && entry.Name == session.Name && entry.CreatedAt == session.CreatedAt
	}
	return nil
}

func (s Store) SetHidden(session model.Session, hidden bool) error {
	if err := validateVisibilityEntry(visibilityEntry{
		ID: session.ID, Name: session.Name, CreatedAt: session.CreatedAt,
	}); err != nil {
		return err
	}
	return s.updateVisibility(context.Background(), func(current *visibilityState) error {
		setHiddenEntry(current, session, hidden)
		return nil
	})
}

// SetHiddenExpected resolves and checks the live tmux lifetime while holding
// the visibility lock. A stale restore therefore cannot delete a replacement
// lifetime's hidden entry.
func (s Store) SetHiddenExpected(
	ctx context.Context,
	id string,
	createdAt int64,
	hidden bool,
	resolve SessionResolver,
) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	if resolve == nil {
		return errors.New("session resolver is required")
	}
	return s.updateVisibility(ctx, func(current *visibilityState) error {
		resolveCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		defer cancel()
		session, err := resolve(resolveCtx, id)
		if err != nil {
			return err
		}
		if session.ID != id || session.CreatedAt != createdAt {
			return ErrSessionChanged
		}
		setHiddenEntry(current, session, hidden)
		return nil
	})
}

func setHiddenEntry(current *visibilityState, session model.Session, hidden bool) {
	if hidden {
		current.Hidden[session.ID] = visibilityEntry{
			ID: session.ID, Name: session.Name, CreatedAt: session.CreatedAt,
		}
	} else {
		delete(current.Hidden, session.ID)
	}
}

func (s Store) updateVisibility(ctx context.Context, action func(*visibilityState) error) error {
	if err := s.ensureRoot(); err != nil {
		return err
	}
	lockPath := filepath.Join(s.root(), "visibility.lock")
	lock, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 2*time.Second); err != nil {
		return fmt.Errorf("session visibility lock: %w", err)
	}
	defer filelock.Unlock(lock)

	current, err := s.readVisibility()
	if errors.Is(err, os.ErrNotExist) {
		current = visibilityState{Version: visibilityVersion, Hidden: map[string]visibilityEntry{}}
	} else if err != nil {
		return err
	}
	if err := action(&current); err != nil {
		return err
	}
	if len(current.Hidden) > 10000 {
		return errors.New("hidden session count exceeds limit")
	}
	for id, entry := range current.Hidden {
		if id != entry.ID {
			return errors.New("hidden session key mismatch")
		}
		if err := validateVisibilityEntry(entry); err != nil {
			return err
		}
	}
	current.Version = visibilityVersion
	current.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	data, err := json.Marshal(current)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	path := s.visibilityPath()
	if info, err := os.Lstat(path); err == nil && (!info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0) {
		return errors.New("session visibility target is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return config.AtomicWrite(path, data, 0o600)
}

func (s Store) readVisibility() (visibilityState, error) {
	var current visibilityState
	path := s.visibilityPath()
	info, err := os.Lstat(path)
	if err != nil {
		return current, err
	}
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 ||
		info.Mode().Perm()&0o077 != 0 || info.Size() < 1 || info.Size() > 2*1024*1024 {
		return current, errors.New("session visibility is not a private bounded regular file")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return current, errors.New("session visibility is not owned by the current user")
	}
	file, err := os.Open(path)
	if err != nil {
		return current, err
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, 2*1024*1024+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&current); err != nil {
		return current, err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return current, errors.New("session visibility contains trailing data")
	}
	if current.Version != visibilityVersion || current.Hidden == nil {
		return current, errors.New("invalid session visibility state")
	}
	for id, entry := range current.Hidden {
		if id != entry.ID {
			return current, errors.New("hidden session key mismatch")
		}
		if err := validateVisibilityEntry(entry); err != nil {
			return current, err
		}
	}
	return current, nil
}

func (s Store) visibilityPath() string {
	return filepath.Join(s.root(), "session-visibility.json")
}

func validateVisibilityEntry(entry visibilityEntry) error {
	if err := model.ValidateSessionID(entry.ID); err != nil {
		return err
	}
	if entry.CreatedAt < 1 || model.SafeText(entry.Name, 512) != entry.Name {
		return errors.New("invalid hidden session identity")
	}
	return nil
}

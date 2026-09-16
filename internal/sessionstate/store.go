package sessionstate

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
)

const stateVersion = 1

var ErrSessionChanged = errors.New("session identity changed")

// SessionResolver must resolve the current tmux session without reading or
// mutating sessionstate. It is called while exactly one metadata lock is held.
type SessionResolver func(context.Context, string) (model.Session, error)

type Store struct {
	StateDir string
}

type entry struct {
	ID        string   `json:"id"`
	Name      string   `json:"name"`
	CreatedAt int64    `json:"created_at"`
	Alias     string   `json:"alias,omitempty"`
	Profile   string   `json:"profile,omitempty"`
	Label     string   `json:"label,omitempty"`
	Tags      []string `json:"tags,omitempty"`
}

type state struct {
	Version   int              `json:"version"`
	Sessions  map[string]entry `json:"sessions"`
	UpdatedAt string           `json:"updated_at"`
}

func (s Store) Apply(value *model.Catalog) error {
	if value == nil {
		return errors.New("catalog is nil")
	}
	current, err := s.read()
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	for index := range value.Sessions {
		session := &value.Sessions[index]
		metadata, exists := current.Sessions[session.ID]
		if !exists || metadata.Name != session.Name || metadata.CreatedAt != session.CreatedAt {
			continue
		}
		session.Alias = metadata.Alias
		session.Profile = metadata.Profile
		session.Label = metadata.Label
		session.Tags = append([]string(nil), metadata.Tags...)
	}
	return nil
}

func (s Store) SetAlias(session model.Session, alias string) error {
	alias = strings.TrimSpace(alias)
	if err := validateAlias(alias); err != nil {
		return err
	}
	return s.update(context.Background(), func(current *state) error {
		metadata := current.Sessions[session.ID]
		if metadata.ID != "" &&
			(metadata.Name != session.Name || metadata.CreatedAt != session.CreatedAt) {
			metadata = entry{}
		}
		metadata.ID = session.ID
		metadata.Name = session.Name
		metadata.CreatedAt = session.CreatedAt
		metadata.Alias = alias
		current.Sessions[session.ID] = metadata
		return nil
	})
}

// SetAliasCurrent resolves the live tmux lifetime after taking the metadata
// lock, so legacy callers cannot write metadata for an identity that was
// recycled between lookup and persistence.
func (s Store) SetAliasCurrent(ctx context.Context, id, alias string, resolve SessionResolver) error {
	return s.setAliasResolved(ctx, id, 0, alias, resolve)
}

// SetAliasExpected resolves and checks the live tmux lifetime while holding
// the metadata lock. The persisted name always comes from that locked lookup.
func (s Store) SetAliasExpected(
	ctx context.Context,
	id string,
	createdAt int64,
	alias string,
	resolve SessionResolver,
) error {
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	return s.setAliasResolved(ctx, id, createdAt, alias, resolve)
}

func (s Store) setAliasResolved(
	ctx context.Context,
	id string,
	createdAt int64,
	alias string,
	resolve SessionResolver,
) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	alias = strings.TrimSpace(alias)
	if err := validateAlias(alias); err != nil {
		return err
	}
	if resolve == nil {
		return errors.New("session resolver is required")
	}
	return s.update(ctx, func(current *state) error {
		resolveCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		defer cancel()
		session, err := resolve(resolveCtx, id)
		if err != nil {
			return err
		}
		if session.ID != id || (createdAt > 0 && session.CreatedAt != createdAt) {
			return ErrSessionChanged
		}
		metadata := current.Sessions[id]
		if metadata.ID != "" &&
			(metadata.Name != session.Name || metadata.CreatedAt != session.CreatedAt) {
			metadata = entry{}
		}
		metadata.ID = session.ID
		metadata.Name = session.Name
		metadata.CreatedAt = session.CreatedAt
		metadata.Alias = alias
		current.Sessions[id] = metadata
		return nil
	})
}

func (s Store) SetProfile(session model.Session, profile model.Profile) error {
	if err := model.ValidateSessionID(session.ID); err != nil {
		return err
	}
	if err := model.ValidateStableID(profile.ID); err != nil {
		return err
	}
	if model.SafeText(profile.Label, 256) != profile.Label {
		return errors.New("profile label contains unsafe text")
	}
	tags := make([]string, 0, len(profile.Tags))
	for _, tag := range profile.Tags {
		if model.SafeText(tag, 128) != tag {
			return errors.New("profile tag contains unsafe text")
		}
		tags = append(tags, tag)
	}
	return s.update(context.Background(), func(current *state) error {
		metadata := current.Sessions[session.ID]
		if metadata.ID != "" &&
			(metadata.Name != session.Name || metadata.CreatedAt != session.CreatedAt) {
			metadata = entry{}
		}
		metadata.ID = session.ID
		metadata.Name = session.Name
		metadata.CreatedAt = session.CreatedAt
		metadata.Profile = profile.ID
		metadata.Label = profile.Label
		metadata.Tags = tags
		current.Sessions[session.ID] = metadata
		return nil
	})
}

func (s Store) Import(sessions []model.Session) error {
	return s.update(context.Background(), func(current *state) error {
		for _, session := range sessions {
			if err := model.ValidateSessionID(session.ID); err != nil {
				return err
			}
			metadata := current.Sessions[session.ID]
			if metadata.ID != "" &&
				(metadata.Name != session.Name || metadata.CreatedAt != session.CreatedAt) {
				metadata = entry{}
			}
			metadata.ID = session.ID
			metadata.Name = session.Name
			metadata.CreatedAt = session.CreatedAt
			if session.Alias != "" {
				if err := validateAlias(session.Alias); err != nil {
					return err
				}
				metadata.Alias = session.Alias
			}
			if session.Profile != "" {
				if err := model.ValidateStableID(session.Profile); err != nil {
					return err
				}
				metadata.Profile = session.Profile
			}
			if session.Label != "" {
				if model.SafeText(session.Label, 256) != session.Label {
					return errors.New("legacy label contains unsafe text")
				}
				metadata.Label = session.Label
			}
			if len(session.Tags) > 0 {
				metadata.Tags = append([]string(nil), session.Tags...)
			}
			current.Sessions[session.ID] = metadata
		}
		return nil
	})
}

func (s Store) update(ctx context.Context, action func(*state) error) error {
	if err := s.ensureRoot(); err != nil {
		return err
	}
	lockPath := filepath.Join(s.root(), "sessions.lock")
	lock, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 2*time.Second); err != nil {
		return fmt.Errorf("session metadata lock: %w", err)
	}
	defer filelock.Unlock(lock)

	current, err := s.read()
	if errors.Is(err, os.ErrNotExist) {
		current = state{Version: stateVersion, Sessions: map[string]entry{}}
	} else if err != nil {
		return err
	}
	if err := action(&current); err != nil {
		return err
	}
	if len(current.Sessions) > 10000 {
		return errors.New("session metadata count exceeds limit")
	}
	for id, metadata := range current.Sessions {
		if id != metadata.ID {
			return errors.New("session metadata key mismatch")
		}
		if err := validateEntry(metadata); err != nil {
			return err
		}
	}
	current.Version = stateVersion
	current.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	return s.write(current)
}

func (s Store) read() (state, error) {
	var current state
	path := filepath.Join(s.root(), "sessions.json")
	info, err := os.Lstat(path)
	if err != nil {
		return current, err
	}
	if !info.Mode().IsRegular() || info.Size() > 8*1024*1024 {
		return current, errors.New("session metadata is not a small regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return current, err
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, 8*1024*1024+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&current); err != nil {
		return current, err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return current, errors.New("session metadata contains trailing data")
	}
	if current.Version != stateVersion || current.Sessions == nil {
		return current, errors.New("invalid session metadata state")
	}
	for id, metadata := range current.Sessions {
		if id != metadata.ID {
			return current, errors.New("session metadata key mismatch")
		}
		if err := validateEntry(metadata); err != nil {
			return current, err
		}
	}
	return current, nil
}

func (s Store) write(current state) error {
	data, err := json.Marshal(current)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	path := filepath.Join(s.root(), "sessions.json")
	if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
		return errors.New("session metadata target is not a regular file")
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temp, err := os.CreateTemp(s.root(), ".hmux-sessions-*")
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
	return syncStateDirectory(s.root())
}

func syncStateDirectory(path string) error {
	directory, err := os.Open(path)
	if err != nil {
		return err
	}
	syncErr := directory.Sync()
	closeErr := directory.Close()
	return errors.Join(syncErr, closeErr)
}

func (s Store) ensureRoot() error {
	root := filepath.Clean(s.StateDir)
	if !filepath.IsAbs(root) || root == string(os.PathSeparator) {
		return errors.New("session state directory must be an absolute non-root path")
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	if info, err := os.Lstat(root); err != nil {
		return err
	} else if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return errors.New("session state path is not a directory")
	}
	if err := os.Chmod(root, 0o700); err != nil {
		return err
	}
	sessionRoot := s.root()
	if err := os.Mkdir(sessionRoot, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return err
	}
	info, err := os.Lstat(sessionRoot)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return errors.New("session metadata path is not a directory")
	}
	return os.Chmod(sessionRoot, 0o700)
}

func (s Store) root() string {
	return filepath.Join(filepath.Clean(s.StateDir), "sessions")
}

func validateEntry(metadata entry) error {
	if err := model.ValidateSessionID(metadata.ID); err != nil {
		return err
	}
	if metadata.CreatedAt <= 0 || model.SafeText(metadata.Name, 512) != metadata.Name {
		return errors.New("invalid session metadata identity")
	}
	if err := validateAlias(metadata.Alias); err != nil {
		return err
	}
	if metadata.Profile != "" {
		if err := model.ValidateStableID(metadata.Profile); err != nil {
			return err
		}
	}
	if model.SafeText(metadata.Label, 256) != metadata.Label {
		return errors.New("invalid session metadata label")
	}
	if len(metadata.Tags) > 64 {
		return errors.New("session metadata tag count exceeds limit")
	}
	for _, tag := range metadata.Tags {
		if model.SafeText(tag, 128) != tag {
			return fmt.Errorf("invalid session metadata tag")
		}
	}
	return nil
}

func validateAlias(alias string) error {
	if len(alias) > 128 || model.SafeText(alias, 128) != alias {
		return errors.New("alias must be at most 128 characters without control characters")
	}
	return nil
}

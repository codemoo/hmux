// Package sharedworkspace owns the one Home workspace used by all HMux clients.
package sharedworkspace

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
)

const MaxTabs = 32
const MaxBytes = 32 << 10

type Snapshot struct {
	Conflict    string                  `json:"conflict,omitempty"`
	Applied     []string                `json:"applied,omitempty"`
	Version     int                     `json:"version"`
	Initialized bool                    `json:"initialized"`
	Revision    uint64                  `json:"revision"`
	Tabs        []model.SessionIdentity `json:"tabs"`
	Selected    *model.SessionIdentity  `json:"selected,omitempty"`
}

// A change carries a client's previously observed local layout and its new
// layout. Applying the delta under a Home file lock preserves concurrent opens
// from other devices, unlike last-writer-wins replacement of the entire array.
type Change struct {
	OperationID string                  `json:"operation_id"`
	Revision    uint64                  `json:"revision"`
	Base        []model.SessionIdentity `json:"base"`
	Tabs        []model.SessionIdentity `json:"tabs"`
	Selected    *model.SessionIdentity  `json:"selected,omitempty"`
}
type Store struct{ StateDir string }
type Catalog func(context.Context) (model.Catalog, error)

func (s Store) Sync(ctx context.Context, change *Change, fetch Catalog) (Snapshot, error) {
	// Catalog/recovery can acquire its own lock. Never hold the workspace lock
	// while entering recovery; recovery also rebases this workspace after boot.
	catalog, err := fetch(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	root := filepath.Join(s.StateDir, "shared-workspace")
	if err := os.MkdirAll(root, 0700); err != nil {
		return Snapshot{}, err
	}
	if err := privatePath(root, true); err != nil {
		return Snapshot{}, err
	}
	lock, err := os.OpenFile(filepath.Join(root, "lock"), os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0600)
	if err != nil {
		return Snapshot{}, err
	}
	defer lock.Close()
	if err = privatePath(lock.Name(), false); err != nil {
		return Snapshot{}, err
	}
	if err = filelock.Acquire(ctx, lock, 3*time.Second); err != nil {
		return Snapshot{}, err
	}
	defer filelock.Unlock(lock)
	path := filepath.Join(root, "workspace.json")
	current, err := read(path)
	if err != nil {
		return Snapshot{}, err
	}
	before := clone(current)
	current.Tabs = rebase(current.Tabs, catalog.Sessions)
	// Focus is owned by each device. Accept the legacy field while migrating
	// old state, but never publish another device's selection.
	current.Selected = nil
	current.Conflict = ""
	conflict := false
	if change != nil {
		replay := false
		for _, id := range current.Applied {
			if id == change.OperationID {
				replay = true
			}
		}
		if !replay {
			if ValidateChange(*change) != nil || change.Revision > current.Revision || current.Revision-change.Revision > 64 {
				conflict = true
			} else {
				c := *change
				c.Base = rebase(c.Base, catalog.Sessions)
				c.Tabs = rebase(c.Tabs, catalog.Sessions)
				c.Selected = nil
				for _, id := range c.Tabs {
					if !contains(c.Base, id) && !live(id, catalog.Sessions) {
						conflict = true
					}
				}
				if !conflict {
					merged, err := Merge(current, c)
					if err != nil {
						conflict = true
					} else {
						current = merged
					}
				}
			}
			if !conflict {
				current.Applied = append(append([]string{}, current.Applied...), change.OperationID)
				if len(current.Applied) > 64 {
					current.Applied = current.Applied[len(current.Applied)-64:]
				}
			}
		}
	}
	if !reflect.DeepEqual(before, current) {
		current.Revision = before.Revision + 1
		raw, err := json.Marshal(current)
		if err != nil {
			return Snapshot{}, err
		}
		if err = config.AtomicWrite(path, raw, 0600); err != nil {
			return Snapshot{}, err
		}
	}
	// A semantic rejection carries the authoritative layout; unlike uncertain
	// transport failure it is never retried with the same delta forever.
	if conflict {
		current.Conflict = "workspace_conflict"
	}
	return current, nil
}
func Merge(current Snapshot, c Change) (Snapshot, error) {
	if err := ValidateChange(c); err != nil {
		return Snapshot{}, err
	}
	result := clone(current)
	result.Version = 1
	result.Initialized = true
	// Only explicit removals disappear. A stale client cannot erase remote opens.
	result.Tabs = nil
	for _, id := range current.Tabs {
		if !contains(c.Base, id) || contains(c.Tabs, id) {
			result.Tabs = append(result.Tabs, id)
		}
	}
	// Place newly opened tabs next to the nearest surviving requested anchor.
	// This preserves open+move before the first sync without moving unseen tabs.
	for index, id := range c.Tabs {
		if contains(c.Base, id) || contains(result.Tabs, id) {
			continue
		}
		kept := []model.SessionIdentity{}
		for _, old := range result.Tabs {
			if old.ID != id.ID {
				kept = append(kept, old)
			}
		}
		result.Tabs = kept
		position := len(result.Tabs)
		anchored := false
		for _, next := range c.Tabs[index+1:] {
			for at, present := range result.Tabs {
				if present == next {
					position = at
					anchored = true
					break
				}
			}
			if anchored {
				break
			}
		}
		if !anchored {
			for i := index - 1; i >= 0; i-- {
				for at, present := range result.Tabs {
					if present == c.Tabs[i] {
						position = at + 1
						anchored = true
						break
					}
				}
				if anchored {
					break
				}
			}
		}
		result.Tabs = append(result.Tabs, model.SessionIdentity{})
		copy(result.Tabs[position+1:], result.Tabs[position:])
		result.Tabs[position] = id
	}
	// Reorder only when the client actually changed its existing relative order.
	oldOrder, newOrder := []model.SessionIdentity{}, []model.SessionIdentity{}
	for _, id := range c.Base {
		if contains(c.Tabs, id) {
			oldOrder = append(oldOrder, id)
		}
	}
	for _, id := range c.Tabs {
		if contains(c.Base, id) {
			newOrder = append(newOrder, id)
		}
	}
	if !reflect.DeepEqual(oldOrder, newOrder) {
		ordered := []model.SessionIdentity{}
		for _, id := range c.Tabs {
			if contains(result.Tabs, id) {
				ordered = append(ordered, id)
			}
		}
		for _, id := range result.Tabs {
			if !contains(ordered, id) {
				ordered = append(ordered, id)
			}
		}
		result.Tabs = ordered
	}
	if len(result.Tabs) > MaxTabs {
		return Snapshot{}, errors.New("shared workspace has 32 tabs")
	}
	result.Selected = nil
	if result.Tabs == nil {
		result.Tabs = []model.SessionIdentity{}
	}
	return result, nil
}
func ValidateChange(c Change) error {
	if len(c.OperationID) < 16 || len(c.OperationID) > 80 {
		return errors.New("workspace operation ID required")
	}
	for _, r := range c.OperationID {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-') {
			return errors.New("invalid operation ID")
		}
	}
	if validateIDs(c.Base) != nil || validateIDs(c.Tabs) != nil {
		return errors.New("invalid workspace tabs")
	}
	if c.Selected != nil && !contains(c.Tabs, *c.Selected) {
		return errors.New("selection is not an open tab")
	}
	return nil
}
func ValidateSnapshot(s Snapshot) error {
	if s.Conflict != "" && s.Conflict != "workspace_conflict" {
		return errors.New("invalid workspace conflict")
	}
	if len(s.Applied) > 64 {
		return errors.New("invalid operation history")
	}
	if s.Version != 1 || validateIDs(s.Tabs) != nil || s.Selected != nil && !contains(s.Tabs, *s.Selected) {
		return errors.New("invalid shared workspace")
	}
	return nil
}
func validateIDs(ids []model.SessionIdentity) error {
	if len(ids) > MaxTabs {
		return errors.New("too many tabs")
	}
	seen := map[string]bool{}
	for _, id := range ids {
		if model.ValidateSessionID(id.ID) != nil || id.CreatedAt < 1 || seen[id.ID] {
			return errors.New("invalid or duplicate identity")
		}
		seen[id.ID] = true
	}
	return nil
}
func contains(ids []model.SessionIdentity, want model.SessionIdentity) bool {
	for _, id := range ids {
		if id == want {
			return true
		}
	}
	return false
}
func live(id model.SessionIdentity, sessions []model.Session) bool {
	for _, s := range sessions {
		if id.ID == s.ID && id.CreatedAt == s.CreatedAt {
			return true
		}
	}
	return false
}
func resolve(id model.SessionIdentity, sessions []model.Session) model.SessionIdentity {
	if live(id, sessions) {
		return id
	}
	var candidates []model.SessionIdentity
	for _, s := range sessions {
		if s.RestoredFrom != nil && *s.RestoredFrom == id {
			candidates = append(candidates, model.SessionIdentity{ID: s.ID, CreatedAt: s.CreatedAt})
		}
	}
	if len(candidates) == 1 {
		return candidates[0]
	}
	return id
}
func rebase(ids []model.SessionIdentity, sessions []model.Session) []model.SessionIdentity {
	result := []model.SessionIdentity{}
	for _, id := range ids {
		v := resolve(id, sessions)
		if !contains(result, v) {
			result = append(result, v)
		}
	}
	return result
}
func clone(s Snapshot) Snapshot {
	s.Tabs = append([]model.SessionIdentity{}, s.Tabs...)
	s.Applied = append([]string(nil), s.Applied...)
	if s.Selected != nil {
		v := *s.Selected
		s.Selected = &v
	}
	return s
}
func privatePath(path string, directory bool) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Getuid() || info.Mode().Perm()&0077 != 0 || info.Mode()&os.ModeSymlink != 0 || info.IsDir() != directory || !directory && !info.Mode().IsRegular() {
		return errors.New("workspace state must be private and owner-controlled")
	}
	return nil
}
func read(path string) (Snapshot, error) {
	empty := Snapshot{Version: 1, Tabs: []model.SessionIdentity{}}
	if _, err := os.Lstat(path); errors.Is(err, os.ErrNotExist) {
		return empty, nil
	}
	if err := privatePath(path, false); err != nil {
		return empty, err
	}
	f, err := os.Open(path)
	if err != nil {
		return empty, err
	}
	defer f.Close()
	raw, err := io.ReadAll(io.LimitReader(f, MaxBytes+1))
	if err != nil || len(raw) > MaxBytes {
		return empty, errors.New("invalid workspace size")
	}
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	var value Snapshot
	if err = d.Decode(&value); err != nil {
		return empty, err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return empty, errors.New("trailing workspace data")
	}
	return value, ValidateSnapshot(value)
}

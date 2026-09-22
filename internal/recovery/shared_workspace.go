package recovery

import (
	"context"
	"errors"
	"os"
	"path/filepath"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

// Rebase shared tabs at each recovery checkpoint, including when no UI is open.
// Otherwise a tab saved as A would miss A->B->C if only C's direct parent B is
// still advertised when the next browser client connects.
func (s Store) rebaseSharedWorkspace(ctx context.Context, previous, added []restoredIdentity, live snapshot) error {
	path := filepath.Join(s.StateDir, "shared-workspace", "workspace.json")
	if _, err := os.Lstat(path); errors.Is(err, os.ErrNotExist) {
		return nil
	} else if err != nil {
		return err
	}
	value := workspaceRecoveryCatalog(previous, added, live)
	_, err := (sharedworkspace.Store{StateDir: s.StateDir}).Sync(ctx, nil, func(context.Context) (model.Catalog, error) { return value, nil })
	return err
}

func workspaceRecoveryCatalog(previous, added []restoredIdentity, live snapshot) model.Catalog {
	value := model.Catalog{}
	// Add current exact identities first. Same-ID recycled lifetimes are never
	// treated as recovery merely because a name or numeric tmux ID happens to match.
	for _, s := range live.Sessions {
		value.Sessions = append(value.Sessions, model.Session{ID: s.Identity.ID, CreatedAt: s.Identity.CreatedAt})
	}
	links := append(append([]restoredIdentity{}, added...), previous...)
	for _, mapping := range links {
		target := mapping.To
		name := mapping.Name
		// Compose only verified persisted links; bound traversal and reject cycles.
		seen := map[model.SessionIdentity]bool{mapping.From: true}
		for step := 0; step <= len(links); step++ {
			if seen[target] {
				break
			}
			seen[target] = true
			found := false
			for _, session := range live.Sessions {
				if session.Identity == target && session.Name == name {
					from := mapping.From
					value.Sessions = append(value.Sessions, model.Session{ID: target.ID, CreatedAt: target.CreatedAt, RestoredFrom: &from})
					found = true
					break
				}
			}
			if found {
				break
			}
			next := -1
			for i, link := range links {
				if link.From == target {
					if next != -1 {
						next = -2
						break
					}
					next = i
				}
			}
			if next < 0 {
				break
			}
			target = links[next].To
			name = links[next].Name
		}
	}
	return value
}

// Package home provides local host services for the web connector.
package home

import (
	"context"
	"errors"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

func Catalog(ctx context.Context, cfg config.HomeConfig) (model.Catalog, error) {
	return agent.CatalogAt(ctx, cfg.StateDir)
}
func CreateSession(ctx context.Context, cfg config.HomeConfig, inventory model.Inventory, profileID, name string) (agent.CreateResult, error) {
	return agent.CreateSession(ctx, inventory, profileID, name, cfg.StateDir)
}
func SetAliasExpected(ctx context.Context, cfg config.HomeConfig, id string, createdAt int64, alias string) error {
	return agent.SetAliasExpected(ctx, cfg.StateDir, id, createdAt, alias)
}
func SetHiddenExpected(ctx context.Context, cfg config.HomeConfig, id string, createdAt int64, hidden bool) error {
	return agent.SetHiddenExpected(ctx, cfg.StateDir, id, createdAt, hidden)
}

// Conversation is an explicit read, never part of catalog polling.
func Conversation(ctx context.Context, cfg config.HomeConfig, id string, createdAt int64) (model.Conversation, error) {
	if model.ValidateSessionID(id) != nil || createdAt < 1 {
		return model.Conversation{}, errors.New("invalid session identity")
	}
	return catalog.ReadConversation(ctx, id, createdAt)
}
func SharedWorkspace(ctx context.Context, cfg config.HomeConfig, change *sharedworkspace.Change) (sharedworkspace.Snapshot, error) {
	if change != nil {
		if err := sharedworkspace.ValidateChange(*change); err != nil {
			return sharedworkspace.Snapshot{}, err
		}
	}
	return (sharedworkspace.Store{StateDir: cfg.StateDir}).Sync(ctx, change, func(ctx context.Context) (model.Catalog, error) { return agent.BasicCatalogAt(ctx, cfg.StateDir) })
}

// VerifyFileStageSession checks the exact tmux lifetime before and after upload.
func VerifyFileStageSession(ctx context.Context, identity filestage.SessionIdentity) error {
	if model.ValidateSessionID(identity.ID) != nil || identity.CreatedAt < 1 {
		return errors.New("invalid session identity")
	}
	return requireTmuxCreatedAt(ctx, catalog.TmuxRunner{}, identity.ID, identity.CreatedAt)
}

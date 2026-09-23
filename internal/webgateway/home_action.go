package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"strings"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/home"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

func changesCatalog(operation string) bool {
	switch operation {
	case "create", "alias", "hidden":
		return true
	}
	return false
}

func homeAction(ctx context.Context, cfg config.HomeConfig, m Message) (any, error) {
	switch m.Operation {
	case "workspace":
		var q struct {
			Change *sharedworkspace.Change `json:"change"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid workspace request")
		}
		return home.SharedWorkspace(ctx, cfg, q.Change)
	case "profiles":
		inventory, err := config.LoadInventory(cfg.InventoryPath)
		if err != nil {
			return nil, err
		}
		profiles := []map[string]string{}
		for _, p := range inventory.Profiles {
			profiles = append(profiles, map[string]string{"id": p.ID, "label": p.Label})
		}
		return profiles, nil
	case "providers", "provider-key", "provider-job-start", "provider-job", "provider-job-input", "provider-job-cancel":
		return providerAction(ctx, cfg, m.Operation, m.Payload)
	case "create":
		var q struct {
			Profile string `json:"profile"`
			Name    string `json:"name"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid create")
		}
		inv, err := config.LoadInventory(cfg.InventoryPath)
		if err != nil {
			return nil, err
		}
		return home.CreateSession(ctx, cfg, inv, q.Profile, q.Name)
	}
	if model.ValidateSessionID(m.Session.ID) != nil || m.Session.CreatedAt < 1 {
		return nil, errors.New("invalid session")
	}
	switch m.Operation {
	case "conversation":
		return home.Conversation(ctx, cfg, m.Session.ID, m.Session.CreatedAt)
	case "alias":
		var q struct {
			Alias string `json:"alias"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid alias")
		}
		return map[string]bool{"ok": true}, home.SetAliasExpected(ctx, cfg, m.Session.ID, m.Session.CreatedAt, q.Alias)
	case "hidden":
		var q struct {
			Hidden *bool `json:"hidden"`
		}
		if strictPayload(m.Payload, &q) != nil || q.Hidden == nil {
			return nil, errors.New("invalid hidden state")
		}
		return map[string]bool{"ok": true}, home.SetHiddenExpected(ctx, cfg, m.Session.ID, m.Session.CreatedAt, *q.Hidden)
	}
	return nil, errors.New("operation not permitted")
}
func strictPayload(raw json.RawMessage, v any) error {
	if len(raw) > 16<<10 {
		return errors.New("payload too large")
	}
	d := json.NewDecoder(strings.NewReader(string(raw)))
	d.DisallowUnknownFields()
	if err := d.Decode(v); err != nil {
		return err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return errors.New("trailing payload")
	}
	return nil
}

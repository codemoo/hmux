package client

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os/exec"
	"strconv"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

// Conversation is an explicit read, never part of metadata/catalog polling.
func Conversation(ctx context.Context, cfg config.ClientConfig, id string, createdAt int64) (model.Conversation, error) {
	if model.ValidateSessionID(id) != nil || createdAt < 1 {
		return model.Conversation{}, errors.New("invalid session identity")
	}
	if cfg.Role == "home" {
		return catalog.ReadConversation(ctx, id, createdAt)
	}
	capabilities, err := remoteAgentCapabilities(ctx, cfg)
	if err != nil {
		return model.Conversation{}, errors.New("Home conversation capability unavailable")
	}
	if !hasCapability(capabilities, "conversation-v1") {
		return model.Conversation{}, errors.New("Update the Home agent to use conversation view")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "conversation", "--session", remoteSessionArg(id), "--created-at", strconv.FormatInt(createdAt, 10))
	command := exec.CommandContext(ctx, "ssh", args...)
	command.WaitDelay = 2_000_000_000
	command.Stderr = io.Discard
	raw, err := safeexec.Output(command, 2*1024*1024)
	if err != nil {
		return model.Conversation{}, errors.New("Home conversation is unavailable")
	}
	if model.ValidateCatalogJSONStructure(raw) != nil {
		return model.Conversation{}, errors.New("invalid conversation structure")
	}
	var value model.Conversation
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if decoder.Decode(&value) != nil || decoder.Decode(&struct{}{}) != io.EOF {
		return model.Conversation{}, errors.New("invalid conversation response")
	}
	if value.SessionID != id || value.CreatedAt != createdAt || len(value.Messages) > 200 {
		return model.Conversation{}, errors.New("conversation identity or size mismatch")
	}
	switch value.Status {
	case "ready", "unavailable", "ambiguous":
	default:
		return model.Conversation{}, errors.New("invalid conversation status")
	}
	if value.Status != "ready" && len(value.Messages) != 0 {
		return model.Conversation{}, errors.New("unexpected conversation messages")
	}
	total := 0
	seen := make(map[string]bool)
	for _, m := range value.Messages {
		if m.ID == "" || seen[m.ID] {
			return model.Conversation{}, errors.New("invalid conversation identity")
		}
		seen[m.ID] = true
		total += len(m.Text)
		if (m.Role != "user" && m.Role != "assistant") || len(m.ID) > 128 || len(m.Text) > 256*1024 {
			return model.Conversation{}, errors.New("invalid conversation message")
		}
	}
	if total > 512*1024 {
		return model.Conversation{}, errors.New("conversation exceeds size limit")
	}
	return value, nil
}

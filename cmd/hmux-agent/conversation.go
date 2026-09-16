package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"strconv"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

func runAgentConversation(ctx context.Context, args []string, writer io.Writer) error {
	if len(args) != 4 || args[0] != "--session" || args[2] != "--created-at" {
		return errors.New("usage: hmux-agent conversation --session id --created-at timestamp")
	}
	createdAt, err := strconv.ParseInt(args[3], 10, 64)
	if err != nil || createdAt < 1 || model.ValidateSessionID(args[1]) != nil {
		return errors.New("invalid session identity")
	}
	value, err := catalog.ReadConversation(ctx, args[1], createdAt)
	if err != nil {
		return errors.New("conversation unavailable for this session")
	}
	return json.NewEncoder(writer).Encode(value)
}

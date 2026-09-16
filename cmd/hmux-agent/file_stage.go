package main

import (
	"context"
	"errors"
	"io"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
)

var loadFileStageConfig = config.LoadClient
var defaultFileStageRoot = filestage.DefaultRoot
var receiveFileStage = filestage.Receive

func runAgentFileStage(ctx context.Context, args []string, reader io.Reader, writer io.Writer) error {
	if len(args) != 1 || args[0] != "--stdio" {
		return errors.New("usage: hmux-agent file-stage --stdio")
	}
	cfg, err := loadFileStageConfig("")
	if err != nil {
		return err
	}
	if cfg.Role != "home" {
		return errors.New("file staging is available only on the Home Mac")
	}
	root, err := defaultFileStageRoot()
	if err != nil {
		return err
	}
	verify := func(verifyCtx context.Context, identity filestage.SessionIdentity) error {
		value, readErr := catalog.ReadBasic(verifyCtx, catalog.TmuxRunner{})
		if readErr != nil {
			return readErr
		}
		for _, session := range value.Sessions {
			if session.ID == identity.ID && session.CreatedAt == identity.CreatedAt {
				return nil
			}
		}
		return catalog.ErrSessionChanged
	}
	return receiveFileStage(ctx, root, reader, writer, verify, time.Now())
}

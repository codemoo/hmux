package client

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
)

const fileStageCapability = "file-stage-v1"

var localStageSessionVerifier = func(ctx context.Context, identity filestage.SessionIdentity) error {
	return requireTmuxCreatedAt(ctx, catalog.TmuxRunner{}, identity.ID, identity.CreatedAt)
}

var fileStageDefaultRoot = filestage.DefaultRoot

// VerifyFileStageSession checks the exact tmux identity on Home. Receivers call
// it before consuming the body and again immediately before committing it.
func VerifyFileStageSession(ctx context.Context, identity filestage.SessionIdentity) error {
	return localStageSessionVerifier(ctx, identity)
}

func StageFiles(
	ctx context.Context,
	cfg config.ClientConfig,
	session filestage.SessionIdentity,
	requestID string,
	paths []string,
) (filestage.Response, error) {
	var response filestage.Response
	prepared, err := filestage.Prepare(paths)
	if err != nil {
		return response, err
	}
	defer prepared.Close()

	var output []byte
	if cfg.Role == "home" {
		root, rootErr := fileStageDefaultRoot()
		if rootErr != nil {
			return response, rootErr
		}
		reader, writer := io.Pipe()
		var buffer bytes.Buffer
		receiveErrors := make(chan error, 1)
		go func() {
			receiveErr := filestage.Receive(ctx, root, reader, &buffer, localStageSessionVerifier, time.Now())
			_ = reader.CloseWithError(receiveErr)
			receiveErrors <- receiveErr
		}()
		writeErr := prepared.WriteTo(ctx, writer, requestID, session)
		_ = writer.CloseWithError(writeErr)
		receiveErr := <-receiveErrors
		if writeErr != nil {
			return response, writeErr
		}
		if receiveErr != nil {
			return response, receiveErr
		}
		output = buffer.Bytes()
	} else {
		if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
			return response, errors.New("unsafe home_alias or agent_path")
		}
		if !remoteAgentSupportsCapability(cfg, fileStageCapability) {
			return response, errors.New("Home hmux-agent must be updated before files can be dropped")
		}
		commandCtx, cancel := context.WithCancel(ctx)
		defer cancel()
		command := exec.CommandContext(
			commandCtx, "ssh",
			"-T",
			"-o", "BatchMode=yes",
			"-o", "ForwardAgent=no",
			"-o", "ClearAllForwardings=yes",
			cfg.HomeAlias, "--", cfg.AgentPath, "file-stage", "--stdio",
		)
		stdin, pipeErr := command.StdinPipe()
		if pipeErr != nil {
			return response, pipeErr
		}
		stdout, pipeErr := command.StdoutPipe()
		if pipeErr != nil {
			_ = stdin.Close()
			return response, pipeErr
		}
		command.Stderr = io.Discard
		command.WaitDelay = 2 * time.Second
		if err := command.Start(); err != nil {
			_ = stdin.Close()
			return response, fmt.Errorf("start Home file staging: %w", err)
		}
		writeErr := prepared.WriteTo(commandCtx, stdin, requestID, session)
		closeErr := stdin.Close()
		if writeErr != nil || closeErr != nil {
			cancel()
			_ = stdout.Close()
			_ = command.Wait()
			if writeErr != nil {
				return response, writeErr
			}
			return response, errors.New("Home file-stage input could not be closed")
		}
		output, err = readBounded(stdout, filestage.MaximumResponseBytes)
		if err != nil {
			cancel()
			_ = stdout.Close()
			_ = command.Wait()
			return response, err
		}
		waitErr := command.Wait()
		_ = stdout.Close()
		if waitErr != nil {
			if ctx.Err() != nil {
				return response, ctx.Err()
			}
			return response, fmt.Errorf("Home file staging failed: %w", waitErr)
		}
	}

	response, err = filestage.DecodeResponse(output)
	if err != nil {
		return filestage.Response{}, err
	}
	if err := prepared.ValidateResponse(response, requestID, session); err != nil {
		return filestage.Response{}, err
	}
	return response, nil
}

func readBounded(reader io.Reader, limit int) ([]byte, error) {
	if limit < 1 {
		return nil, errors.New("invalid response limit")
	}
	data, err := io.ReadAll(io.LimitReader(reader, int64(limit)+1))
	if err != nil {
		return nil, err
	}
	if len(data) > limit {
		return nil, errors.New("file-stage response exceeds size limit")
	}
	return data, nil
}

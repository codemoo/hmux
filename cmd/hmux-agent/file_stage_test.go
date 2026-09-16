package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
)

func TestAgentFileStageRequiresExactArgumentsBeforeLoadingConfig(t *testing.T) {
	loaded := false
	previousLoader := loadFileStageConfig
	loadFileStageConfig = func(string) (config.ClientConfig, error) {
		loaded = true
		return config.ClientConfig{}, nil
	}
	t.Cleanup(func() { loadFileStageConfig = previousLoader })
	for _, args := range [][]string{nil, {}, {"--stdio", "extra"}, {"--other"}} {
		if err := runAgentFileStage(context.Background(), args, bytes.NewReader(nil), io.Discard); err == nil {
			t.Fatalf("arguments %v were accepted", args)
		}
	}
	if loaded {
		t.Fatal("config was loaded for invalid arguments")
	}
}

func TestAgentFileStageIsHomeRoleOnly(t *testing.T) {
	previousLoader := loadFileStageConfig
	loadFileStageConfig = func(string) (config.ClientConfig, error) {
		return config.ClientConfig{Role: "remote"}, nil
	}
	t.Cleanup(func() { loadFileStageConfig = previousLoader })
	err := runAgentFileStage(context.Background(), []string{"--stdio"}, bytes.NewReader(nil), io.Discard)
	if err == nil || err.Error() != "file staging is available only on the Home Mac" {
		t.Fatalf("error=%v", err)
	}
}

func TestAgentFileStageDelegatesToReceiverForHomeRole(t *testing.T) {
	previousLoader := loadFileStageConfig
	previousRoot := defaultFileStageRoot
	previousReceive := receiveFileStage
	loadFileStageConfig = func(string) (config.ClientConfig, error) {
		return config.ClientConfig{Role: "home"}, nil
	}
	defaultFileStageRoot = func() (string, error) { return "/private/tmp/hmux/staged-files-v1", nil }
	wantErr := errors.New("receiver called")
	called := false
	receiveFileStage = func(
		_ context.Context,
		root string,
		reader io.Reader,
		writer io.Writer,
		verify filestage.VerifySession,
		now time.Time,
	) error {
		called = true
		if root != "/private/tmp/hmux/staged-files-v1" || reader == nil || writer == nil || verify == nil || now.IsZero() {
			t.Fatalf("invalid receiver arguments root=%q", root)
		}
		return wantErr
	}
	t.Cleanup(func() {
		loadFileStageConfig = previousLoader
		defaultFileStageRoot = previousRoot
		receiveFileStage = previousReceive
	})
	err := runAgentFileStage(context.Background(), []string{"--stdio"}, bytes.NewReader(nil), io.Discard)
	if !called || !errors.Is(err, wantErr) {
		t.Fatalf("called=%t error=%v", called, err)
	}
}

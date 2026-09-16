package main

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"testing"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

func TestForegroundCatalogStreamBypassesAppRuntimeBroker(t *testing.T) {
	directory := t.TempDir()
	configPath := filepath.Join(directory, "client.toml")
	configData := []byte("schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\nupdate_check = true\n")
	if err := os.WriteFile(configPath, configData, 0o600); err != nil {
		t.Fatal(err)
	}

	originalBroker := appRuntimeBrokerRun
	originalStream := foregroundAppCatalogStreamRun
	t.Cleanup(func() {
		appRuntimeBrokerRun = originalBroker
		foregroundAppCatalogStreamRun = originalStream
	})
	brokerCalls := 0
	streamCalls := 0
	appRuntimeBrokerRun = func(config.ClientConfig, []string, bool) error {
		brokerCalls++
		return nil
	}
	foregroundAppCatalogStreamRun = func(config.ClientConfig, io.Writer) error {
		streamCalls++
		return nil
	}

	if err := run([]string{"--config", configPath, "app", "catalog-stream"}); err != nil {
		t.Fatal(err)
	}
	if brokerCalls != 0 || streamCalls != 1 {
		t.Fatalf("broker calls=%d stream calls=%d", brokerCalls, streamCalls)
	}
}

func TestUsageAppParentContextRejectsMismatchedParent(t *testing.T) {
	t.Setenv("HMUX_USAGE_PARENT_PID", strconv.Itoa(os.Getpid()+1000))
	ctx, cancel, err := usageAppParentContext(context.Background(), os.Getppid)
	if cancel != nil {
		cancel()
	}
	if err == nil || ctx != nil {
		t.Fatalf("mismatched parent accepted: ctx=%v err=%v", ctx, err)
	}
}

func TestForegroundUsageStreamBypassesAppRuntimeBroker(t *testing.T) {
	directory := t.TempDir()
	configPath := filepath.Join(directory, "client.toml")
	configData := []byte("schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\nupdate_check = true\n")
	if err := os.WriteFile(configPath, configData, 0o600); err != nil {
		t.Fatal(err)
	}

	originalBroker := appRuntimeBrokerRun
	originalStream := foregroundAppUsageStreamRun
	t.Cleanup(func() {
		appRuntimeBrokerRun = originalBroker
		foregroundAppUsageStreamRun = originalStream
	})
	brokerCalls := 0
	streamCalls := 0
	appRuntimeBrokerRun = func(config.ClientConfig, []string, bool) error {
		brokerCalls++
		return nil
	}
	foregroundAppUsageStreamRun = func(config.ClientConfig, io.Writer) error {
		streamCalls++
		return nil
	}

	if err := run([]string{"--config", configPath, "app", "usage-stream"}); err != nil {
		t.Fatal(err)
	}
	if brokerCalls != 0 || streamCalls != 1 {
		t.Fatalf("broker calls=%d stream calls=%d", brokerCalls, streamCalls)
	}
}

func TestAppUsageStreamReturnsStructuredUpdateStatusForOldAgent(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	var output bytes.Buffer
	if err := runAppUsageStream(context.Background(), cfg, &output); err != nil {
		t.Fatal(err)
	}
	decoder, err := usagestream.NewDecoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	frame, err := decoder.Decode()
	if err != nil || frame.Type != "status" || frame.Code != "home_agent_update_required" || frame.Sequence != 1 {
		t.Fatalf("update status=%#v err=%v", frame, err)
	}
}

func TestAppCatalogStreamReturnsUnsupportedBootstrapForOldAgent(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	var output bytes.Buffer
	if err := runAppCatalogStream(context.Background(), cfg, &output); err != nil {
		t.Fatal(err)
	}
	var bootstrap catalogstream.Bootstrap
	if err := json.Unmarshal(output.Bytes(), &bootstrap); err != nil {
		t.Fatal(err)
	}
	if bootstrap.Supported || bootstrap.URL != "" || bootstrap.Token != "" ||
		bootstrap.MaximumFrameBytes != catalogstream.MaximumFrameSize {
		t.Fatalf("unsupported bootstrap=%#v", bootstrap)
	}
}

func TestPublishLatestCatalogReplacesStalePendingSnapshot(t *testing.T) {
	updates := make(chan model.Catalog, 1)
	publishLatestCatalog(updates, model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: []model.Session{{ID: "$1", CreatedAt: 1}}})
	publishLatestCatalog(updates, model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: []model.Session{{ID: "$2", CreatedAt: 2}}})
	latest := <-updates
	if len(latest.Sessions) != 1 || latest.Sessions[0].ID != "$2" {
		t.Fatalf("latest snapshot=%#v", latest)
	}
}

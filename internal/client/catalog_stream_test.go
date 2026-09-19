package client

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestRemoteCatalogStreamUsesOneFixedSSHCommandAndStopsChild(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	argsPath := filepath.Join(directory, "args")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'catalog-stream-v1\\nhost-metrics-v1\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	var frames bytes.Buffer
	if err := catalogstream.WriteFrame(&frames, catalogstream.SourceFrame{
		StreamProtocolVersion: catalogstream.ProtocolVersion,
		Sequence:              1,
		Type:                  "snapshot",
		Catalog: model.Catalog{
			ProtocolVersion: model.ProtocolVersion,
			GeneratedAt:     time.Now(),
			Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: "one"}},
		},
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, frames.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	supported, err := CatalogStreamSupported(context.Background(), cfg)
	if err != nil || !supported {
		t.Fatalf("supported=%t err=%v", supported, err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	var received model.Catalog
	err = StreamCatalogs(ctx, cfg, func(value model.Catalog) error {
		received = value
		cancel()
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("stream error=%v", err)
	}
	if len(received.Sessions) != 1 || received.Sessions[0].HostAlias != cfg.HomeAlias {
		t.Fatalf("received=%#v", received)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(data)), "\n")
	want := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "catalog-stream", "--stdio", "--host-metrics")
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("stream SSH args=%v", got)
	}
	for _, forbidden := range []string{"StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null", "ForwardAgent=yes", "-L", "-R"} {
		if strings.Contains(string(data), forbidden) {
			t.Fatalf("stream SSH weakened by %q", forbidden)
		}
	}
}

func TestRemoteCatalogStreamRejectsSequenceGap(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'catalog-stream-v1\\nhost-metrics-v1\\n'; exit 0;; esac\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	var frames bytes.Buffer
	if err := catalogstream.WriteFrame(&frames, catalogstream.SourceFrame{
		StreamProtocolVersion: catalogstream.ProtocolVersion,
		Sequence:              2,
		Type:                  "snapshot",
		Catalog:               model.Catalog{ProtocolVersion: model.ProtocolVersion},
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, frames.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	err := StreamCatalogs(context.Background(), cfg, func(model.Catalog) error { return nil })
	if err == nil || !strings.Contains(err.Error(), "sequence gap") {
		t.Fatalf("sequence gap error=%v", err)
	}
}

func TestObservedCatalogFetchRunsBeforeUnchangedSnapshotSuppression(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	value := model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		Sessions:        []model.Session{{ID: "$1", CreatedAt: 42, PanePID: 123}},
	}
	observed := 0
	fetch := observeCatalogFetch(func(context.Context) (model.Catalog, error) {
		value.GeneratedAt = time.Now().UTC()
		return value, nil
	}, func(_ context.Context, got model.Catalog) error {
		observed++
		if got.Sessions[0].PanePID != 123 {
			t.Fatalf("observer lost in-memory pane PID: %#v", got.Sessions[0])
		}
		if observed == 2 {
			cancel()
		}
		return nil
	})
	published := 0
	err := catalogstream.Produce(ctx, time.Millisecond, fetch, func(frame catalogstream.SourceFrame) error {
		if frame.Type == "snapshot" {
			published++
		}
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("produce error=%v", err)
	}
	if observed != 2 || published != 1 {
		t.Fatalf("observed=%d published=%d", observed, published)
	}
}

func TestRemoteCatalogStreamRejectsHomeObserver(t *testing.T) {
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	err := StreamCatalogsObserved(context.Background(), cfg, func(context.Context, model.Catalog) error { return nil }, func(model.Catalog) error { return nil })
	if err == nil || !strings.Contains(err.Error(), "observer requires Home role") {
		t.Fatalf("observer error=%v", err)
	}
}

func TestRemoteCatalogStreamExpiresSilentProducerLease(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'catalog-stream-v1\\n'; exit 0;; esac\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	var frames bytes.Buffer
	if err := catalogstream.WriteFrame(&frames, catalogstream.SourceFrame{
		StreamProtocolVersion: catalogstream.ProtocolVersion,
		Sequence:              1,
		Type:                  "snapshot",
		Catalog:               model.Catalog{ProtocolVersion: model.ProtocolVersion},
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, frames.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	previousLease := catalogStreamSourceLease
	catalogStreamSourceLease = 100 * time.Millisecond
	t.Cleanup(func() { catalogStreamSourceLease = previousLease })
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	start := time.Now()
	err := StreamCatalogs(context.Background(), cfg, func(model.Catalog) error { return nil })
	if err == nil || !strings.Contains(err.Error(), "source lease expired") {
		t.Fatalf("lease error=%v", err)
	}
	if elapsed := time.Since(start); elapsed > 3*time.Second {
		t.Fatalf("silent producer was not reaped promptly: %v", elapsed)
	}
}

func TestRemoteCatalogStreamKeepsLegacyAgentUnflagged(t *testing.T) {
	args, err := runCatalogStreamFixture(t, "catalog-stream-v1", model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		GeneratedAt:     time.Now().UTC(),
		Sessions:        []model.Session{},
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("legacy stream error=%v", err)
	}
	want := append(remoteSSHBaseArgs(config.DefaultClientConfig().HomeAlias), config.DefaultClientConfig().AgentPath, "catalog-stream", "--stdio")
	if !reflect.DeepEqual(args, want) {
		t.Fatalf("legacy stream args=%v want=%v", args, want)
	}
}

func TestRemoteCatalogStreamRejectsUnnegotiatedHostMetrics(t *testing.T) {
	percent := 20.0
	_, err := runCatalogStreamFixture(t, "catalog-stream-v1", model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		GeneratedAt:     time.Now().UTC(),
		Sessions:        []model.Session{},
		HostMetrics: &model.HostMetrics{
			ObservedAt: time.Now().UTC(),
			CPUPercent: &percent,
		},
	})
	if err == nil || !strings.Contains(err.Error(), "unsolicited host metrics") {
		t.Fatalf("unnegotiated metrics error=%v", err)
	}
}

func TestRemoteCatalogStreamDropsInvalidNegotiatedHostMetrics(t *testing.T) {
	percent := 100.1
	_, err := runCatalogStreamFixture(t, "catalog-stream-v1\nhost-metrics-v1", model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		GeneratedAt:     time.Now().UTC(),
		Sessions:        []model.Session{},
		HostMetrics: &model.HostMetrics{
			ObservedAt: time.Now().UTC(),
			CPUPercent: &percent,
		},
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("invalid metrics interrupted catalog: %v", err)
	}
}

func runCatalogStreamFixture(t *testing.T, capabilities string, catalog model.Catalog) ([]string, error) {
	t.Helper()
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	argsPath := filepath.Join(directory, "args")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf '%s\\n' \"$HMUX_FAKE_CAPABILITIES\"; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	var frames bytes.Buffer
	if err := catalogstream.WriteFrame(&frames, catalogstream.SourceFrame{
		StreamProtocolVersion: catalogstream.ProtocolVersion,
		Sequence:              1,
		Type:                  "snapshot",
		Catalog:               catalog,
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, frames.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	t.Setenv("HMUX_FAKE_CAPABILITIES", capabilities)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	ctx, cancel := context.WithCancel(context.Background())
	err := StreamCatalogs(ctx, cfg, func(model.Catalog) error {
		cancel()
		return nil
	})
	data, readErr := os.ReadFile(argsPath)
	if readErr != nil {
		return nil, err
	}
	return strings.Split(strings.TrimSpace(string(data)), "\n"), err
}

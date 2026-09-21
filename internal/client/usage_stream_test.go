package client

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

func usageTestFrame(t *testing.T, sequence uint64) []byte {
	t.Helper()
	snapshot := json.RawMessage(`{"schema":1,"seq":1,"generated_at_utc":"2026-08-24T00:00:00.000Z","provider":"claude","burn_rate_per_min":0,"burn_state":"idle","today_total_tokens":0,"today_sessions":0,"rolling_5h":{"used_pct":0,"remaining_seconds":0,"resets_at":null},"weekly":{"used_pct":0,"remaining_seconds":0,"resets_at":null},"status":{"state":"networkError","data_source":"api_only","quota_source":"none","stale":true}}`)
	var output bytes.Buffer
	encoder, err := usagestream.NewEncoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	if err := encoder.Encode(usagestream.Frame{
		ProtocolVersion: usagestream.ProtocolVersion,
		Sequence:        sequence,
		Type:            "snapshot",
		Provider:        "claude",
		Snapshot:        snapshot,
	}); err != nil {
		t.Fatal(err)
	}
	return output.Bytes()
}

func TestRemoteUsageStreamUsesFixedSSHCommandAndStopsChild(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	argsPath := filepath.Join(directory, "args")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'usage-stream-v1\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, usageTestFrame(t, 1), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	supported, err := UsageStreamSupported(context.Background(), cfg)
	if err != nil || !supported {
		t.Fatalf("supported=%t err=%v", supported, err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	var output cancelWriter
	output.cancel = cancel
	err = StreamUsage(ctx, cfg, &output)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("stream error=%v", err)
	}
	decoder, _ := usagestream.NewDecoder(bytes.NewReader(output.data.Bytes()))
	if frame, decodeErr := decoder.Decode(); decodeErr != nil || frame.Sequence != 1 {
		t.Fatalf("proxied frame=%#v err=%v", frame, decodeErr)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(data)), "\n")
	want := []string{
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		cfg.HomeAlias, "--", cfg.AgentPath, "usage-stream", "--stdio",
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("stream SSH args=%v", got)
	}
	for _, forbidden := range []string{"StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null", "ForwardAgent=yes", "-L", "-R"} {
		if strings.Contains(string(data), forbidden) {
			t.Fatalf("stream SSH weakened by %q", forbidden)
		}
	}
}

func TestRemoteUsageStreamRejectsSequenceGap(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	framesPath := filepath.Join(directory, "frames")
	script := "#!/bin/sh\nset -eu\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(framesPath, usageTestFrame(t, 2), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_FRAMES", framesPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	err := StreamUsage(context.Background(), cfg, &bytes.Buffer{})
	if err == nil || !strings.Contains(err.Error(), "sequence gap") {
		t.Fatalf("sequence gap error=%v", err)
	}
}

func TestRemoteUsageStreamExpiresSilentProducerLease(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nset -eu\nexec sleep 60\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	previousLease := usageStreamSourceLease
	usageStreamSourceLease = 100 * time.Millisecond
	t.Cleanup(func() { usageStreamSourceLease = previousLease })
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	start := time.Now()
	err := StreamUsage(context.Background(), cfg, &bytes.Buffer{})
	if err == nil || !strings.Contains(err.Error(), "source lease expired") {
		t.Fatalf("lease error=%v", err)
	}
	if elapsed := time.Since(start); elapsed > 3*time.Second {
		t.Fatalf("silent producer was not reaped promptly: %v", elapsed)
	}
}

type cancelWriter struct {
	data   bytes.Buffer
	cancel context.CancelFunc
}

func (w *cancelWriter) Write(data []byte) (int, error) {
	count, err := w.data.Write(data)
	if w.cancel != nil {
		w.cancel()
		w.cancel = nil
	}
	return count, err
}

func TestRemoteUsageSourcesRequireAdvertisedCapability(t *testing.T) {
	for _, capable := range []bool{false, true} {
		t.Run(fmt.Sprint(capable), func(t *testing.T) {
			dir := t.TempDir()
			argsPath := filepath.Join(dir, "args")
			frames := filepath.Join(dir, "frames")
			capabilities := "usage-stream-v1"
			if capable {
				capabilities += "\\nusage-sources-v1"
			}
			script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf '" + capabilities + "\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat \"$HMUX_FAKE_FRAMES\"\nexec sleep 60\n"
			if err := os.WriteFile(filepath.Join(dir, "ssh"), []byte(script), 0700); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(frames, usageTestFrame(t, 1), 0600); err != nil {
				t.Fatal(err)
			}
			t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
			t.Setenv("HMUX_FAKE_ARGS", argsPath)
			t.Setenv("HMUX_FAKE_FRAMES", frames)
			cfg := config.DefaultClientConfig()
			cfg.Role = "remote"
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			out := cancelWriter{cancel: cancel}
			if err := StreamUsageWithSources(ctx, cfg, &out); !errors.Is(err, context.Canceled) {
				t.Fatal(err)
			}
			raw, err := os.ReadFile(argsPath)
			if err != nil {
				t.Fatal(err)
			}
			if strings.Contains(string(raw), "--sources") != capable {
				t.Fatal("source option ignored capability", string(raw))
			}
		})
	}
}

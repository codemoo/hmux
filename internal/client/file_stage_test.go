package client

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
)

func TestRemoteStageFilesUsesFixedSSHArgumentsAndPathFreeBinaryInput(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	argsPath := filepath.Join(directory, "args")
	stdinPath := filepath.Join(directory, "stdin")
	responsePath := filepath.Join(directory, "response")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'file-stage-v1\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat > \"$HMUX_FAKE_STDIN\"\ncat \"$HMUX_FAKE_RESPONSE\"\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	localPath := filepath.Join(directory, "sensitive local name.txt")
	contents := []byte("opaque file body")
	if err := os.WriteFile(localPath, contents, 0o600); err != nil {
		t.Fatal(err)
	}
	requestID := "00112233445566778899aabbccddeeff"
	session := filestage.SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000}
	stageID := "ffeeddccbbaa99887766554433221100"
	expires := int64(1_700_086_400)
	hash := sha256.Sum256(contents)
	response := filestage.Response{
		ProtocolVersion: filestage.ProtocolVersion,
		RequestID:       requestID,
		StageID:         stageID,
		Session:         session,
		ExpiresAtUnix:   expires,
		Files: []filestage.StagedFile{{
			Index: 0,
			Path: filepath.Join(
				"/Users/home/Library/Caches/hmux/staged-files-v1",
				"1700086400-"+stageID,
				"file-0001.txt",
			),
			Size:   int64(len(contents)),
			SHA256: hex.EncodeToString(hash[:]),
		}},
	}
	responseData, err := json.Marshal(response)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(responsePath, append(responseData, '\n'), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_STDIN", stdinPath)
	t.Setenv("HMUX_FAKE_RESPONSE", responsePath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	gotResponse, err := StageFiles(context.Background(), cfg, session, requestID, []string{localPath})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(gotResponse, response) {
		t.Fatalf("response=%#v want %#v", gotResponse, response)
	}
	argsData, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	gotArgs := strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs := []string{
		"-T",
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		cfg.HomeAlias, "--", cfg.AgentPath, "file-stage", "--stdio",
	}
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("file-stage SSH args=%v", gotArgs)
	}
	for _, forbidden := range []string{"StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null", "ForwardAgent=yes", "-L", "-R", "scp", "rsync"} {
		if strings.Contains(string(argsData), forbidden) {
			t.Fatalf("file staging SSH weakened by %q", forbidden)
		}
	}
	input, err := os.ReadFile(stdinPath)
	if err != nil {
		t.Fatal(err)
	}
	if len(input) < 12 || string(input[:8]) != "HMXSTG1\n" {
		t.Fatalf("invalid binary prefix: %q", input)
	}
	headerLength := int(binary.BigEndian.Uint32(input[8:12]))
	if headerLength < 1 || 12+headerLength+len(contents) != len(input) {
		t.Fatalf("invalid captured input lengths header=%d total=%d", headerLength, len(input))
	}
	var header filestage.Header
	if err := json.Unmarshal(input[12:12+headerLength], &header); err != nil {
		t.Fatal(err)
	}
	if header.RequestID != requestID || header.Session != session || header.Files[0].Extension != "txt" {
		t.Fatalf("header=%#v", header)
	}
	if !reflect.DeepEqual(input[12+headerLength:], contents) {
		t.Fatal("captured body changed")
	}
	if strings.Contains(string(input[:12+headerLength]), localPath) || strings.Contains(string(input[:12+headerLength]), filepath.Base(localPath)) {
		t.Fatal("local path or original filename leaked into remote metadata")
	}
}

func TestRemoteStageFilesFailsClosedWithoutCapability(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nset -eu\nprintf 'expected-identity-v1\\n'\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	localPath := filepath.Join(directory, "payload.txt")
	if err := os.WriteFile(localPath, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	_, err := StageFiles(
		context.Background(), cfg,
		filestage.SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000},
		"00112233445566778899aabbccddeeff",
		[]string{localPath},
	)
	if err == nil || !strings.Contains(err.Error(), "must be updated") {
		t.Fatalf("error=%v", err)
	}
}

func TestRemoteStageFilesCancellationReapsSSH(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'file-stage-v1\\n'; exit 0;; esac\ncat >/dev/null\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	localPath := filepath.Join(directory, "payload.txt")
	if err := os.WriteFile(localPath, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	started := time.Now()
	_, err := StageFiles(
		ctx, cfg,
		filestage.SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000},
		"00112233445566778899aabbccddeeff",
		[]string{localPath},
	)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > 3*time.Second {
		t.Fatalf("SSH child was not reaped promptly: %v", elapsed)
	}
}

func TestRemoteStageFilesOversizedResponseReapsSSH(t *testing.T) {
	directory := t.TempDir()
	sshPath := filepath.Join(directory, "ssh")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities*) printf 'file-stage-v1\\n'; exit 0;; esac\ncat >/dev/null\ndd if=/dev/zero bs=65537 count=1 2>/dev/null\nexec sleep 60\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	localPath := filepath.Join(directory, "payload.txt")
	if err := os.WriteFile(localPath, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	started := time.Now()
	_, err := StageFiles(
		context.Background(), cfg,
		filestage.SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000},
		"00112233445566778899aabbccddeeff",
		[]string{localPath},
	)
	if err == nil || !strings.Contains(err.Error(), "exceeds size limit") {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > 3*time.Second {
		t.Fatalf("oversized producer was not reaped promptly: %v", elapsed)
	}
}

func TestLocalStageFilesDoesNotDeadlockWhenReceiverRejectsBeforeBody(t *testing.T) {
	directory := t.TempDir()
	localPath := filepath.Join(directory, "payload.txt")
	if err := os.WriteFile(localPath, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	previousRoot := fileStageDefaultRoot
	previousVerifier := localStageSessionVerifier
	fileStageDefaultRoot = func() (string, error) {
		return filepath.Join(directory, "hmux", "staged-files-v1"), nil
	}
	wantErr := errors.New("session changed before body")
	localStageSessionVerifier = func(context.Context, filestage.SessionIdentity) error { return wantErr }
	t.Cleanup(func() {
		fileStageDefaultRoot = previousRoot
		localStageSessionVerifier = previousVerifier
	})
	cfg := config.DefaultClientConfig()
	cfg.Role = "home"
	started := time.Now()
	_, err := StageFiles(
		context.Background(), cfg,
		filestage.SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000},
		"00112233445566778899aabbccddeeff",
		[]string{localPath},
	)
	if err == nil {
		t.Fatal("early receiver rejection was lost")
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("early receiver rejection deadlocked for %v", elapsed)
	}
}

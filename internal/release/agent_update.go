package release

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

const agentUpdateLockName = ".hmux-agent.update.lock"

// UpdateInstalledAgent downloads only the role-bound, signed agent artifact.
// The current process keeps running from its already-open executable while the
// path is replaced atomically for subsequent SSH invocations.
func UpdateInstalledAgent(ctx context.Context, cfg config.ClientConfig, currentVersion string) (bool, error) {
	if !ValidVersion(currentVersion) {
		return false, errors.New("installed agent version is not a release version")
	}
	manifest, err := FetchManifestForPlatform(ctx, cfg, AgentPlatform())
	if err != nil {
		return false, err
	}
	if !IsNewerVersion(manifest.Version, currentVersion) {
		return false, nil
	}
	artifact, err := FetchArtifactForPlatform(ctx, cfg, manifest, AgentPlatform())
	if err != nil {
		return false, err
	}
	target, err := os.Executable()
	if err != nil {
		return false, err
	}
	target, err = filepath.Abs(target)
	if err != nil {
		return false, err
	}
	validator := func(path string) error {
		candidateCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		output, err := safeexec.Output(exec.CommandContext(candidateCtx, path, "version"), 4096)
		if err != nil {
			return fmt.Errorf("validate staged agent: %w", err)
		}
		expected := fmt.Sprintf("hmux-agent %s protocol=%d", manifest.Version, model.ProtocolVersion)
		if strings.TrimSpace(string(output)) != expected {
			return errors.New("staged agent reported an unexpected identity")
		}
		return nil
	}
	if err := installAgentArtifact(ctx, target, currentVersion, manifest, artifact, cfg.PublicKeyPath, time.Now().UTC(), validator); err != nil {
		return false, err
	}
	return true, nil
}

func installAgentArtifact(
	ctx context.Context,
	target string,
	currentVersion string,
	manifest model.Manifest,
	artifact []byte,
	publicKeyPath string,
	now time.Time,
	validateCandidate func(string) error,
) error {
	if !filepath.IsAbs(target) || filepath.Base(target) != "hmux-agent" {
		return errors.New("agent executable path is unsafe")
	}
	if !ValidVersion(currentVersion) || !IsNewerVersion(manifest.Version, currentVersion) {
		return errors.New("agent update is not a newer release")
	}
	if err := VerifyArtifactForPlatform(artifact, manifest, publicKeyPath, AgentPlatform()); err != nil {
		// This second pass binds the bytes, version, role and pinned signer at
		// the mutation boundary even when the fetcher was changed later.
		return err
	}
	parent := filepath.Dir(target)
	if err := ensurePrivateRealDir(parent); err != nil {
		return err
	}
	info, err := os.Lstat(target)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o022 != 0 || info.Mode().Perm()&0o100 == 0 {
		return errors.New("installed agent must be an executable non-writable regular file, not a symlink")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Getuid() || stat.Nlink != 1 {
		return errors.New("installed agent must be singly linked and owned by the current user")
	}

	lock, err := os.OpenFile(filepath.Join(parent, agentUpdateLockName), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 5*time.Second); err != nil {
		return fmt.Errorf("agent update lock: %w", err)
	}
	defer filelock.Unlock(lock)

	// Re-check after taking the lock so a concurrent updater cannot swap the
	// path between validation and backup.
	lockedInfo, err := os.Lstat(target)
	if err != nil || !os.SameFile(info, lockedInfo) {
		return errors.New("installed agent changed during update")
	}
	stage := filepath.Join(parent, fmt.Sprintf(".hmux-agent.next-%d", os.Getpid()))
	if _, err := os.Lstat(stage); err == nil || !errors.Is(err, os.ErrNotExist) {
		return errors.New("agent update staging path already exists")
	}
	if err := config.AtomicWrite(stage, artifact, 0o700); err != nil {
		return err
	}
	defer os.Remove(stage)
	if err := validateCandidate(stage); err != nil {
		return err
	}

	stamp := now.UTC().Format("20060102T150405.000000000Z")
	backup := fmt.Sprintf("%s.hmux-backup-%s-%d", target, stamp, os.Getpid())
	if _, err := os.Lstat(backup); err == nil || !errors.Is(err, os.ErrNotExist) {
		return errors.New("agent backup path already exists")
	}
	if err := copyAgentBackup(target, backup, info.Size()); err != nil {
		return err
	}
	if err := syncDirectory(parent); err != nil {
		return fmt.Errorf("sync agent backup: %w", err)
	}
	if err := os.Rename(stage, target); err != nil {
		return err
	}
	if err := syncDirectory(parent); err != nil {
		return restoreAgentBackup(parent, target, backup, fmt.Errorf("sync installed agent: %w", err))
	}
	if err := validateCandidate(target); err != nil {
		rejected := fmt.Sprintf("%s.hmux-rejected-%s-%d", target, stamp, os.Getpid())
		// A hard link preserves rejected bytes without ever removing target.
		_ = os.Link(target, rejected)
		return restoreAgentBackup(parent, target, backup, fmt.Errorf("installed agent validation failed: %w", err))
	}
	return nil
}

func copyAgentBackup(source, destination string, expectedSize int64) error {
	if expectedSize < 1 || expectedSize > 256*1024*1024 {
		return errors.New("installed agent size is outside the backup limit")
	}
	input, err := os.Open(source)
	if err != nil {
		return err
	}
	defer input.Close()
	inputInfo, err := input.Stat()
	if err != nil || !inputInfo.Mode().IsRegular() || inputInfo.Size() != expectedSize {
		return errors.New("installed agent changed before backup")
	}
	output, err := os.OpenFile(destination, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o700)
	if err != nil {
		return err
	}
	complete := false
	defer func() {
		_ = output.Close()
		if !complete {
			_ = os.Remove(destination)
		}
	}()
	written, copyErr := io.Copy(output, io.LimitReader(input, expectedSize+1))
	if copyErr != nil || written != expectedSize {
		return errors.New("installed agent changed while creating backup")
	}
	if err := output.Sync(); err != nil {
		return err
	}
	if err := output.Close(); err != nil {
		return err
	}
	complete = true
	return nil
}

func restoreAgentBackup(parent, target, backup string, cause error) error {
	if restoreErr := os.Rename(backup, target); restoreErr != nil {
		return fmt.Errorf("%v; atomically restore previous agent: %w", cause, restoreErr)
	}
	if syncErr := syncDirectory(parent); syncErr != nil {
		return fmt.Errorf("%v; previous agent rolled back but directory sync failed: %w", cause, syncErr)
	}
	return fmt.Errorf("previous agent rolled back after update failure: %w", cause)
}

func syncDirectory(path string) error {
	directory, err := os.Open(path)
	if err != nil {
		return err
	}
	defer directory.Close()
	return directory.Sync()
}

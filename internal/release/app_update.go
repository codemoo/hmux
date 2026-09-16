package release

import (
	"archive/zip"
	"bytes"
	"context"
	"debug/macho"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

const (
	appBundleName             = "HMux.app"
	appBundleIdentifier       = "dev.hmux.app"
	appUpdateLockName         = ".hmux-app.update.lock"
	appUpdateStatusName       = "native-app-update.json"
	appRollbackHoldName       = "native-app-rollback.json"
	maximumAppArchiveEntries  = 20_000
	maximumAppUncompressed    = int64(1024 * 1024 * 1024)
	maximumAppSymlinkTarget   = int64(4096)
	maximumAppUpdateStateSize = int64(4096)
)

type AppUpdateStatus struct {
	Version     string    `json:"version"`
	InstalledAt time.Time `json:"installed_at"`
}

type AppRollbackStatus struct {
	BlockedVersion string    `json:"blocked_version"`
	RolledBackTo   string    `json:"rolled_back_to"`
	RolledBackAt   time.Time `json:"rolled_back_at"`
}

// UpdateInstalledApp verifies and atomically swaps a complete HMux bundle.
// macOS keeps the already-running executable mapped, so the current UI remains
// alive and can offer a deliberate restart after the on-disk bundle is ready.
func UpdateInstalledApp(
	ctx context.Context,
	cfg config.ClientConfig,
	currentVersion string,
	bundlePath string,
) (bool, error) {
	if !ValidVersion(currentVersion) {
		return false, errors.New("running app version is not a release version")
	}
	if err := recoverInterruptedInstalledApp(ctx, bundlePath, currentVersion); err != nil {
		return false, err
	}
	manifest, err := FetchManifestForPlatform(ctx, cfg, AppPlatform())
	if err != nil {
		return false, err
	}
	if !IsNewerVersion(manifest.Version, currentVersion) {
		return false, nil
	}
	rollback, err := readAppRollbackStatus(cfg.CacheDir)
	if err != nil {
		return false, err
	}
	if rollback != nil && rollback.BlockedVersion == manifest.Version && rollback.RolledBackTo == currentVersion {
		return false, nil
	}
	artifact, err := FetchArtifactForPlatform(ctx, cfg, manifest, AppPlatform())
	if err != nil {
		return false, err
	}
	if err := installAppArtifact(ctx, bundlePath, currentVersion, manifest, artifact, cfg.PublicKeyPath, time.Now().UTC()); err != nil {
		return false, err
	}
	status := AppUpdateStatus{Version: manifest.Version, InstalledAt: time.Now().UTC()}
	if err := recordAppUpdateStatus(cfg.CacheDir, status); err != nil {
		// The on-disk validated bundle remains authoritative. The caller can
		// derive the restart banner from it even if optional status persistence
		// is temporarily unavailable.
		return true, nil
	}
	_ = clearAppRollbackStatus(cfg.CacheDir)
	return true, nil
}

func recoverInterruptedInstalledApp(ctx context.Context, target, runningVersion string) error {
	if !filepath.IsAbs(target) || filepath.Base(target) != appBundleName || !ValidVersion(runningVersion) {
		return errors.New("installed app recovery path is unsafe")
	}
	target = filepath.Clean(target)
	parent := filepath.Dir(target)
	if err := ensurePrivateRealDir(parent); err != nil {
		return fmt.Errorf("app location does not support safe automatic replacement: %w", err)
	}
	info, err := os.Lstat(target)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("installed app must be a real directory, not a symlink")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("installed app must be owned by the current user")
	}
	installedVersion, err := plistValue(ctx, filepath.Join(target, "Contents", "Info.plist"), "CFBundleShortVersionString")
	if err != nil || !ValidVersion(installedVersion) ||
		(installedVersion != runningVersion && !IsNewerVersion(installedVersion, runningVersion)) {
		return errors.New("installed app version is invalid")
	}
	if err := validateAppBundle(ctx, target, installedVersion); err != nil {
		return fmt.Errorf("validate installed app before recovery: %w", err)
	}
	lock, err := os.OpenFile(filepath.Join(parent, appUpdateLockName), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 5*time.Second); err != nil {
		return fmt.Errorf("native app recovery lock: %w", err)
	}
	defer filelock.Unlock(lock)
	lockedInfo, err := os.Lstat(target)
	if err != nil || !os.SameFile(info, lockedInfo) {
		return errors.New("installed app changed during recovery")
	}
	return recoverAppStagesLocked(ctx, parent, target, installedVersion)
}

func ReadAppUpdateStatus(cacheDir, runningVersion string) (*AppUpdateStatus, error) {
	if !ValidVersion(runningVersion) {
		return nil, errors.New("running app version is invalid")
	}
	base := filepath.Clean(cacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return nil, errors.New("unsafe app update state directory")
	}
	statePath := filepath.Join(base, appUpdateStatusName)
	info, err := os.Lstat(statePath)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o022 != 0 ||
		info.Size() < 1 || info.Size() > maximumAppUpdateStateSize {
		return nil, errors.New("app update state must be a small non-writable regular file")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return nil, errors.New("app update state must be owned by the current user")
	}
	data, err := os.ReadFile(statePath)
	if err != nil {
		return nil, err
	}
	var status AppUpdateStatus
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&status); err != nil {
		return nil, err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return nil, errors.New("app update state contains trailing data")
	}
	if !ValidVersion(status.Version) || status.InstalledAt.IsZero() {
		return nil, errors.New("app update state is invalid")
	}
	if !IsNewerVersion(status.Version, runningVersion) {
		return nil, nil
	}
	rollback, err := readAppRollbackStatus(cacheDir)
	if err != nil {
		return nil, err
	}
	if rollback != nil && rollback.BlockedVersion == status.Version && rollback.RolledBackTo == runningVersion {
		return nil, nil
	}
	return &status, nil
}

func RollbackInstalledApp(ctx context.Context, cfg config.ClientConfig, target string) (string, error) {
	if !filepath.IsAbs(target) || filepath.Base(target) != appBundleName {
		return "", errors.New("app bundle path is unsafe")
	}
	target = filepath.Clean(target)
	parent := filepath.Dir(target)
	if err := ensurePrivateRealDir(parent); err != nil {
		return "", fmt.Errorf("app location does not support safe rollback: %w", err)
	}

	lock, err := os.OpenFile(filepath.Join(parent, appUpdateLockName), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return "", err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 5*time.Second); err != nil {
		return "", fmt.Errorf("native app rollback lock: %w", err)
	}
	defer filelock.Unlock(lock)

	currentVersion, err := plistValue(ctx, filepath.Join(target, "Contents", "Info.plist"), "CFBundleShortVersionString")
	if err != nil || !ValidVersion(currentVersion) {
		return "", errors.New("installed app version is invalid")
	}
	if err := validateAppBundle(ctx, target, currentVersion); err != nil {
		return "", fmt.Errorf("validate installed app before rollback: %w", err)
	}
	if err := recoverAppStagesLocked(ctx, parent, target, currentVersion); err != nil {
		return "", fmt.Errorf("recover interrupted native app update: %w", err)
	}
	candidates, err := appRollbackCandidates(parent, filepath.Base(target), currentVersion)
	if err != nil {
		return "", err
	}
	var backup appRollbackCandidate
	for _, candidate := range candidates {
		if err := validateAppBundle(ctx, candidate.path, candidate.version); err == nil {
			backup = candidate
			break
		}
	}
	if backup.path == "" {
		return "", errors.New("no validated previous HMux.app backup is available")
	}
	if err := atomicSwapPaths(target, backup.path); err != nil {
		return "", fmt.Errorf("atomically roll back app bundle: %w", err)
	}
	restoreCurrent := func(cause error) (string, error) {
		if restoreErr := atomicSwapPaths(target, backup.path); restoreErr != nil {
			return "", fmt.Errorf("%v; atomically restore current app: %w", cause, restoreErr)
		}
		_ = syncDirectory(parent)
		return "", cause
	}
	if err := syncDirectory(parent); err != nil {
		return restoreCurrent(fmt.Errorf("sync rolled-back app bundle: %w", err))
	}
	if err := validateAppBundle(ctx, target, backup.version); err != nil {
		return restoreCurrent(fmt.Errorf("validate rolled-back app bundle: %w", err))
	}
	hold := AppRollbackStatus{
		BlockedVersion: currentVersion,
		RolledBackTo:   backup.version,
		RolledBackAt:   time.Now().UTC(),
	}
	if err := recordAppRollbackStatus(cfg.CacheDir, hold); err != nil {
		return restoreCurrent(fmt.Errorf("record native app rollback hold: %w", err))
	}
	return backup.version, nil
}

type appRollbackCandidate struct {
	path    string
	version string
	stamp   time.Time
}

func appRollbackCandidates(parent, targetName, currentVersion string) ([]appRollbackCandidate, error) {
	entries, err := os.ReadDir(parent)
	if err != nil {
		return nil, err
	}
	prefix := targetName + ".hmux-backup-"
	candidates := make([]appRollbackCandidate, 0, 4)
	for _, entry := range entries {
		if len(candidates) >= 128 || !strings.HasPrefix(entry.Name(), prefix) {
			continue
		}
		remainder := strings.TrimPrefix(entry.Name(), prefix)
		pidSeparator := strings.LastIndexByte(remainder, '-')
		if pidSeparator < 1 {
			continue
		}
		if _, err := strconv.ParseUint(remainder[pidSeparator+1:], 10, 31); err != nil {
			continue
		}
		versionAndStamp := remainder[:pidSeparator]
		stampSeparator := strings.LastIndexByte(versionAndStamp, '-')
		if stampSeparator < 1 {
			continue
		}
		version := versionAndStamp[:stampSeparator]
		stamp, err := time.Parse("20060102T150405.000000000Z", versionAndStamp[stampSeparator+1:])
		if err != nil || !ValidVersion(version) || !IsNewerVersion(currentVersion, version) {
			continue
		}
		path := filepath.Join(parent, entry.Name())
		info, err := os.Lstat(path)
		if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
			continue
		}
		if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
			continue
		}
		candidates = append(candidates, appRollbackCandidate{path: path, version: version, stamp: stamp})
	}
	sort.Slice(candidates, func(left, right int) bool { return candidates[left].stamp.After(candidates[right].stamp) })
	return candidates, nil
}

func recordAppRollbackStatus(cacheDir string, status AppRollbackStatus) error {
	if !ValidVersion(status.BlockedVersion) || !ValidVersion(status.RolledBackTo) ||
		!IsNewerVersion(status.BlockedVersion, status.RolledBackTo) || status.RolledBackAt.IsZero() {
		return errors.New("invalid native app rollback status")
	}
	data, err := json.Marshal(status)
	if err != nil {
		return err
	}
	base := filepath.Clean(cacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return errors.New("unsafe native app rollback state directory")
	}
	return config.AtomicWrite(filepath.Join(base, appRollbackHoldName), data, 0o600)
}

func readAppRollbackStatus(cacheDir string) (*AppRollbackStatus, error) {
	base := filepath.Clean(cacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return nil, errors.New("unsafe native app rollback state directory")
	}
	statePath := filepath.Join(base, appRollbackHoldName)
	info, err := os.Lstat(statePath)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o022 != 0 ||
		info.Size() < 1 || info.Size() > maximumAppUpdateStateSize {
		return nil, errors.New("native app rollback state must be a small non-writable regular file")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return nil, errors.New("native app rollback state must be owned by the current user")
	}
	data, err := os.ReadFile(statePath)
	if err != nil {
		return nil, err
	}
	var status AppRollbackStatus
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&status); err != nil {
		return nil, err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return nil, errors.New("native app rollback state contains trailing data")
	}
	if !ValidVersion(status.BlockedVersion) || !ValidVersion(status.RolledBackTo) ||
		!IsNewerVersion(status.BlockedVersion, status.RolledBackTo) || status.RolledBackAt.IsZero() {
		return nil, errors.New("native app rollback state is invalid")
	}
	return &status, nil
}

func clearAppRollbackStatus(cacheDir string) error {
	base := filepath.Clean(cacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return errors.New("unsafe native app rollback state directory")
	}
	path := filepath.Join(base, appRollbackHoldName)
	err := os.Remove(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}

// DetectInstalledAppUpdate derives restart state from the validated bundle
// when optional cache status could not be persisted after a successful swap.
func DetectInstalledAppUpdate(
	ctx context.Context,
	bundlePath string,
	runningVersion string,
) (*AppUpdateStatus, error) {
	if !ValidVersion(runningVersion) || !filepath.IsAbs(bundlePath) || filepath.Base(bundlePath) != appBundleName {
		return nil, errors.New("invalid installed app update lookup")
	}
	version, err := plistValue(ctx, filepath.Join(bundlePath, "Contents", "Info.plist"), "CFBundleShortVersionString")
	if err != nil {
		return nil, err
	}
	if !IsNewerVersion(version, runningVersion) {
		return nil, nil
	}
	if err := validateAppBundle(ctx, bundlePath, version); err != nil {
		return nil, err
	}
	return &AppUpdateStatus{Version: version, InstalledAt: time.Now().UTC()}, nil
}

func recordAppUpdateStatus(cacheDir string, status AppUpdateStatus) error {
	if !ValidVersion(status.Version) || status.InstalledAt.IsZero() {
		return errors.New("invalid app update status")
	}
	base := filepath.Clean(cacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return errors.New("unsafe app update state directory")
	}
	data, err := json.Marshal(status)
	if err != nil {
		return err
	}
	return config.AtomicWrite(filepath.Join(base, appUpdateStatusName), data, 0o600)
}

func installAppArtifact(
	ctx context.Context,
	target string,
	currentVersion string,
	manifest model.Manifest,
	artifact []byte,
	publicKeyPath string,
	now time.Time,
) error {
	if !filepath.IsAbs(target) || filepath.Base(target) != appBundleName {
		return errors.New("app bundle path is unsafe")
	}
	target = filepath.Clean(target)
	if !ValidVersion(currentVersion) || !IsNewerVersion(manifest.Version, currentVersion) {
		return errors.New("app update is not a newer release")
	}
	if err := VerifyArtifactForPlatform(artifact, manifest, publicKeyPath, AppPlatform()); err != nil {
		return err
	}
	parent := filepath.Dir(target)
	if err := ensurePrivateRealDir(parent); err != nil {
		return fmt.Errorf("app location does not support safe automatic replacement: %w", err)
	}
	currentInfo, err := os.Lstat(target)
	if err != nil {
		return err
	}
	if currentInfo.Mode()&os.ModeSymlink != 0 || !currentInfo.IsDir() {
		return errors.New("installed app must be a real directory, not a symlink")
	}
	if stat, ok := currentInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("installed app must be owned by the current user")
	}
	installedVersion, versionErr := plistValue(ctx, filepath.Join(target, "Contents", "Info.plist"), "CFBundleShortVersionString")
	if versionErr != nil || !ValidVersion(installedVersion) {
		return errors.New("installed app version is invalid")
	}
	if installedVersion != currentVersion && installedVersion != manifest.Version {
		return errors.New("installed app version changed unexpectedly")
	}
	if err := validateAppBundle(ctx, target, installedVersion); err != nil {
		return fmt.Errorf("validate installed app: %w", err)
	}

	lock, err := os.OpenFile(filepath.Join(parent, appUpdateLockName), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 5*time.Second); err != nil {
		return fmt.Errorf("native app update lock: %w", err)
	}
	defer filelock.Unlock(lock)
	lockedInfo, err := os.Lstat(target)
	if err != nil || !os.SameFile(currentInfo, lockedInfo) {
		return errors.New("installed app changed during update")
	}
	if err := recoverAppStagesLocked(ctx, parent, target, installedVersion); err != nil {
		return fmt.Errorf("recover interrupted native app update: %w", err)
	}
	if installedVersion == manifest.Version {
		return nil
	}

	stageRoot := filepath.Join(parent, fmt.Sprintf(".hmux-app-stage-%d", os.Getpid()))
	if _, err := os.Lstat(stageRoot); err == nil || !errors.Is(err, os.ErrNotExist) {
		return errors.New("app update staging path already exists")
	}
	if err := os.Mkdir(stageRoot, 0o700); err != nil {
		return err
	}
	cleanupStage := true
	defer func() {
		if cleanupStage {
			removeAppStage(parent, stageRoot)
		}
	}()
	candidate, err := extractAppArchive(artifact, stageRoot)
	if err != nil {
		return err
	}
	if err := validateAppBundle(ctx, candidate, manifest.Version); err != nil {
		return fmt.Errorf("validate staged app: %w", err)
	}

	stamp := now.UTC().Format("20060102T150405.000000000Z")
	backup := fmt.Sprintf("%s.hmux-backup-%s-%s-%d", target, currentVersion, stamp, os.Getpid())
	if _, err := os.Lstat(backup); err == nil || !errors.Is(err, os.ErrNotExist) {
		return errors.New("app backup path already exists")
	}
	if err := atomicSwapPaths(target, candidate); err != nil {
		return fmt.Errorf("atomically exchange app bundles: %w", err)
	}
	rollbackSwap := func(cause error) error {
		if restoreErr := atomicSwapPaths(target, candidate); restoreErr != nil {
			cleanupStage = false
			return fmt.Errorf("%v; atomically restore previous app: %w", cause, restoreErr)
		}
		if syncErr := syncDirectory(parent); syncErr != nil {
			return fmt.Errorf("%v; previous app restored but directory sync failed: %w", cause, syncErr)
		}
		return cause
	}
	if err := syncDirectory(parent); err != nil {
		return rollbackSwap(fmt.Errorf("sync exchanged app bundle: %w", err))
	}
	if err := validateAppBundle(ctx, target, manifest.Version); err != nil {
		return rollbackSwap(fmt.Errorf("installed app validation failed: %w", err))
	}
	if err := os.Rename(candidate, backup); err != nil {
		return rollbackSwap(fmt.Errorf("preserve previous app backup: %w", err))
	}
	if err := syncDirectory(parent); err != nil {
		if restoreErr := atomicSwapPaths(target, backup); restoreErr != nil {
			return fmt.Errorf("sync app backup: %v; atomically restore previous app: %w", err, restoreErr)
		}
		if restoreSyncErr := syncDirectory(parent); restoreSyncErr != nil {
			return fmt.Errorf("sync app backup: %v; previous app restored but directory sync failed: %w", err, restoreSyncErr)
		}
		return fmt.Errorf("sync app backup after replacement: %w", err)
	}
	return nil
}

func removeAppStage(parent, stageRoot string) {
	if filepath.Dir(stageRoot) == parent {
		if _, ok := appStagePID(filepath.Base(stageRoot)); !ok {
			return
		}
		_ = os.RemoveAll(stageRoot)
	}
}

func appStagePID(name string) (uint64, bool) {
	const prefix = ".hmux-app-stage-"
	if !strings.HasPrefix(name, prefix) {
		return 0, false
	}
	value := strings.TrimPrefix(name, prefix)
	pid, err := strconv.ParseUint(value, 10, 31)
	return pid, err == nil && pid > 0
}

// recoverAppStagesLocked adopts the previous validated bundle left inside a
// stage directory if the process died after RENAME_SWAP but before the backup
// rename. A stage containing a newer candidate (pre-swap crash) is discarded.
// The caller must hold appUpdateLockName and must have validated target.
func recoverAppStagesLocked(
	ctx context.Context,
	parent string,
	target string,
	currentVersion string,
) error {
	entries, err := os.ReadDir(parent)
	if err != nil {
		return err
	}
	seen := 0
	changed := false
	for _, entry := range entries {
		stagePID, ok := appStagePID(entry.Name())
		if !ok {
			continue
		}
		seen++
		if seen > 128 {
			return errors.New("too many native app staging directories")
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		stageRoot := filepath.Join(parent, entry.Name())
		info, statErr := os.Lstat(stageRoot)
		if statErr != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() || info.Mode().Perm()&0o077 != 0 {
			return errors.New("native app staging directory is unsafe")
		}
		stat, ok := info.Sys().(*syscall.Stat_t)
		if !ok || int(stat.Uid) != os.Getuid() {
			return errors.New("native app staging directory has an invalid owner")
		}

		candidate := filepath.Join(stageRoot, appBundleName)
		candidateVersion, versionErr := plistValue(
			ctx,
			filepath.Join(candidate, "Contents", "Info.plist"),
			"CFBundleShortVersionString",
		)
		if versionErr == nil && ValidVersion(candidateVersion) &&
			IsNewerVersion(currentVersion, candidateVersion) &&
			validateAppBundle(ctx, candidate, candidateVersion) == nil {
			stamp := info.ModTime().UTC().Format("20060102T150405.000000000Z")
			backup := fmt.Sprintf("%s.hmux-backup-%s-%s-%d", target, candidateVersion, stamp, stagePID)
			if _, err := os.Lstat(backup); err == nil || !errors.Is(err, os.ErrNotExist) {
				return errors.New("recovered native app backup path already exists")
			}
			if err := os.Rename(candidate, backup); err != nil {
				return err
			}
		}
		removeAppStage(parent, stageRoot)
		changed = true
	}
	if changed {
		return syncDirectory(parent)
	}
	return nil
}

type appArchiveEntry struct {
	file    *zip.File
	name    string
	mode    os.FileMode
	symlink bool
}

func extractAppArchive(data []byte, stageRoot string) (string, error) {
	reader, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		return "", err
	}
	if len(reader.File) < 1 || len(reader.File) > maximumAppArchiveEntries {
		return "", errors.New("app archive has an invalid entry count")
	}
	entries := make([]appArchiveEntry, 0, len(reader.File))
	seen := make(map[string]string, len(reader.File))
	symlinks := make(map[string]struct{})
	var uncompressed int64
	for _, file := range reader.File {
		name := strings.TrimSuffix(file.Name, "/")
		if name == "__MACOSX" || strings.HasPrefix(name, "__MACOSX/") {
			continue
		}
		if err := validateAppArchiveName(name); err != nil {
			return "", err
		}
		folded := strings.ToLower(name)
		if previous, exists := seen[folded]; exists {
			return "", fmt.Errorf("app archive contains duplicate paths %q and %q", previous, name)
		}
		seen[folded] = name
		if file.UncompressedSize64 > uint64(maximumAppUncompressed) ||
			uncompressed > maximumAppUncompressed-int64(file.UncompressedSize64) {
			return "", errors.New("app archive exceeds the uncompressed size limit")
		}
		uncompressed += int64(file.UncompressedSize64)
		mode := file.Mode()
		entry := appArchiveEntry{file: file, name: name, mode: mode, symlink: mode&os.ModeSymlink != 0}
		if !entry.symlink && !mode.IsDir() && !mode.IsRegular() {
			return "", fmt.Errorf("app archive entry %q has an unsupported type", name)
		}
		if entry.symlink {
			symlinks[name] = struct{}{}
		}
		entries = append(entries, entry)
	}
	if len(entries) == 0 {
		return "", errors.New("app archive contains no bundle entries")
	}
	for _, entry := range entries {
		for ancestor := path.Dir(entry.name); ancestor != "."; ancestor = path.Dir(ancestor) {
			if _, exists := symlinks[ancestor]; exists {
				return "", fmt.Errorf("app archive entry %q descends through a symlink", entry.name)
			}
		}
	}

	for _, entry := range entries {
		if entry.symlink {
			continue
		}
		destination := filepath.Join(stageRoot, filepath.FromSlash(entry.name))
		if entry.mode.IsDir() {
			if err := os.MkdirAll(destination, 0o700); err != nil {
				return "", err
			}
			continue
		}
		if err := os.MkdirAll(filepath.Dir(destination), 0o700); err != nil {
			return "", err
		}
		input, err := entry.file.Open()
		if err != nil {
			return "", err
		}
		mode := os.FileMode(0o600)
		if entry.mode.Perm()&0o111 != 0 {
			mode = 0o700
		}
		output, err := os.OpenFile(destination, os.O_CREATE|os.O_EXCL|os.O_WRONLY, mode)
		if err != nil {
			_ = input.Close()
			return "", err
		}
		written, copyErr := io.Copy(output, io.LimitReader(input, int64(entry.file.UncompressedSize64)+1))
		closeErr := output.Close()
		inputErr := input.Close()
		if copyErr != nil || closeErr != nil || inputErr != nil || written != int64(entry.file.UncompressedSize64) {
			return "", errors.New("app archive entry changed while extracting")
		}
	}

	candidate := filepath.Join(stageRoot, appBundleName)
	for _, entry := range entries {
		if !entry.symlink {
			continue
		}
		if entry.file.UncompressedSize64 < 1 || entry.file.UncompressedSize64 > uint64(maximumAppSymlinkTarget) {
			return "", errors.New("app archive symlink target has an invalid size")
		}
		input, err := entry.file.Open()
		if err != nil {
			return "", err
		}
		targetData, readErr := io.ReadAll(io.LimitReader(input, maximumAppSymlinkTarget+1))
		closeErr := input.Close()
		if readErr != nil || closeErr != nil || int64(len(targetData)) != int64(entry.file.UncompressedSize64) {
			return "", errors.New("app archive symlink changed while extracting")
		}
		target := string(targetData)
		if err := validateAppSymlink(entry.name, target); err != nil {
			return "", err
		}
		destination := filepath.Join(stageRoot, filepath.FromSlash(entry.name))
		if err := os.Symlink(target, destination); err != nil {
			return "", err
		}
	}
	resolvedCandidate, err := filepath.EvalSymlinks(candidate)
	if err != nil {
		return "", err
	}
	for name := range symlinks {
		resolved, err := filepath.EvalSymlinks(filepath.Join(stageRoot, filepath.FromSlash(name)))
		if err != nil {
			return "", fmt.Errorf("app archive contains a dangling or cyclic symlink: %w", err)
		}
		if resolved != resolvedCandidate && !strings.HasPrefix(resolved, resolvedCandidate+string(os.PathSeparator)) {
			return "", errors.New("app archive symlink resolves outside HMux.app")
		}
	}
	return candidate, nil
}

func validateAppArchiveName(name string) error {
	if name == "" || len(name) > 1024 || strings.ContainsAny(name, "\\\x00") || path.IsAbs(name) || path.Clean(name) != name {
		return errors.New("app archive contains an unsafe path")
	}
	if name != appBundleName && !strings.HasPrefix(name, appBundleName+"/") {
		return errors.New("app archive contains data outside HMux.app")
	}
	for _, r := range name {
		if r == ':' || unicode.IsControl(r) {
			return errors.New("app archive path contains unsupported characters")
		}
	}
	return nil
}

func validateAppSymlink(name, target string) error {
	if target == "" || strings.ContainsRune(target, 0) || filepath.IsAbs(target) || strings.Contains(target, "\\") {
		return fmt.Errorf("app archive symlink %q has an unsafe target", name)
	}
	resolved := path.Clean(path.Join(path.Dir(name), target))
	if resolved != appBundleName && !strings.HasPrefix(resolved, appBundleName+"/") {
		return fmt.Errorf("app archive symlink %q escapes HMux.app", name)
	}
	return nil
}

func validateAppBundle(ctx context.Context, bundlePath, expectedVersion string) error {
	info, err := os.Lstat(bundlePath)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("HMux.app must be a real directory")
	}
	plistPath := filepath.Join(bundlePath, "Contents", "Info.plist")
	identifier, err := plistValue(ctx, plistPath, "CFBundleIdentifier")
	if err != nil || identifier != appBundleIdentifier {
		return errors.New("app bundle identifier is invalid")
	}
	version, err := plistValue(ctx, plistPath, "CFBundleShortVersionString")
	if err != nil || version != expectedVersion {
		return errors.New("app bundle version does not match the signed manifest")
	}
	executable, err := plistValue(ctx, plistPath, "CFBundleExecutable")
	if err != nil || executable != "ghostty" {
		return errors.New("app bundle executable is invalid")
	}
	for _, binary := range []string{
		filepath.Join(bundlePath, "Contents", "MacOS", executable),
		filepath.Join(bundlePath, "Contents", "Helpers", "hmux"),
	} {
		if err := validateMachOArchitecture(binary); err != nil {
			return err
		}
	}
	if err := ValidateAppBackend(
		ctx,
		filepath.Join(bundlePath, "Contents", "Helpers", "hmux"),
		CurrentAppBackendRequirement(),
	); err != nil {
		return fmt.Errorf("app helper protocol validation failed: %w", err)
	}
	command := exec.CommandContext(ctx, "/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2", bundlePath)
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		return fmt.Errorf("app code signature validation failed: %w: %s", err, model.SafeText(safeexec.Stderr(err), 500))
	}
	return nil
}

func plistValue(ctx context.Context, plistPath, key string) (string, error) {
	if strings.ContainsAny(key, " :/\t\r\n") {
		return "", errors.New("invalid plist key")
	}
	command := exec.CommandContext(ctx, "/usr/libexec/PlistBuddy", "-c", "Print :"+key, plistPath)
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return "", err
	}
	value := strings.TrimSpace(string(output))
	if value == "" || strings.ContainsAny(value, "\r\n") {
		return "", errors.New("invalid plist value")
	}
	return value, nil
}

func validateMachOArchitecture(binaryPath string) error {
	info, err := os.Lstat(binaryPath)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return errors.New("app bundle contains an invalid executable")
	}
	wanted := macho.CpuArm64
	if runtime.GOARCH == "amd64" {
		wanted = macho.CpuAmd64
	} else if runtime.GOARCH != "arm64" {
		return errors.New("unsupported app architecture")
	}
	if file, err := macho.Open(binaryPath); err == nil {
		defer file.Close()
		if file.Cpu != wanted {
			return errors.New("app executable architecture mismatch")
		}
		return nil
	}
	fat, err := macho.OpenFat(binaryPath)
	if err != nil {
		return errors.New("app executable is not Mach-O")
	}
	defer fat.Close()
	for _, architecture := range fat.Arches {
		if architecture.Cpu == wanted {
			return nil
		}
	}
	return errors.New("app executable does not contain the current architecture")
}

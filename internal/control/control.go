package control

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/binary"
	"encoding/csv"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/release"
	"github.com/codemoo/hmux/internal/sshconfig"
)

type Store struct {
	Root string
}

type ArtifactFlag []string

func (a *ArtifactFlag) String() string { return strings.Join(*a, ",") }
func (a *ArtifactFlag) Set(value string) error {
	*a = append(*a, value)
	return nil
}

func (s Store) InventoryPath() string { return filepath.Join(s.Root, "inventory", "inventory.toml") }

func (s Store) Validate() (model.Inventory, error) {
	return config.LoadInventory(s.InventoryPath())
}

func (s Store) Reconcile(ctx context.Context) error {
	return s.withLock(func() error {
		inventory, err := s.Validate()
		if err != nil {
			return err
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
			return s.reconcileLocked(inventory)
		}
	})
}

func (s Store) reconcileLocked(inventory model.Inventory) error {
	sshData, err := sshconfig.Render(inventory)
	if err != nil {
		return err
	}
	csvData, err := renderTermiusCSV(inventory)
	if err != nil {
		return err
	}
	status := map[string]any{
		"schema_version": model.SchemaVersion,
		"revision":       inventory.Revision,
		"reconciled_at":  time.Now().UTC(),
		"termius_mode":   "generated-import-artifact",
		"delete_enabled": false,
	}
	statusData, err := json.MarshalIndent(status, "", "  ")
	if err != nil {
		return err
	}
	return atomicWriteSet([]fileUpdate{
		{path: filepath.Join(s.Root, "rendered", "ssh", "50-hmux.generated.conf"), data: sshData, mode: 0o600},
		{path: filepath.Join(s.Root, "rendered", "termius", "hmux-hosts.csv"), data: csvData, mode: 0o600},
		{path: filepath.Join(s.Root, "state", "reconcile.json"), data: statusData, mode: 0o600},
	})
}

func (s Store) Rendered(kind string, output io.Writer) error {
	var path string
	switch kind {
	case "ssh":
		path = filepath.Join(s.Root, "rendered", "ssh", "50-hmux.generated.conf")
	case "termius":
		path = filepath.Join(s.Root, "rendered", "termius", "hmux-hosts.csv")
	case "inventory":
		path = s.InventoryPath()
	default:
		return errors.New("rendered kind must be ssh, termius or inventory")
	}
	data, err := readOwnedRegularFile(path, 16*1024*1024)
	if err != nil {
		return err
	}
	_, err = output.Write(data)
	return err
}

func readOwnedRegularFile(path string, maximum int64) ([]byte, error) {
	linkInfo, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if linkInfo.Mode()&os.ModeSymlink != 0 || !linkInfo.Mode().IsRegular() ||
		linkInfo.Mode().Perm()&0o022 != 0 || linkInfo.Size() < 1 ||
		linkInfo.Size() > maximum {
		return nil, errors.New("served file must be a bounded non-writable regular file, not a symlink")
	}
	if stat, ok := linkInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return nil, errors.New("served file must be owned by the current user")
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !os.SameFile(linkInfo, info) || !info.Mode().IsRegular() ||
		info.Mode().Perm()&0o022 != 0 || info.Size() != linkInfo.Size() {
		return nil, errors.New("served file changed while it was being opened")
	}
	data, err := io.ReadAll(io.LimitReader(file, maximum+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) != info.Size() {
		return nil, errors.New("served file changed while it was being read")
	}
	return data, nil
}

func (s Store) Manifest(platform, version string) (model.Manifest, error) {
	if err := safePlatform(platform); err != nil {
		return model.Manifest{}, err
	}
	if version == "" {
		target, err := os.Readlink(filepath.Join(s.Root, "current"))
		if err != nil {
			return model.Manifest{}, fmt.Errorf("current release: %w", err)
		}
		version = filepath.Base(target)
		if target != filepath.Join("releases", version) {
			return model.Manifest{}, errors.New("current release link has an invalid target")
		}
	}
	if err := safeVersion(version); err != nil {
		return model.Manifest{}, err
	}
	path := filepath.Join(s.Root, "releases", version, "manifest-"+platform+".json")
	info, err := os.Lstat(path)
	if err != nil {
		return model.Manifest{}, err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() ||
		info.Mode().Perm()&0o022 != 0 || info.Size() < 1 || info.Size() > 1024*1024 {
		return model.Manifest{}, errors.New("manifest must be a small non-writable regular file, not a symlink")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return model.Manifest{}, errors.New("manifest must be owned by the current user")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return model.Manifest{}, err
	}
	var manifest model.Manifest
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&manifest); err != nil {
		return manifest, err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		if err == nil {
			return manifest, errors.New("manifest contains trailing JSON")
		}
		return manifest, err
	}
	if manifest.Platform != platform || manifest.Version != version {
		return manifest, errors.New("manifest path/content mismatch")
	}
	if err := release.ValidateManifestForPlatform(manifest, platform); err != nil {
		return manifest, fmt.Errorf("invalid manifest: %w", err)
	}
	return manifest, nil
}

func (s Store) Artifact(platform, version string, output io.Writer) error {
	manifest, err := s.Manifest(platform, version)
	if err != nil {
		return err
	}
	path := filepath.Join(s.Root, "releases", manifest.Version, platform, manifest.Artifact)
	linkInfo, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if linkInfo.Mode()&os.ModeSymlink != 0 || !linkInfo.Mode().IsRegular() ||
		linkInfo.Mode().Perm()&0o022 != 0 || linkInfo.Size() != manifest.Size {
		return errors.New("artifact must be a non-writable regular file, not a symlink")
	}
	if stat, ok := linkInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("artifact must be owned by the current user")
	}
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	if !os.SameFile(linkInfo, info) || !info.Mode().IsRegular() ||
		info.Mode().Perm()&0o022 != 0 || info.Size() != manifest.Size {
		return errors.New("artifact changed while it was being opened")
	}
	_, err = io.Copy(output, file)
	return err
}

func (s Store) Publish(version string, artifacts []string, privateKeyPath string) error {
	if err := safeVersion(version); err != nil {
		return err
	}
	if len(artifacts) == 0 {
		return errors.New("at least one --artifact platform=path is required")
	}
	var privateKey ed25519.PrivateKey
	var keyID string
	if privateKeyPath != "" {
		var err error
		privateKey, keyID, err = loadPrivateKey(privateKeyPath)
		if err != nil {
			return err
		}
	}
	return s.withLock(func() error {
		releaseDir := filepath.Join(s.Root, "releases", version)
		if _, err := os.Lstat(releaseDir); err == nil {
			return fmt.Errorf("release %q already exists", version)
		}
		stageDir := filepath.Join(s.Root, "releases", fmt.Sprintf(".%s-stage-%d", version, os.Getpid()))
		_ = os.RemoveAll(stageDir)
		defer os.RemoveAll(stageDir)
		seenPlatforms := map[string]bool{}
		for _, item := range artifacts {
			platform, source, ok := strings.Cut(item, "=")
			if !ok {
				return fmt.Errorf("invalid artifact %q; expected platform=path", item)
			}
			if err := safePlatform(platform); err != nil {
				return err
			}
			if seenPlatforms[platform] {
				return fmt.Errorf("duplicate artifact platform %q", platform)
			}
			seenPlatforms[platform] = true
			data, err := os.ReadFile(source)
			if err != nil {
				return err
			}
			if len(data) == 0 || len(data) > 256*1024*1024 {
				return fmt.Errorf("artifact %s has invalid size", platform)
			}
			sum := sha256.Sum256(data)
			manifest := model.Manifest{
				SchemaVersion: model.SchemaVersion, Version: version, Platform: platform,
				Artifact: "hmux", SHA256: hex.EncodeToString(sum[:]), Size: int64(len(data)),
				PublishedAt: time.Now().UTC(), MinProtocol: model.ProtocolVersion, KeyID: keyID,
			}
			if privateKey != nil {
				manifest.SignatureType = release.SignatureType
				payload, err := release.SignaturePayload(manifest)
				if err != nil {
					return err
				}
				// Signature is retained for a safe upgrade from 0.1.2 clients,
				// which verify the artifact bytes directly. New clients require
				// ManifestSignature, which also binds all release metadata.
				manifest.Signature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, data))
				manifest.ManifestSignature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, payload))
			}
			platformDir := filepath.Join(stageDir, platform)
			if err := config.AtomicWrite(filepath.Join(platformDir, "hmux"), data, 0o700); err != nil {
				return err
			}
			manifestData, _ := json.MarshalIndent(manifest, "", "  ")
			if err := config.AtomicWrite(filepath.Join(stageDir, "manifest-"+platform+".json"), manifestData, 0o600); err != nil {
				return err
			}
		}
		if err := os.Rename(stageDir, releaseDir); err != nil {
			return err
		}
		temp := filepath.Join(s.Root, fmt.Sprintf(".current-%d", os.Getpid()))
		_ = os.Remove(temp)
		if err := os.Symlink(filepath.Join("releases", version), temp); err != nil {
			return err
		}
		if err := os.Rename(temp, filepath.Join(s.Root, "current")); err != nil {
			_ = os.Remove(temp)
			return err
		}
		return nil
	})
}

func (s Store) Rollback(version string) error {
	if err := safeVersion(version); err != nil {
		return err
	}
	if err := s.validateRelease(version); err != nil {
		return err
	}
	return s.withLock(func() error {
		temp := filepath.Join(s.Root, fmt.Sprintf(".current-%d", os.Getpid()))
		_ = os.Remove(temp)
		if err := os.Symlink(filepath.Join("releases", version), temp); err != nil {
			return err
		}
		if err := os.Rename(temp, filepath.Join(s.Root, "current")); err != nil {
			_ = os.Remove(temp)
			return err
		}
		return nil
	})
}

func (s Store) validateRelease(version string) error {
	releaseDir := filepath.Join(s.Root, "releases", version)
	info, err := os.Lstat(releaseDir)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return fmt.Errorf("release %q is unavailable", version)
	}
	entries, err := os.ReadDir(releaseDir)
	if err != nil {
		return err
	}
	manifests := 0
	for _, entry := range entries {
		name := entry.Name()
		if !strings.HasPrefix(name, "manifest-") || !strings.HasSuffix(name, ".json") {
			continue
		}
		entryInfo, err := entry.Info()
		if err != nil || entry.Type()&os.ModeSymlink != 0 || !entryInfo.Mode().IsRegular() {
			return fmt.Errorf("release %q manifest is not a regular file", version)
		}
		platform := strings.TrimSuffix(strings.TrimPrefix(name, "manifest-"), ".json")
		if safePlatform(platform) != nil {
			return fmt.Errorf("release %q has invalid manifest name", version)
		}
		manifest, err := s.Manifest(platform, version)
		if err != nil {
			return err
		}
		if manifest.SchemaVersion != model.SchemaVersion || manifest.Artifact != "hmux" ||
			manifest.Size < 1 || manifest.Size > 256*1024*1024 || manifest.MinProtocol < 1 {
			return fmt.Errorf("release %q has invalid manifest metadata", version)
		}
		expectedSHA, err := hex.DecodeString(manifest.SHA256)
		if err != nil || len(expectedSHA) != sha256.Size {
			return fmt.Errorf("release %q has invalid checksum", version)
		}
		if manifest.SignatureType != "" {
			legacy, legacyErr := base64.StdEncoding.DecodeString(manifest.Signature)
			metadata, metadataErr := base64.StdEncoding.DecodeString(manifest.ManifestSignature)
			if manifest.SignatureType != release.SignatureType || legacyErr != nil || metadataErr != nil ||
				len(legacy) != ed25519.SignatureSize || len(metadata) != ed25519.SignatureSize ||
				len(manifest.KeyID) != 16 {
				return fmt.Errorf("release %q has invalid signature metadata", version)
			}
		}
		hasher := sha256.New()
		if err := s.Artifact(platform, version, hasher); err != nil {
			return fmt.Errorf("release %q artifact is invalid: %w", version, err)
		}
		if !bytes.Equal(hasher.Sum(nil), expectedSHA) {
			return fmt.Errorf("release %q artifact checksum mismatch", version)
		}
		manifests++
	}
	if manifests == 0 {
		return fmt.Errorf("release %q has no manifests", version)
	}
	return nil
}

func (s Store) Health(ctx context.Context) map[string]any {
	result := map[string]any{"schema_version": model.SchemaVersion, "ok": true, "checked_at": time.Now().UTC()}
	if inventory, err := s.Validate(); err != nil {
		result["ok"] = false
		result["inventory_error"] = err.Error()
	} else {
		result["inventory_revision"] = inventory.Revision
		result["hosts"] = len(inventory.Hosts)
		result["profiles"] = len(inventory.Profiles)
	}
	target, err := os.Readlink(filepath.Join(s.Root, "current"))
	if err != nil {
		result["ok"] = false
		result["release_error"] = err.Error()
	} else {
		currentVersion := filepath.Base(target)
		if safeVersion(currentVersion) != nil || target != filepath.Join("releases", currentVersion) {
			result["ok"] = false
			result["release_error"] = "current release link has an invalid target"
		} else if validateErr := s.validateRelease(currentVersion); validateErr != nil {
			result["ok"] = false
			result["release_error"] = validateErr.Error()
		} else {
			result["current_release"] = currentVersion
		}
	}
	var releases []string
	if entries, readErr := os.ReadDir(filepath.Join(s.Root, "releases")); readErr == nil {
		for _, entry := range entries {
			if entry.IsDir() && safeVersion(entry.Name()) == nil {
				releases = append(releases, entry.Name())
			}
		}
		sort.Strings(releases)
	}
	result["available_releases"] = releases
	result["minimum_release_retention_met"] = len(releases) >= 3
	if data, err := os.ReadFile(filepath.Join(s.Root, "state", "reconcile.json")); err == nil {
		var reconcile any
		_ = json.Unmarshal(data, &reconcile)
		result["reconcile"] = reconcile
	} else {
		result["ok"] = false
		result["reconcile_error"] = err.Error()
	}
	if systemctl, lookupErr := exec.LookPath("systemctl"); lookupErr == nil {
		units := map[string]string{}
		for _, unit := range []string{"hmux-reconcile.timer", "hmux-health.timer"} {
			output, statusErr := exec.CommandContext(ctx, systemctl, "--user", "is-active", unit).Output()
			status := strings.TrimSpace(string(output))
			if status == "" {
				status = "unknown"
			}
			units[unit] = status
			if statusErr != nil || status != "active" {
				result["ok"] = false
			}
		}
		result["systemd_units"] = units
	} else {
		result["systemd_units"] = "systemctl unavailable"
	}
	return result
}

func (s Store) HostList() ([]model.Host, error) {
	inventory, err := s.Validate()
	if err != nil {
		return nil, err
	}
	sort.Slice(inventory.Hosts, func(i, j int) bool { return inventory.Hosts[i].ID < inventory.Hosts[j].ID })
	return inventory.Hosts, nil
}

func (s Store) HostDiff() (map[string]any, error) {
	inventory, err := s.Validate()
	if err != nil {
		return nil, err
	}
	expectedSSH, err := sshconfig.Render(inventory)
	if err != nil {
		return nil, err
	}
	expectedTermius, err := renderTermiusCSV(inventory)
	if err != nil {
		return nil, err
	}
	actualSSH, sshErr := os.ReadFile(filepath.Join(s.Root, "rendered", "ssh", "50-hmux.generated.conf"))
	actualTermius, termiusErr := os.ReadFile(filepath.Join(s.Root, "rendered", "termius", "hmux-hosts.csv"))
	sshPending := sshErr != nil || !bytes.Equal(actualSSH, expectedSSH)
	termiusPending := termiusErr != nil || !bytes.Equal(actualTermius, expectedTermius)
	return map[string]any{
		"revision": inventory.Revision, "pending": sshPending || termiusPending,
		"ssh_pending": sshPending, "termius_artifact_pending": termiusPending,
		"source_of_truth": "dmz-inventory",
	}, nil
}

func (s Store) HostUpsert(host model.Host, edit, dryRun bool) error {
	return s.withLock(func() error {
		inventory, err := s.Validate()
		if err != nil {
			return err
		}
		index := -1
		for i := range inventory.Hosts {
			if inventory.Hosts[i].ID == host.ID {
				index = i
				break
			}
		}
		if edit && index < 0 {
			return fmt.Errorf("host %q does not exist", host.ID)
		}
		if !edit && index >= 0 {
			return fmt.Errorf("host %q already exists", host.ID)
		}
		if index >= 0 {
			inventory.Hosts[index] = host
		} else {
			inventory.Hosts = append(inventory.Hosts, host)
		}
		if err := inventory.Validate(); err != nil {
			return err
		}
		if dryRun {
			return nil
		}
		oldData, err := os.ReadFile(s.InventoryPath())
		if err != nil {
			return err
		}
		if err := s.snapshotInventory(); err != nil {
			return err
		}
		inventory.Revision = time.Now().UTC().Format("20060102T150405.000000000Z")
		if err := config.SaveInventory(s.InventoryPath(), inventory); err != nil {
			return err
		}
		if err := s.reconcileLocked(inventory); err != nil {
			if restoreErr := config.AtomicWrite(s.InventoryPath(), oldData, 0o600); restoreErr != nil {
				return fmt.Errorf("reconcile failed: %v; inventory restore failed: %w", err, restoreErr)
			}
			return fmt.Errorf("reconcile failed; inventory restored: %w", err)
		}
		return nil
	})
}

func (s Store) IdentitySetPath(id, path string) error {
	return s.withLock(func() error {
		inventory, err := s.Validate()
		if err != nil {
			return err
		}
		found := false
		for index := range inventory.IdentityRefs {
			if inventory.IdentityRefs[index].ID == id {
				inventory.IdentityRefs[index].Path = path
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("identity_ref %q does not exist", id)
		}
		if err := inventory.Validate(); err != nil {
			return err
		}
		oldData, err := os.ReadFile(s.InventoryPath())
		if err != nil {
			return err
		}
		if err := s.snapshotInventory(); err != nil {
			return err
		}
		inventory.Revision = time.Now().UTC().Format("20060102T150405.000000000Z")
		if err := config.SaveInventory(s.InventoryPath(), inventory); err != nil {
			return err
		}
		if err := s.reconcileLocked(inventory); err != nil {
			if restoreErr := config.AtomicWrite(s.InventoryPath(), oldData, 0o600); restoreErr != nil {
				return fmt.Errorf("reconcile failed: %v; inventory restore failed: %w", err, restoreErr)
			}
			return fmt.Errorf("reconcile failed; inventory restored: %w", err)
		}
		return nil
	})
}

func (s Store) AuthorizeStagedClientKey() error {
	return s.withLock(func() error {
		incoming := filepath.Join(s.Root, "state", "client-key.pub.incoming")
		info, err := os.Lstat(incoming)
		if err != nil {
			return fmt.Errorf("staged client key: %w", err)
		}
		if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 || info.Size() < 1 || info.Size() > 4096 {
			return errors.New("staged client key must be a regular file no larger than 4096 bytes")
		}
		data, err := os.ReadFile(incoming)
		if err != nil {
			return err
		}
		fields := strings.Fields(string(data))
		if len(fields) < 2 || len(fields) > 3 || fields[0] != "ssh-ed25519" {
			return errors.New("staged client key must contain one ssh-ed25519 public key")
		}
		rawKey, err := base64.StdEncoding.DecodeString(fields[1])
		if err != nil || !validOpenSSHEd25519Blob(rawKey) {
			return errors.New("staged client key has invalid Ed25519 data")
		}
		home, err := os.UserHomeDir()
		if err != nil {
			return err
		}
		sshDir := filepath.Join(home, ".ssh")
		if err := os.MkdirAll(sshDir, 0o700); err != nil {
			return err
		}
		sshInfo, err := os.Lstat(sshDir)
		if err != nil || sshInfo.Mode()&os.ModeSymlink != 0 || !sshInfo.IsDir() {
			return errors.New("SSH directory must be a real directory, not a symlink")
		}
		if err := os.Chmod(sshDir, 0o700); err != nil {
			return err
		}
		authorizedPath := filepath.Join(sshDir, "authorized_keys")
		if authorizedInfo, statErr := os.Lstat(authorizedPath); statErr == nil {
			if authorizedInfo.Mode()&os.ModeSymlink != 0 || !authorizedInfo.Mode().IsRegular() {
				return errors.New("authorized_keys must be a regular non-symlink file")
			}
			if authorizedInfo.Mode().Perm()&0o022 != 0 {
				return errors.New("authorized_keys must not be group/world writable")
			}
			if stat, ok := authorizedInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
				return errors.New("authorized_keys must be owned by the current user")
			}
		} else if !errors.Is(statErr, os.ErrNotExist) {
			return statErr
		}
		existing, err := os.ReadFile(authorizedPath)
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		for _, line := range strings.Split(string(existing), "\n") {
			current := strings.Fields(line)
			for index := 0; index+1 < len(current); index++ {
				if current[index] == "ssh-ed25519" && current[index+1] == fields[1] {
					return os.Remove(incoming)
				}
			}
		}
		if len(existing) > 0 {
			if _, err := config.Backup(authorizedPath, time.Now()); err != nil {
				return err
			}
		}
		updated := append([]byte(nil), existing...)
		if len(updated) > 0 && updated[len(updated)-1] != '\n' {
			updated = append(updated, '\n')
		}
		updated = append(updated, []byte("ssh-ed25519 "+fields[1]+" hmux-external-client\n")...)
		if err := config.AtomicWrite(authorizedPath, updated, 0o600); err != nil {
			return err
		}
		return os.Remove(incoming)
	})
}

func validOpenSSHEd25519Blob(data []byte) bool {
	readField := func() ([]byte, bool) {
		if len(data) < 4 {
			return nil, false
		}
		size := uint64(binary.BigEndian.Uint32(data[:4]))
		data = data[4:]
		if size > uint64(len(data)) {
			return nil, false
		}
		field := data[:size]
		data = data[size:]
		return field, true
	}
	algorithm, ok := readField()
	if !ok || string(algorithm) != "ssh-ed25519" {
		return false
	}
	publicKey, ok := readField()
	return ok && len(publicKey) == ed25519.PublicKeySize && len(data) == 0
}

func GenerateKeyPair(privatePath, publicPath string) error {
	for _, path := range []string{privatePath, publicPath} {
		if _, err := os.Lstat(path); err == nil {
			return fmt.Errorf("refusing to overwrite existing key file %q", filepath.Base(path))
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return err
	}
	privateDER, err := x509.MarshalPKCS8PrivateKey(privateKey)
	if err != nil {
		return err
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		return err
	}
	if err := config.AtomicWrite(privatePath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: privateDER}), 0o600); err != nil {
		return err
	}
	if err := config.AtomicWrite(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o644); err != nil {
		_ = os.Remove(privatePath)
		return err
	}
	return nil
}

func (s Store) snapshotInventory() error {
	data, err := os.ReadFile(s.InventoryPath())
	if err != nil {
		return err
	}
	name := time.Now().UTC().Format("20060102T150405.000000000Z") + ".toml"
	return config.AtomicWrite(filepath.Join(s.Root, "history", name), data, 0o600)
}

func (s Store) withLock(action func() error) error {
	if err := os.MkdirAll(filepath.Join(s.Root, "state"), 0o700); err != nil {
		return err
	}
	file, err := os.OpenFile(filepath.Join(s.Root, "state", "control.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	if err := syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return errors.New("another hmux-control operation is running")
	}
	defer syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
	return action()
}

type fileUpdate struct {
	path string
	data []byte
	mode os.FileMode
}

type previousFile struct {
	path   string
	data   []byte
	mode   os.FileMode
	exists bool
}

func atomicWriteSet(updates []fileUpdate) error {
	previous := make([]previousFile, 0, len(updates))
	for _, update := range updates {
		old := previousFile{path: update.path}
		if info, err := os.Stat(update.path); err == nil {
			old.exists = true
			old.mode = info.Mode().Perm()
			old.data, err = os.ReadFile(update.path)
			if err != nil {
				return errors.Join(err, restorePreviousFiles(previous))
			}
		} else if !errors.Is(err, os.ErrNotExist) {
			return errors.Join(err, restorePreviousFiles(previous))
		}
		previous = append(previous, old)
		if err := config.AtomicWrite(update.path, update.data, update.mode); err != nil {
			return errors.Join(err, restorePreviousFiles(previous))
		}
	}
	return nil
}

func restorePreviousFiles(previous []previousFile) error {
	var result error
	for index := len(previous) - 1; index >= 0; index-- {
		item := previous[index]
		if item.exists {
			result = errors.Join(result, config.AtomicWrite(item.path, item.data, item.mode))
		} else {
			if err := os.Remove(item.path); err != nil && !errors.Is(err, os.ErrNotExist) {
				result = errors.Join(result, err)
			}
		}
	}
	return result
}

func renderTermiusCSV(inventory model.Inventory) ([]byte, error) {
	var out bytes.Buffer
	writer := csv.NewWriter(&out)
	if err := writer.Write([]string{"Label", "Address", "Port", "Username", "Group", "Tags", "JumpHost"}); err != nil {
		return nil, err
	}
	hostByID := map[string]model.Host{}
	for _, host := range inventory.Hosts {
		hostByID[host.ID] = host
	}
	for _, host := range inventory.Hosts {
		jump := ""
		if host.ProxyJump != "" {
			jump = hostByID[host.ProxyJump].SSHAlias
		}
		if err := writer.Write([]string{host.SSHAlias, host.Address, fmt.Sprint(host.Port), host.User, "hmux", strings.Join(host.Tags, ";"), jump}); err != nil {
			return nil, err
		}
	}
	writer.Flush()
	return out.Bytes(), writer.Error()
}

func loadPrivateKey(path string) (ed25519.PrivateKey, string, error) {
	linkInfo, err := os.Lstat(path)
	if err != nil {
		return nil, "", err
	}
	if linkInfo.Mode()&os.ModeSymlink != 0 || !linkInfo.Mode().IsRegular() {
		return nil, "", errors.New("private signing key must be a regular non-symlink file")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, "", err
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, "", err
	}
	if info.Mode().Perm()&0o077 != 0 {
		return nil, "", errors.New("private signing key must not be group/world accessible")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return nil, "", errors.New("private signing key must be owned by the current user")
	}
	block, _ := pem.Decode(data)
	if block == nil {
		return nil, "", errors.New("invalid private key PEM")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, "", err
	}
	key, ok := parsed.(ed25519.PrivateKey)
	if !ok {
		return nil, "", errors.New("signing key is not Ed25519")
	}
	publicDER, _ := x509.MarshalPKIXPublicKey(key.Public())
	sum := sha256.Sum256(publicDER)
	return key, hex.EncodeToString(sum[:8]), nil
}

func safeVersion(value string) error {
	if !release.ValidVersion(value) {
		return errors.New("invalid release version")
	}
	return nil
}

func safePlatform(value string) error {
	switch value {
	case "darwin-arm64", "darwin-amd64", "linux-amd64", "linux-arm64",
		"darwin-arm64-agent", "darwin-amd64-agent", "darwin-arm64-app":
		return nil
	default:
		return fmt.Errorf("unsupported platform %q", value)
	}
}

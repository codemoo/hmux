package release

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

const SignatureType = "ed25519-manifest-v1"

var versionPattern = regexp.MustCompile(`^[0-9]+(?:\.[0-9]+){1,3}(?:[-+][A-Za-z0-9][A-Za-z0-9.-]{0,31})?$`)
var artifactPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`)
var keyIDPattern = regexp.MustCompile(`^[a-f0-9]{16}$`)

const cachedManifestLimit = 1024 * 1024

func Platform() string {
	return runtime.GOOS + "-" + runtime.GOARCH
}

func AgentPlatform() string {
	return Platform() + "-agent"
}

func AppPlatform() string {
	return Platform() + "-app"
}

func FetchManifest(ctx context.Context, cfg config.ClientConfig) (model.Manifest, error) {
	return FetchManifestForPlatform(ctx, cfg, Platform())
}

func FetchManifestForPlatform(ctx context.Context, cfg config.ClientConfig, platform string) (model.Manifest, error) {
	var manifest model.Manifest
	if !safeAlias(cfg.DMZAlias) || !safeRemotePath(cfg.ControlPath) {
		return manifest, errors.New("dmz_alias or control_path contains unsupported characters")
	}
	if platform != Platform() && platform != AgentPlatform() && platform != AppPlatform() {
		return manifest, errors.New("unsupported release platform")
	}
	cmd := exec.CommandContext(ctx, "ssh", "-o", "BatchMode=yes", cfg.DMZAlias, "--",
		cfg.ControlPath, "manifest", "--platform", platform)
	output, err := safeexec.Output(cmd, 1024*1024)
	if err != nil {
		return manifest, fmt.Errorf("fetch manifest: %w", err)
	}
	if err := decodeManifest(output, &manifest); err != nil {
		return manifest, fmt.Errorf("decode manifest: %w", err)
	}
	if err := ValidateManifestForPlatform(manifest, platform); err != nil {
		return manifest, err
	}
	return manifest, nil
}

func FetchArtifact(ctx context.Context, cfg config.ClientConfig, manifest model.Manifest) ([]byte, error) {
	return FetchArtifactForPlatform(ctx, cfg, manifest, Platform())
}

func FetchArtifactForPlatform(ctx context.Context, cfg config.ClientConfig, manifest model.Manifest, platform string) ([]byte, error) {
	if !safeAlias(cfg.DMZAlias) || !safeRemotePath(cfg.ControlPath) {
		return nil, errors.New("dmz_alias or control_path contains unsupported characters")
	}
	if platform != Platform() && platform != AgentPlatform() && platform != AppPlatform() {
		return nil, errors.New("unsupported release platform")
	}
	if err := ValidateManifestForPlatform(manifest, platform); err != nil {
		return nil, err
	}
	cmd := exec.CommandContext(ctx, "ssh", "-o", "BatchMode=yes", cfg.DMZAlias, "--",
		cfg.ControlPath, "artifact", "--platform", manifest.Platform, "--version", manifest.Version)
	output, err := safeexec.Output(cmd, manifest.Size)
	if err != nil {
		return nil, fmt.Errorf("fetch artifact: %w", err)
	}
	if err := VerifyArtifactForPlatform(output, manifest, cfg.PublicKeyPath, platform); err != nil {
		return nil, err
	}
	return output, nil
}

func Install(cfg config.ClientConfig, manifest model.Manifest, artifact []byte) (string, error) {
	return installValidated(cfg, manifest, artifact, nil)
}

// InstallValidated caches a signed immutable release and activates it only
// after the caller's compatibility check succeeds. The update lock makes both
// compatibility and anti-downgrade checks authoritative at activation time.
func InstallValidated(
	cfg config.ClientConfig,
	manifest model.Manifest,
	artifact []byte,
	validate func(string) error,
) (string, error) {
	if validate == nil {
		return "", errors.New("release validator is required")
	}
	return installValidated(cfg, manifest, artifact, validate)
}

func installValidated(
	cfg config.ClientConfig,
	manifest model.Manifest,
	artifact []byte,
	validate func(string) error,
) (string, error) {
	if err := ValidateManifest(manifest); err != nil {
		return "", err
	}
	if err := VerifyArtifact(artifact, manifest, cfg.PublicKeyPath); err != nil {
		return "", err
	}
	base := filepath.Clean(cfg.CacheDir)
	if base == "." || base == string(os.PathSeparator) {
		return "", errors.New("unsafe cache directory")
	}
	var binaryPath string
	err := withUpdateLock(base, func() error {
		releasesDir := filepath.Join(base, "releases")
		if err := ensurePrivateRealDir(releasesDir); err != nil {
			return err
		}
		releaseDir := filepath.Join(base, "releases", manifest.Version)
		if _, err := os.Lstat(releaseDir); err == nil {
			cachedManifest, cachedArtifact, verifyErr := readVerifiedCached(cfg, manifest.Version)
			if verifyErr != nil {
				return fmt.Errorf("existing immutable release is invalid: %w", verifyErr)
			}
			requestedManifest, marshalErr := json.Marshal(manifest)
			if marshalErr != nil {
				return marshalErr
			}
			existingManifest, marshalErr := json.Marshal(cachedManifest)
			if marshalErr != nil {
				return marshalErr
			}
			if !bytes.Equal(existingManifest, requestedManifest) || !bytes.Equal(cachedArtifact, artifact) {
				return fmt.Errorf("cached release %q is immutable and differs from the requested artifact", manifest.Version)
			}
			binaryPath = filepath.Join(releaseDir, "hmux")
			if validate != nil {
				if err := validate(binaryPath); err != nil {
					return fmt.Errorf("validate cached release compatibility: %w", err)
				}
			}
			if err := requireForwardActivationUnlocked(cfg, manifest.Version); err != nil {
				return err
			}
			return selectCurrent(base, manifest.Version)
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		}
		stageDir := filepath.Join(releasesDir, fmt.Sprintf(".%s-stage-%d", manifest.Version, os.Getpid()))
		_ = os.RemoveAll(stageDir)
		if err := os.Mkdir(stageDir, 0o700); err != nil {
			return err
		}
		defer os.RemoveAll(stageDir)
		stagedBinary := filepath.Join(stageDir, "hmux")
		if err := config.AtomicWrite(stagedBinary, artifact, 0o700); err != nil {
			return err
		}
		manifestData, err := json.MarshalIndent(manifest, "", "  ")
		if err != nil {
			return err
		}
		if err := config.AtomicWrite(filepath.Join(stageDir, "manifest.json"), manifestData, 0o600); err != nil {
			return err
		}
		if err := os.Rename(stageDir, releaseDir); err != nil {
			return err
		}
		binaryPath = filepath.Join(releaseDir, "hmux")
		if validate != nil {
			if err := validate(binaryPath); err != nil {
				return fmt.Errorf("validate staged release compatibility: %w", err)
			}
		}
		if err := requireForwardActivationUnlocked(cfg, manifest.Version); err != nil {
			return err
		}
		return selectCurrent(base, manifest.Version)
	})
	return binaryPath, err
}

func requireForwardActivationUnlocked(cfg config.ClientConfig, candidate string) error {
	selected, err := selectedVersionUnlocked(cfg)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("verify currently selected release before activation: %w", err)
	}
	if selected == candidate {
		return nil
	}
	if !IsNewerVersion(candidate, selected) {
		return fmt.Errorf("refusing release rollback from %s to %s; use explicit rollback", selected, candidate)
	}
	return nil
}

func VerifyArtifact(data []byte, manifest model.Manifest, publicKeyPath string) error {
	return VerifyArtifactForPlatform(data, manifest, publicKeyPath, Platform())
}

func VerifyArtifactForPlatform(data []byte, manifest model.Manifest, publicKeyPath, platform string) error {
	if err := ValidateManifestForPlatform(manifest, platform); err != nil {
		return err
	}
	if int64(len(data)) != manifest.Size {
		return fmt.Errorf("artifact size mismatch: expected %d, got %d", manifest.Size, len(data))
	}
	sum := sha256.Sum256(data)
	actual := hex.EncodeToString(sum[:])
	if actual != strings.ToLower(manifest.SHA256) {
		return fmt.Errorf("artifact sha256 mismatch")
	}
	if publicKeyPath == "" && manifest.ManifestSignature == "" {
		return nil
	}
	if manifest.ManifestSignature == "" {
		return errors.New("pinned release key requires a signed manifest")
	}
	if manifest.SignatureType != SignatureType {
		return fmt.Errorf("unsupported release signature type %q", manifest.SignatureType)
	}
	if publicKeyPath == "" {
		return errors.New("signed release requires a pinned public key path")
	}
	keyInfo, err := os.Lstat(publicKeyPath)
	if err != nil {
		return fmt.Errorf("signed release requires pinned public key: %w", err)
	}
	if keyInfo.Mode()&os.ModeSymlink != 0 || !keyInfo.Mode().IsRegular() || keyInfo.Mode().Perm()&0o022 != 0 {
		return errors.New("pinned public key must be a non-writable regular file, not a symlink")
	}
	if stat, ok := keyInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("pinned public key must be owned by the current user")
	}
	blockData, err := os.ReadFile(publicKeyPath)
	if err != nil {
		return fmt.Errorf("signed release requires pinned public key: %w", err)
	}
	block, _ := pem.Decode(blockData)
	if block == nil {
		return errors.New("invalid public key PEM")
	}
	parsed, err := x509.ParsePKIXPublicKey(block.Bytes)
	if err != nil {
		return fmt.Errorf("parse public key: %w", err)
	}
	publicKey, ok := parsed.(ed25519.PublicKey)
	if !ok {
		return errors.New("release public key is not Ed25519")
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		return err
	}
	keySum := sha256.Sum256(publicDER)
	expectedKeyID := hex.EncodeToString(keySum[:8])
	if manifest.KeyID != expectedKeyID {
		return errors.New("release signing key id does not match pinned public key")
	}
	signature, err := base64.StdEncoding.DecodeString(manifest.ManifestSignature)
	if err != nil || len(signature) != ed25519.SignatureSize {
		return errors.New("invalid release signature")
	}
	payload, err := SignaturePayload(manifest)
	if err != nil {
		return err
	}
	if !ed25519.Verify(publicKey, payload, signature) {
		return errors.New("release signature verification failed")
	}
	return nil
}

func ValidateManifest(manifest model.Manifest) error {
	return ValidateManifestForPlatform(manifest, Platform())
}

func ValidateManifestForPlatform(manifest model.Manifest, platform string) error {
	if manifest.SchemaVersion != model.SchemaVersion {
		return fmt.Errorf("unsupported manifest schema_version %d", manifest.SchemaVersion)
	}
	if !ValidVersion(manifest.Version) {
		return errors.New("invalid manifest version")
	}
	if manifest.Platform != platform {
		return fmt.Errorf("manifest platform %q does not match %q", manifest.Platform, platform)
	}
	if !artifactPattern.MatchString(manifest.Artifact) || filepath.Base(manifest.Artifact) != manifest.Artifact {
		return errors.New("invalid artifact name")
	}
	if len(manifest.SHA256) != 64 {
		return errors.New("invalid artifact sha256")
	}
	if _, err := hex.DecodeString(manifest.SHA256); err != nil {
		return errors.New("invalid artifact sha256")
	}
	if manifest.Size < 1 || manifest.Size > 256*1024*1024 {
		return errors.New("invalid artifact size")
	}
	if manifest.PublishedAt.IsZero() {
		return errors.New("manifest published_at is required")
	}
	if manifest.MinProtocol < 1 {
		return errors.New("manifest min_protocol must be positive")
	}
	if manifest.MinProtocol > model.ProtocolVersion {
		return fmt.Errorf("release requires protocol %d, client supports %d", manifest.MinProtocol, model.ProtocolVersion)
	}
	if manifest.KeyID != "" && !keyIDPattern.MatchString(manifest.KeyID) {
		return errors.New("invalid release key id")
	}
	return nil
}

func ValidVersion(value string) bool {
	return versionPattern.MatchString(value)
}

func SignaturePayload(manifest model.Manifest) ([]byte, error) {
	type envelope struct {
		Domain        string `json:"domain"`
		SchemaVersion int    `json:"schema_version"`
		Version       string `json:"version"`
		Platform      string `json:"platform"`
		Artifact      string `json:"artifact"`
		SHA256        string `json:"sha256"`
		Size          int64  `json:"size"`
		PublishedAt   string `json:"published_at"`
		MinProtocol   int    `json:"min_protocol"`
		SignatureType string `json:"signature_type"`
		KeyID         string `json:"key_id"`
	}
	return json.Marshal(envelope{
		Domain: "hmux-release-manifest-v1", SchemaVersion: manifest.SchemaVersion,
		Version: manifest.Version, Platform: manifest.Platform, Artifact: manifest.Artifact,
		SHA256: strings.ToLower(manifest.SHA256), Size: manifest.Size,
		PublishedAt: manifest.PublishedAt.UTC().Format("2006-01-02T15:04:05.999999999Z07:00"),
		MinProtocol: manifest.MinProtocol, SignatureType: manifest.SignatureType, KeyID: manifest.KeyID,
	})
}

func Rollback(cfg config.ClientConfig, version string) error {
	if !ValidVersion(version) {
		return errors.New("invalid rollback version")
	}
	base := filepath.Clean(cfg.CacheDir)
	if base == "." || base == string(os.PathSeparator) {
		return errors.New("unsafe cache directory")
	}
	return withUpdateLock(base, func() error {
		if err := verifyCachedUnlocked(cfg, version); err != nil {
			return err
		}
		return selectCurrent(base, version)
	})
}

func VerifyCached(cfg config.ClientConfig, version string) error {
	if !ValidVersion(version) {
		return errors.New("invalid cached release version")
	}
	base := filepath.Clean(cfg.CacheDir)
	if base == "." || base == string(os.PathSeparator) {
		return errors.New("unsafe cache directory")
	}
	return withUpdateLock(base, func() error {
		return verifyCachedUnlocked(cfg, version)
	})
}

func verifyCachedUnlocked(cfg config.ClientConfig, version string) error {
	_, _, err := readVerifiedCached(cfg, version)
	return err
}

func readVerifiedCached(cfg config.ClientConfig, version string) (model.Manifest, []byte, error) {
	var manifest model.Manifest
	releaseDir := filepath.Join(filepath.Clean(cfg.CacheDir), "releases", version)
	info, err := os.Lstat(releaseDir)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return manifest, nil, fmt.Errorf("cached release %q is unavailable", version)
	}
	manifestPath := filepath.Join(releaseDir, "manifest.json")
	manifestInfo, err := os.Lstat(manifestPath)
	if err != nil || manifestInfo.Mode()&os.ModeSymlink != 0 || !manifestInfo.Mode().IsRegular() ||
		manifestInfo.Mode().Perm()&0o022 != 0 || manifestInfo.Size() < 1 ||
		manifestInfo.Size() > cachedManifestLimit {
		return manifest, nil, fmt.Errorf("cached release %q has no verifiable regular manifest", version)
	}
	if stat, ok := manifestInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return manifest, nil, errors.New("cached manifest must be owned by the current user")
	}
	manifestData, err := os.ReadFile(manifestPath)
	if err != nil {
		return manifest, nil, fmt.Errorf("cached release %q has no verifiable manifest", version)
	}
	if err := decodeManifest(manifestData, &manifest); err != nil {
		return manifest, nil, fmt.Errorf("decode cached manifest: %w", err)
	}
	if manifest.Version != version {
		return manifest, nil, errors.New("cached manifest version mismatch")
	}
	if err := ValidateManifest(manifest); err != nil {
		return manifest, nil, fmt.Errorf("validate cached manifest: %w", err)
	}
	artifactPath := filepath.Join(releaseDir, "hmux")
	artifactInfo, err := os.Lstat(artifactPath)
	if err != nil || artifactInfo.Mode()&os.ModeSymlink != 0 || !artifactInfo.Mode().IsRegular() ||
		artifactInfo.Mode().Perm()&0o022 != 0 || artifactInfo.Size() != manifest.Size {
		return manifest, nil, errors.New("cached artifact must be a non-writable regular file, not a symlink")
	}
	if stat, ok := artifactInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return manifest, nil, errors.New("cached artifact must be owned by the current user")
	}
	artifact, err := os.ReadFile(artifactPath)
	if err != nil {
		return manifest, nil, err
	}
	if err := VerifyArtifact(artifact, manifest, cfg.PublicKeyPath); err != nil {
		return manifest, nil, fmt.Errorf("verify cached release: %w", err)
	}
	return manifest, artifact, nil
}

func decodeManifest(data []byte, destination *model.Manifest) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("manifest contains trailing JSON")
		}
		return err
	}
	return nil
}

func withUpdateLock(base string, action func() error) error {
	if err := ensurePrivateRealDir(base); err != nil {
		return err
	}
	lock, err := os.OpenFile(filepath.Join(base, ".update.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(context.Background(), lock, 5*time.Second); err != nil {
		return fmt.Errorf("release update lock: %w", err)
	}
	defer filelock.Unlock(lock)
	return action()
}

func ensurePrivateRealDir(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return fmt.Errorf("%s must be a real directory, not a symlink", path)
	}
	if info.Mode().Perm()&0o022 != 0 {
		return fmt.Errorf("%s must not be group/world writable", path)
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return fmt.Errorf("%s must be owned by the current user", path)
	}
	return nil
}

func selectCurrent(base, version string) error {
	current := filepath.Join(base, "current")
	tempLink := filepath.Join(base, fmt.Sprintf(".current-%d", os.Getpid()))
	_ = os.Remove(tempLink)
	target := filepath.Join("releases", version, "hmux")
	if err := os.Symlink(target, tempLink); err != nil {
		return err
	}
	if err := os.Rename(tempLink, current); err != nil {
		_ = os.Remove(tempLink)
		return err
	}
	return nil
}

func safeRemotePath(value string) bool {
	if value == "" {
		return false
	}
	for _, r := range value {
		if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || strings.ContainsRune("~/_-.", r)) {
			return false
		}
	}
	return true
}

func safeAlias(value string) bool {
	if value == "" || len(value) > 128 || value[0] == '-' {
		return false
	}
	for _, r := range value {
		if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || strings.ContainsRune("._-", r)) {
			return false
		}
	}
	return true
}

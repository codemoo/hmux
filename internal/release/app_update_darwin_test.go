//go:build darwin

package release

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestLiveAppUpdateZipRoundTrip(t *testing.T) {
	source := os.Getenv("HMUX_TEST_APP_BUNDLE")
	if source == "" {
		t.Skip("set HMUX_TEST_APP_BUNDLE to exercise a built Release bundle")
	}
	root := t.TempDir()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	sourceVersion, err := plistValue(ctx, filepath.Join(source, "Contents", "Info.plist"), "CFBundleShortVersionString")
	if err != nil || !ValidVersion(sourceVersion) {
		t.Fatalf("source version=%q err=%v", sourceVersion, err)
	}
	parts := strings.Split(sourceVersion, ".")
	patch, err := strconv.Atoi(parts[len(parts)-1])
	if err != nil {
		t.Fatal(err)
	}
	parts[len(parts)-1] = strconv.Itoa(patch + 1)
	candidateVersion := strings.Join(parts, ".")
	target := filepath.Join(root, appBundleName)
	if output, err := exec.Command("/usr/bin/ditto", source, target).CombinedOutput(); err != nil {
		t.Fatalf("copy target: %v: %s", err, output)
	}
	candidateRoot := filepath.Join(t.TempDir(), "candidate")
	if err := os.Mkdir(candidateRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	candidate := filepath.Join(candidateRoot, appBundleName)
	if output, err := exec.Command("/usr/bin/ditto", source, candidate).CombinedOutput(); err != nil {
		t.Fatalf("copy candidate: %v: %s", err, output)
	}
	plist := filepath.Join(candidate, "Contents", "Info.plist")
	if output, err := exec.Command(
		"/usr/libexec/PlistBuddy", "-c", "Set :CFBundleShortVersionString "+candidateVersion, plist,
	).CombinedOutput(); err != nil {
		t.Fatalf("set candidate version: %v: %s", err, output)
	}
	if output, err := exec.Command("/usr/bin/codesign", "--force", "--deep", "--sign", "-", candidate).CombinedOutput(); err != nil {
		t.Fatalf("sign candidate: %v: %s", err, output)
	}
	archivePath := filepath.Join(t.TempDir(), "HMux.zip")
	if output, err := exec.Command(
		"/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", candidate, archivePath,
	).CombinedOutput(); err != nil {
		t.Fatalf("archive candidate: %v: %s", err, output)
	}
	artifact, err := os.ReadFile(archivePath)
	if err != nil {
		t.Fatal(err)
	}
	manifest, publicPath := signedAppManifest(t, candidateVersion, artifact)
	if err := installAppArtifact(
		ctx, target, sourceVersion, manifest, artifact, publicPath, time.Unix(1800000000, 0),
	); err != nil {
		t.Fatal(err)
	}
	if version, err := plistValue(ctx, filepath.Join(target, "Contents", "Info.plist"), "CFBundleShortVersionString"); err != nil || version != candidateVersion {
		t.Fatalf("installed version=%q err=%v", version, err)
	}
	backups, err := filepath.Glob(target + ".hmux-backup-" + sourceVersion + "-*")
	if err != nil || len(backups) != 1 {
		t.Fatalf("backups=%v err=%v", backups, err)
	}
	// Simulate a crash after RENAME_SWAP but before the previous bundle was
	// renamed out of the private stage directory. Recovery must adopt it as a
	// normal rollback candidate before the rollback command runs.
	orphanRoot := filepath.Join(root, ".hmux-app-stage-4242")
	if err := os.Mkdir(orphanRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(backups[0], filepath.Join(orphanRoot, appBundleName)); err != nil {
		t.Fatal(err)
	}
	if err := recoverAppStagesLocked(ctx, root, target, candidateVersion); err != nil {
		t.Fatal(err)
	}
	backups, err = filepath.Glob(target + ".hmux-backup-" + sourceVersion + "-*")
	if err != nil || len(backups) != 1 {
		t.Fatalf("recovered backups=%v err=%v", backups, err)
	}
	if _, err := os.Lstat(orphanRoot); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("orphan stage still exists: %v", err)
	}
	cache := filepath.Join(root, "cache")
	if err := os.Mkdir(cache, 0o700); err != nil {
		t.Fatal(err)
	}
	cfg := config.DefaultClientConfig()
	cfg.CacheDir = cache
	configPath := filepath.Join(root, "client.toml")
	if err := config.SaveClient(configPath, cfg); err != nil {
		t.Fatal(err)
	}
	helper := filepath.Join(target, "Contents", "Helpers", "hmux")
	command := exec.Command(helper, "--config", configPath, "--no-update-check", "app", "rollback-native")
	command.Env = make([]string, 0, len(os.Environ()))
	for _, item := range os.Environ() {
		if strings.HasPrefix(item, "HMUX_APP_VERSION=") || strings.HasPrefix(item, "HMUX_APP_BUNDLE_PATH=") {
			continue
		}
		command.Env = append(command.Env, item)
	}
	output, err := command.CombinedOutput()
	if err != nil || !strings.Contains(string(output), `"rolled_back":true`) ||
		!strings.Contains(string(output), `"version":"`+sourceVersion+`"`) {
		t.Fatalf("rollback CLI err=%v output=%s", err, output)
	}
	if version, err := plistValue(ctx, filepath.Join(target, "Contents", "Info.plist"), "CFBundleShortVersionString"); err != nil || version != sourceVersion {
		t.Fatalf("rolled-back version=%q err=%v", version, err)
	}
	hold, err := readAppRollbackStatus(cache)
	if err != nil || hold == nil || hold.BlockedVersion != candidateVersion || hold.RolledBackTo != sourceVersion {
		t.Fatalf("rollback hold=%#v err=%v", hold, err)
	}
}

func TestAtomicSwapPathsExchangesCompleteDirectories(t *testing.T) {
	root := t.TempDir()
	left := filepath.Join(root, "left")
	right := filepath.Join(root, "right")
	if err := os.Mkdir(left, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(right, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(left, "value"), []byte("old"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(right, "value"), []byte("new"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := atomicSwapPaths(left, right); err != nil {
		t.Fatal(err)
	}
	leftValue, leftErr := os.ReadFile(filepath.Join(left, "value"))
	rightValue, rightErr := os.ReadFile(filepath.Join(right, "value"))
	if leftErr != nil || rightErr != nil || string(leftValue) != "new" || string(rightValue) != "old" {
		t.Fatalf("left=%q err=%v right=%q err=%v", leftValue, leftErr, rightValue, rightErr)
	}
}

func signedAppManifest(t *testing.T, version string, artifact []byte) (model.Manifest, string) {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		t.Fatal(err)
	}
	publicPath := filepath.Join(t.TempDir(), "release-public-key.pem")
	if err := os.WriteFile(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(artifact)
	keySum := sha256.Sum256(publicDER)
	manifest := model.Manifest{
		SchemaVersion: model.SchemaVersion,
		Version:       version,
		Platform:      AppPlatform(),
		Artifact:      "hmux",
		SHA256:        hex.EncodeToString(sum[:]),
		Size:          int64(len(artifact)),
		PublishedAt:   time.Unix(1800000000, 0).UTC(),
		MinProtocol:   model.ProtocolVersion,
		SignatureType: SignatureType,
		KeyID:         hex.EncodeToString(keySum[:8]),
	}
	payload, err := SignaturePayload(manifest)
	if err != nil {
		t.Fatal(err)
	}
	manifest.ManifestSignature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, payload))
	manifest.Signature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, artifact))
	return manifest, publicPath
}

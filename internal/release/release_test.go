package release

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestVerifySignedArtifactAndInstall(t *testing.T) {
	data := []byte("safe artifact")
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	publicDER, _ := x509.MarshalPKIXPublicKey(publicKey)
	publicPath := filepath.Join(t.TempDir(), "public.pem")
	if err := os.WriteFile(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(data)
	keySum := sha256.Sum256(publicDER)
	manifest := model.Manifest{
		SchemaVersion: 1, Version: "0.1.0", Platform: Platform(), Artifact: "hmux",
		SHA256: hex.EncodeToString(sum[:]), Size: int64(len(data)),
		PublishedAt: time.Unix(1700000000, 0).UTC(), MinProtocol: model.ProtocolVersion,
		SignatureType: SignatureType, KeyID: hex.EncodeToString(keySum[:8]),
	}
	payload, err := SignaturePayload(manifest)
	if err != nil {
		t.Fatal(err)
	}
	manifest.Signature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, data))
	manifest.ManifestSignature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, payload))
	if err := VerifyArtifact(data, manifest, publicPath); err != nil {
		t.Fatal(err)
	}
	publicLink := filepath.Join(t.TempDir(), "public-link.pem")
	if err := os.Symlink(publicPath, publicLink); err != nil {
		t.Fatal(err)
	}
	if err := VerifyArtifact(data, manifest, publicLink); err == nil {
		t.Fatal("symlinked pinned public key was accepted")
	}
	bad := append([]byte(nil), data...)
	bad[0] ^= 1
	if err := VerifyArtifact(bad, manifest, publicPath); err == nil {
		t.Fatal("tampered artifact accepted")
	}
	relabelled := manifest
	relabelled.Version = "0.2.0"
	if err := VerifyArtifact(data, relabelled, publicPath); err == nil {
		t.Fatal("signed artifact was accepted with relabelled metadata")
	}
	unsigned := manifest
	unsigned.Signature = ""
	unsigned.ManifestSignature = ""
	unsigned.SignatureType = ""
	if err := VerifyArtifact(data, unsigned, publicPath); err == nil {
		t.Fatal("signature stripping was accepted despite a pinned key")
	}
	cfg := config.DefaultClientConfig()
	cfg.CacheDir = t.TempDir()
	cfg.PublicKeyPath = publicPath
	path, err := Install(cfg, manifest, data)
	if err != nil {
		t.Fatal(err)
	}
	if got, err := os.ReadFile(path); err != nil || string(got) != string(data) {
		t.Fatalf("installed artifact got=%q err=%v", got, err)
	}
	target, err := os.Readlink(filepath.Join(cfg.CacheDir, "current"))
	if err != nil || target != filepath.Join("releases", "0.1.0", "hmux") {
		t.Fatalf("current=%q err=%v", target, err)
	}
	if err := Rollback(cfg, "0.1.0"); err != nil {
		t.Fatal(err)
	}
	if repeatedPath, err := Install(cfg, manifest, data); err != nil || repeatedPath != path {
		t.Fatalf("idempotent install failed: path=%q err=%v", repeatedPath, err)
	}
	symlinkCache := filepath.Join(t.TempDir(), "cache")
	if err := os.Symlink(t.TempDir(), symlinkCache); err != nil {
		t.Fatal(err)
	}
	symlinkCfg := cfg
	symlinkCfg.CacheDir = symlinkCache
	if _, err := Install(symlinkCfg, manifest, data); err == nil {
		t.Fatal("symlinked cache root was accepted")
	}
	if err := os.WriteFile(path, []byte("tampered cache"), 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := Install(cfg, manifest, data); err == nil {
		t.Fatal("install silently replaced an existing tampered immutable release")
	}
	if got, err := os.ReadFile(path); err != nil || string(got) != "tampered cache" {
		t.Fatalf("rejected install changed immutable cache: got=%q err=%v", got, err)
	}
	if err := Rollback(cfg, "0.1.0"); err == nil {
		t.Fatal("tampered cached release was accepted")
	}
}

func TestInstallRejectsSignedRollbackButExplicitRollbackRemainsAvailable(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		t.Fatal(err)
	}
	publicPath := filepath.Join(t.TempDir(), "public.pem")
	if err := os.WriteFile(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	keySum := sha256.Sum256(publicDER)
	signed := func(version string, data []byte) model.Manifest {
		sum := sha256.Sum256(data)
		manifest := model.Manifest{
			SchemaVersion: model.SchemaVersion, Version: version, Platform: Platform(), Artifact: "hmux",
			SHA256: hex.EncodeToString(sum[:]), Size: int64(len(data)),
			PublishedAt: time.Unix(1800000000, 0).UTC(), MinProtocol: model.ProtocolVersion,
			SignatureType: SignatureType, KeyID: hex.EncodeToString(keySum[:8]),
		}
		payload, payloadErr := SignaturePayload(manifest)
		if payloadErr != nil {
			t.Fatal(payloadErr)
		}
		manifest.ManifestSignature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, payload))
		return manifest
	}
	cfg := config.DefaultClientConfig()
	cfg.CacheDir = t.TempDir()
	cfg.PublicKeyPath = publicPath
	newData := []byte("release-0.1.30")
	newManifest := signed("0.1.30", newData)
	if _, err := Install(cfg, newManifest, newData); err != nil {
		t.Fatal(err)
	}
	oldData := []byte("release-0.1.29")
	oldManifest := signed("0.1.29", oldData)
	if _, err := Install(cfg, oldManifest, oldData); err == nil || !strings.Contains(err.Error(), "rollback") {
		t.Fatalf("signed rollback error=%v", err)
	}
	if selected, err := SelectedVersion(cfg); err != nil || selected != "0.1.30" {
		t.Fatalf("selected=%q err=%v", selected, err)
	}
	if err := Rollback(cfg, "0.1.29"); err != nil {
		t.Fatal(err)
	}
	if selected, err := SelectedVersion(cfg); err != nil || selected != "0.1.29" {
		t.Fatalf("explicit rollback selected=%q err=%v", selected, err)
	}
}

func TestVersionValidationRejectsTraversalAndAmbiguousNames(t *testing.T) {
	for _, value := range []string{".", "..", "../0.1.0", "0.1.0/../x", "latest", "0.1.0\nnext"} {
		if ValidVersion(value) {
			t.Errorf("unsafe version %q was accepted", value)
		}
	}
	for _, value := range []string{"0.1.0", "1.2", "1.2.3-rc.1", "1.2.3+build.7"} {
		if !ValidVersion(value) {
			t.Errorf("valid version %q was rejected", value)
		}
	}
}

func TestManifestDecoderAndCacheRejectUnexpectedOrOversizedMetadata(t *testing.T) {
	var manifest model.Manifest
	if err := decodeManifest([]byte(`{"schema_version":1,"unexpected":true}`), &manifest); err == nil {
		t.Fatal("unknown manifest field was accepted")
	}
	if err := decodeManifest([]byte(`{} {}`), &manifest); err == nil {
		t.Fatal("trailing manifest JSON was accepted")
	}

	cfg := config.DefaultClientConfig()
	cfg.CacheDir = t.TempDir()
	releaseDir := filepath.Join(cfg.CacheDir, "releases", "0.1.0")
	if err := os.MkdirAll(releaseDir, 0o700); err != nil {
		t.Fatal(err)
	}
	oversized := bytes.Repeat([]byte{' '}, cachedManifestLimit+1)
	if err := os.WriteFile(filepath.Join(releaseDir, "manifest.json"), oversized, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := VerifyCached(cfg, "0.1.0"); err == nil {
		t.Fatal("oversized cached manifest was accepted")
	}
}

func TestBootstrapJQPayloadMatchesGoCanonicalPayload(t *testing.T) {
	if _, err := exec.LookPath("jq"); err != nil {
		t.Skip("jq is unavailable")
	}
	if _, err := exec.LookPath("openssl"); err != nil {
		t.Skip("openssl is unavailable")
	}
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		t.Fatal(err)
	}
	keySum := sha256.Sum256(publicDER)
	artifact := []byte("bootstrap artifact")
	artifactSum := sha256.Sum256(artifact)
	manifest := model.Manifest{
		SchemaVersion: model.SchemaVersion, Version: "0.1.3", Platform: Platform(),
		Artifact: "hmux", SHA256: hex.EncodeToString(artifactSum[:]), Size: int64(len(artifact)),
		PublishedAt: time.Unix(1700000000, 123456789).UTC(), MinProtocol: model.ProtocolVersion,
		SignatureType: SignatureType, KeyID: hex.EncodeToString(keySum[:8]),
	}
	payload, err := SignaturePayload(manifest)
	if err != nil {
		t.Fatal(err)
	}
	manifest.ManifestSignature = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, payload))
	manifestData, err := json.Marshal(manifest)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	manifestPath := filepath.Join(dir, "manifest.json")
	if err := os.WriteFile(manifestPath, manifestData, 0o600); err != nil {
		t.Fatal(err)
	}
	filter := `{
domain:"hmux-release-manifest-v1",
schema_version:.schema_version,
version:.version,
platform:.platform,
artifact:.artifact,
sha256:(.sha256|ascii_downcase),
size:.size,
published_at:.published_at,
min_protocol:.min_protocol,
signature_type:.signature_type,
key_id:.key_id
}`
	jqPayload, err := exec.Command("jq", "-cj", filter, manifestPath).Output()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(jqPayload, payload) {
		t.Fatalf("bootstrap jq payload differs:\njq=%s\ngo=%s", jqPayload, payload)
	}
	publicPath := filepath.Join(dir, "public.pem")
	signaturePath := filepath.Join(dir, "signature.bin")
	payloadPath := filepath.Join(dir, "payload.json")
	if err := os.WriteFile(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(signaturePath, ed25519.Sign(privateKey, payload), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(payloadPath, payload, 0o600); err != nil {
		t.Fatal(err)
	}
	command := exec.Command("openssl", "pkeyutl", "-verify", "-pubin", "-inkey", publicPath,
		"-rawin", "-in", payloadPath, "-sigfile", signaturePath)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("openssl bootstrap verification failed: %v: %s", err, output)
	}
}

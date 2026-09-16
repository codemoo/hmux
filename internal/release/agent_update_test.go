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
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func TestInstallAgentArtifactIsSignedAtomicAndBackedUp(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "hmux-agent")
	if err := os.WriteFile(target, []byte("old-agent"), 0o700); err != nil {
		t.Fatal(err)
	}
	artifact := []byte("new-agent")
	manifest, publicPath := signedAgentManifest(t, dir, "0.1.28", artifact)
	validator := func(path string) error {
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if string(data) != string(artifact) {
			return errors.New("unexpected candidate")
		}
		return nil
	}
	if err := installAgentArtifact(context.Background(),
		target, "0.1.27", manifest, artifact, publicPath,
		time.Unix(1800000000, 0), validator,
	); err != nil {
		t.Fatal(err)
	}
	if data, err := os.ReadFile(target); err != nil || string(data) != "new-agent" {
		t.Fatalf("installed agent=%q err=%v", data, err)
	}
	backups, err := filepath.Glob(target + ".hmux-backup-*")
	if err != nil || len(backups) != 1 {
		t.Fatalf("backups=%v err=%v", backups, err)
	}
	if data, err := os.ReadFile(backups[0]); err != nil || string(data) != "old-agent" {
		t.Fatalf("backup=%q err=%v", data, err)
	}
}

func TestInstallAgentArtifactRejectsRoleSwapAndRollsBackValidationFailure(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "hmux-agent")
	if err := os.WriteFile(target, []byte("old-agent"), 0o700); err != nil {
		t.Fatal(err)
	}
	artifact := []byte("new-agent")
	manifest, publicPath := signedAgentManifest(t, dir, "0.1.28", artifact)

	wrongRole := manifest
	wrongRole.Platform = Platform()
	if err := installAgentArtifact(context.Background(),
		target, "0.1.27", wrongRole, artifact, publicPath,
		time.Unix(1800000000, 0), func(string) error { return nil },
	); err == nil {
		t.Fatal("client-role artifact was accepted as an agent")
	}
	if data, _ := os.ReadFile(target); string(data) != "old-agent" {
		t.Fatalf("role rejection changed target: %q", data)
	}

	checks := 0
	err := installAgentArtifact(context.Background(),
		target, "0.1.27", manifest, artifact, publicPath,
		time.Unix(1800000000, 0), func(string) error {
			checks++
			if checks == 2 {
				return errors.New("post-install failure")
			}
			return nil
		},
	)
	if err == nil || !strings.Contains(err.Error(), "rolled back") {
		t.Fatalf("validation failure err=%v", err)
	}
	if data, readErr := os.ReadFile(target); readErr != nil || string(data) != "old-agent" {
		t.Fatalf("rollback target=%q err=%v", data, readErr)
	}
}

func signedAgentManifest(t *testing.T, dir, version string, artifact []byte) (model.Manifest, string) {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	publicDER, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		t.Fatal(err)
	}
	publicPath := filepath.Join(dir, "release-public-key.pem")
	if err := os.WriteFile(publicPath, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: publicDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(artifact)
	keySum := sha256.Sum256(publicDER)
	manifest := model.Manifest{
		SchemaVersion: model.SchemaVersion,
		Version:       version,
		Platform:      AgentPlatform(),
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

package control

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/binary"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/release"
)

func testInventory() model.Inventory {
	return model.Inventory{
		SchemaVersion: 1, Revision: "test",
		Clients:      []model.Client{{ID: "home-mac", Role: "home"}},
		IdentityRefs: []model.IdentityRef{{ID: "key", Path: "~/.ssh/EXAMPLE_TEST_KEY"}},
		Hosts: []model.Host{
			{ID: "dmz", SSHAlias: "hmux-dmz", Address: "dmz.invalid", User: "u", Port: 22, IdentityRef: "key"},
			{ID: "home", SSHAlias: "hmux-home", Address: "home.invalid", User: "u", Port: 22, IdentityRef: "key", ProxyJump: "dmz"},
		},
		Profiles: []model.Profile{{ID: "shell", Label: "Shell", DefaultDirectory: "~/", Command: []string{"zsh"}}},
	}
}

func TestHostUpsertDryRunAndAutomaticReconcile(t *testing.T) {
	store := Store{Root: t.TempDir()}
	if err := config.SaveInventory(store.InventoryPath(), testInventory()); err != nil {
		t.Fatal(err)
	}
	if err := store.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	host := testInventory().Hosts[1]
	host.Address = "new-home.invalid"
	if err := store.HostUpsert(host, true, true); err != nil {
		t.Fatal(err)
	}
	inventory, err := store.Validate()
	if err != nil {
		t.Fatal(err)
	}
	if inventory.Hosts[1].Address != "home.invalid" {
		t.Fatal("dry-run changed inventory")
	}
	if err := store.HostUpsert(host, true, false); err != nil {
		t.Fatal(err)
	}
	rendered, err := os.ReadFile(filepath.Join(store.Root, "rendered", "ssh", "50-hmux.generated.conf"))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(rendered, []byte("HostName new-home.invalid")) {
		t.Fatal("host edit did not automatically reconcile SSH output")
	}
}

func TestHostDiffDetectsMissingAndCurrentRenderedArtifacts(t *testing.T) {
	store := Store{Root: t.TempDir()}
	if err := config.SaveInventory(store.InventoryPath(), testInventory()); err != nil {
		t.Fatal(err)
	}
	diff, err := store.HostDiff()
	if err != nil {
		t.Fatal(err)
	}
	if pending, _ := diff["pending"].(bool); !pending {
		t.Fatal("missing rendered artifacts were not reported as pending")
	}
	if err := store.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	diff, err = store.HostDiff()
	if err != nil {
		t.Fatal(err)
	}
	if pending, _ := diff["pending"].(bool); pending {
		t.Fatal("current rendered artifacts were reported as pending")
	}
}

func TestRenderedRejectsSymlinkAndWritableArtifact(t *testing.T) {
	store := Store{Root: t.TempDir()}
	if err := config.SaveInventory(store.InventoryPath(), testInventory()); err != nil {
		t.Fatal(err)
	}
	if err := store.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(store.Root, "rendered", "ssh", "50-hmux.generated.conf")
	original, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o622); err != nil {
		t.Fatal(err)
	}
	if err := store.Rendered("ssh", &bytes.Buffer{}); err == nil {
		t.Fatal("group/world-writable rendered artifact was accepted")
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(t.TempDir(), "ssh.conf")
	if err := os.WriteFile(target, original, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, path); err != nil {
		t.Fatal(err)
	}
	if err := store.Rendered("ssh", &bytes.Buffer{}); err == nil {
		t.Fatal("symlinked rendered artifact was accepted")
	}
}

func TestReconcileFailureRestoresPreviouslyRenderedSSH(t *testing.T) {
	store := Store{Root: t.TempDir()}
	inventory := testInventory()
	if err := config.SaveInventory(store.InventoryPath(), inventory); err != nil {
		t.Fatal(err)
	}
	if err := store.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	sshPath := filepath.Join(store.Root, "rendered", "ssh", "50-hmux.generated.conf")
	before, err := os.ReadFile(sshPath)
	if err != nil {
		t.Fatal(err)
	}
	inventory.Hosts[1].Address = "changed-home.invalid"
	if err := config.SaveInventory(store.InventoryPath(), inventory); err != nil {
		t.Fatal(err)
	}
	termiusDir := filepath.Join(store.Root, "rendered", "termius")
	if err := os.RemoveAll(termiusDir); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(termiusDir, []byte("blocks directory creation"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := store.Reconcile(context.Background()); err == nil {
		t.Fatal("expected reconcile failure")
	}
	after, err := os.ReadFile(sshPath)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("failed reconcile replaced last-known-good SSH output")
	}
}

func TestSignedPublishSupportsLegacyAndManifestVerification(t *testing.T) {
	store := Store{Root: t.TempDir()}
	privatePath := filepath.Join(t.TempDir(), "private.pem")
	publicPath := filepath.Join(t.TempDir(), "public.pem")
	if err := GenerateKeyPair(privatePath, publicPath); err != nil {
		t.Fatal(err)
	}
	artifactData := []byte("signed binary")
	artifactPath := filepath.Join(t.TempDir(), "hmux")
	if err := os.WriteFile(artifactPath, artifactData, 0o700); err != nil {
		t.Fatal(err)
	}
	platform := release.Platform()
	if err := store.Publish("0.1.3", []string{platform + "=" + artifactPath}, privatePath); err != nil {
		t.Fatal(err)
	}
	manifest, err := store.Manifest(platform, "0.1.3")
	if err != nil {
		t.Fatal(err)
	}
	if manifest.Signature == "" || manifest.ManifestSignature == "" {
		t.Fatal("publish did not create both transition signatures")
	}
	if err := release.VerifyArtifact(artifactData, manifest, publicPath); err != nil {
		t.Fatal(err)
	}
}

func TestPublishFailureLeavesNoPartialRelease(t *testing.T) {
	store := Store{Root: t.TempDir()}
	artifactPath := filepath.Join(t.TempDir(), "hmux")
	if err := os.WriteFile(artifactPath, []byte("binary"), 0o700); err != nil {
		t.Fatal(err)
	}
	err := store.Publish("0.9.0", []string{
		"darwin-arm64=" + artifactPath,
		"darwin-amd64=" + filepath.Join(t.TempDir(), "missing"),
	}, "")
	if err == nil {
		t.Fatal("expected publish failure")
	}
	if _, statErr := os.Stat(filepath.Join(store.Root, "releases", "0.9.0")); !os.IsNotExist(statErr) {
		t.Fatal("partial release directory was retained")
	}
	if _, statErr := os.Lstat(filepath.Join(store.Root, "current")); !os.IsNotExist(statErr) {
		t.Fatal("current changed after failed publish")
	}
}

func TestDMZRollbackRejectsTamperedArtifact(t *testing.T) {
	store := Store{Root: t.TempDir()}
	artifactPath := filepath.Join(t.TempDir(), "hmux")
	if err := os.WriteFile(artifactPath, []byte("binary"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := store.Publish("0.8.0", []string{"darwin-arm64=" + artifactPath}, ""); err != nil {
		t.Fatal(err)
	}
	cached := filepath.Join(store.Root, "releases", "0.8.0", "darwin-arm64", "hmux")
	if err := os.WriteFile(cached, []byte("tamper"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := store.Rollback("0.8.0"); err == nil {
		t.Fatal("DMZ rollback accepted a tampered artifact")
	}
}

func TestManifestAndArtifactRejectTamperedReleaseFiles(t *testing.T) {
	store := Store{Root: t.TempDir()}
	source := filepath.Join(t.TempDir(), "hmux")
	if err := os.WriteFile(source, []byte("binary"), 0o700); err != nil {
		t.Fatal(err)
	}
	const version = "0.8.1"
	const platform = "darwin-arm64"
	if err := store.Publish(version, []string{platform + "=" + source}, ""); err != nil {
		t.Fatal(err)
	}
	manifestPath := filepath.Join(
		store.Root, "releases", version, "manifest-"+platform+".json",
	)
	originalManifest, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatal(err)
	}
	unknownField := append([]byte(nil), bytes.TrimSpace(originalManifest)...)
	lastBrace := bytes.LastIndexByte(unknownField, '}')
	if lastBrace < 0 {
		t.Fatal("published manifest is not JSON")
	}
	unknownField = append(unknownField[:lastBrace], append([]byte(`,"unexpected":true}`), unknownField[lastBrace+1:]...)...)
	for name, data := range map[string][]byte{
		"unknown field": unknownField,
		"trailing JSON": append(append([]byte(nil), originalManifest...), []byte(`{}`)...),
	} {
		t.Run(name, func(t *testing.T) {
			t.Cleanup(func() {
				if err := os.WriteFile(manifestPath, originalManifest, 0o600); err != nil {
					t.Error(err)
				}
			})
			if err := os.WriteFile(manifestPath, data, 0o600); err != nil {
				t.Fatal(err)
			}
			if _, err := store.Manifest(platform, version); err == nil {
				t.Fatal("tampered manifest was accepted")
			}
		})
	}

	artifactPath := filepath.Join(store.Root, "releases", version, platform, "hmux")
	if err := os.Remove(artifactPath); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(source, artifactPath); err != nil {
		t.Fatal(err)
	}
	if err := store.Artifact(platform, version, &bytes.Buffer{}); err == nil {
		t.Fatal("symlinked release artifact was accepted")
	}
}

func TestSafeVersionRejectsTraversal(t *testing.T) {
	for _, value := range []string{".", "..", "../0.1.0", "latest"} {
		if err := safeVersion(value); err == nil {
			t.Errorf("unsafe version %q accepted", value)
		}
	}
}

func TestAuthorizeStagedClientKeyIsValidatedAndIdempotent(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	store := Store{Root: filepath.Join(home, ".local", "share", "hmux-control")}
	incoming := filepath.Join(store.Root, "state", "client-key.pub.incoming")
	if err := os.MkdirAll(filepath.Dir(incoming), 0o700); err != nil {
		t.Fatal(err)
	}
	publicKey, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	var blob bytes.Buffer
	for _, field := range [][]byte{[]byte("ssh-ed25519"), publicKey} {
		if err := binary.Write(&blob, binary.BigEndian, uint32(len(field))); err != nil {
			t.Fatal(err)
		}
		if _, err := blob.Write(field); err != nil {
			t.Fatal(err)
		}
	}
	encodedKey := base64.StdEncoding.EncodeToString(blob.Bytes())
	for name, candidate := range map[string][]byte{
		"truncated": blob.Bytes()[:blob.Len()-1],
		"trailing":  append(append([]byte(nil), blob.Bytes()...), 0),
		"raw":       publicKey,
	} {
		if validOpenSSHEd25519Blob(candidate) {
			t.Fatalf("%s OpenSSH blob was accepted", name)
		}
	}
	line := "ssh-ed25519 " + encodedKey + " test-comment\n"
	sshDir := filepath.Join(home, ".ssh")
	if err := os.MkdirAll(sshDir, 0o700); err != nil {
		t.Fatal(err)
	}
	restrictedLine := "restrict ssh-ed25519 " + encodedKey + " existing-comment\n"
	if err := os.WriteFile(filepath.Join(sshDir, "authorized_keys"), []byte(restrictedLine), 0o600); err != nil {
		t.Fatal(err)
	}
	for run := 0; run < 2; run++ {
		if err := os.WriteFile(incoming, []byte(line), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := store.AuthorizeStagedClientKey(); err != nil {
			t.Fatal(err)
		}
	}
	authorized, err := os.ReadFile(filepath.Join(home, ".ssh", "authorized_keys"))
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Count(authorized, []byte(encodedKey)) != 1 {
		t.Fatalf("public key was not authorized exactly once: %q", authorized)
	}
	if string(authorized) != restrictedLine {
		t.Fatalf("existing restricted authorization was weakened: %q", authorized)
	}
	if _, err := os.Stat(incoming); !os.IsNotExist(err) {
		t.Fatal("staged public key was not removed")
	}
	if err := os.WriteFile(incoming, []byte("ssh-rsa invalid\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := store.AuthorizeStagedClientKey(); err == nil {
		t.Fatal("invalid staged key was accepted")
	}
	rawKeyLine := "ssh-ed25519 " + base64.StdEncoding.EncodeToString(publicKey) + "\n"
	if err := os.WriteFile(incoming, []byte(rawKeyLine), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := store.AuthorizeStagedClientKey(); err == nil {
		t.Fatal("raw Ed25519 bytes were accepted as an OpenSSH public key blob")
	}
}

func TestOpenSSHEd25519BlobMatchesSSHKeygenOutput(t *testing.T) {
	sshKeygen, err := exec.LookPath("ssh-keygen")
	if err != nil {
		t.Skip("ssh-keygen is unavailable")
	}
	privatePath := filepath.Join(t.TempDir(), "id_ed25519")
	if output, err := exec.Command(sshKeygen, "-q", "-t", "ed25519", "-N", "", "-f", privatePath).CombinedOutput(); err != nil {
		t.Fatalf("ssh-keygen: %v: %s", err, output)
	}
	publicData, err := os.ReadFile(privatePath + ".pub")
	if err != nil {
		t.Fatal(err)
	}
	fields := strings.Fields(string(publicData))
	if len(fields) < 2 {
		t.Fatal("ssh-keygen returned malformed public key")
	}
	blob, err := base64.StdEncoding.DecodeString(fields[1])
	if err != nil || !validOpenSSHEd25519Blob(blob) {
		t.Fatalf("real ssh-keygen Ed25519 blob was rejected: %v", err)
	}
}

func TestGenerateKeyPairRefusesOverwrite(t *testing.T) {
	dir := t.TempDir()
	privatePath := filepath.Join(dir, "private.pem")
	publicPath := filepath.Join(dir, "public.pem")
	if err := GenerateKeyPair(privatePath, publicPath); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(privatePath)
	if err != nil {
		t.Fatal(err)
	}
	if err := GenerateKeyPair(privatePath, publicPath); err == nil {
		t.Fatal("existing signing key was overwritten")
	}
	after, err := os.ReadFile(privatePath)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("private key changed after refused overwrite")
	}
}

func TestReconcilePublishAndRollback(t *testing.T) {
	store := Store{Root: t.TempDir()}
	if err := config.SaveInventory(store.InventoryPath(), testInventory()); err != nil {
		t.Fatal(err)
	}
	if err := store.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	var ssh bytes.Buffer
	if err := store.Rendered("ssh", &ssh); err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(ssh.Bytes(), []byte("ProxyJump hmux-dmz")) {
		t.Fatal("rendered SSH fragment lacks ProxyJump")
	}
	if data, err := os.ReadFile(filepath.Join(store.Root, "rendered", "termius", "hmux-hosts.csv")); err != nil || !bytes.Contains(data, []byte("JumpHost")) {
		t.Fatalf("Termius artifact err=%v", err)
	}
	artifact := filepath.Join(t.TempDir(), "hmux")
	if err := os.WriteFile(artifact, []byte("binary"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := store.Publish("0.1.0", []string{"darwin-arm64=" + artifact}, ""); err != nil {
		t.Fatal(err)
	}
	var output bytes.Buffer
	if err := store.Artifact("darwin-arm64", "0.1.0", &output); err != nil || output.String() != "binary" {
		t.Fatalf("artifact=%q err=%v", output.String(), err)
	}
	if err := store.Publish("0.2.0", []string{"darwin-arm64=" + artifact}, ""); err != nil {
		t.Fatal(err)
	}
	if err := store.Rollback("0.1.0"); err != nil {
		t.Fatal(err)
	}
	target, _ := os.Readlink(filepath.Join(store.Root, "current"))
	if target != filepath.Join("releases", "0.1.0") {
		t.Fatalf("current=%q", target)
	}
}

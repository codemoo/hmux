package release

import (
	"archive/zip"
	"bytes"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestExtractAppArchiveAcceptsOnlyContainedPathsAndSymlinks(t *testing.T) {
	archive := appTestArchive(t, []appTestZipEntry{
		{name: "HMux.app/", mode: os.ModeDir | 0o755},
		{name: "HMux.app/Contents/", mode: os.ModeDir | 0o755},
		{name: "HMux.app/Contents/file", mode: 0o644, data: "safe"},
		{name: "HMux.app/Contents/link", mode: os.ModeSymlink | 0o755, data: "file"},
	})
	root := t.TempDir()
	bundle, err := extractAppArchive(archive, root)
	if err != nil {
		t.Fatal(err)
	}
	if bundle != filepath.Join(root, appBundleName) {
		t.Fatalf("bundle=%q", bundle)
	}
	resolved, err := filepath.EvalSymlinks(filepath.Join(bundle, "Contents", "link"))
	expected, expectedErr := filepath.EvalSymlinks(filepath.Join(bundle, "Contents", "file"))
	if err != nil || expectedErr != nil || resolved != expected {
		t.Fatalf("resolved=%q err=%v", resolved, err)
	}

	for name, entries := range map[string][]appTestZipEntry{
		"traversal": {
			{name: "HMux.app/../outside", mode: 0o644, data: "bad"},
		},
		"outside-root": {
			{name: "other.app/file", mode: 0o644, data: "bad"},
		},
		"case-collision": {
			{name: "HMux.app/File", mode: 0o644, data: "one"},
			{name: "HMux.app/file", mode: 0o644, data: "two"},
		},
		"escaping-symlink": {
			{name: "HMux.app/link", mode: os.ModeSymlink | 0o755, data: "../outside"},
		},
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := extractAppArchive(appTestArchive(t, entries), t.TempDir()); err == nil {
				t.Fatal("unsafe app archive was accepted")
			}
		})
	}
}

func TestAppUpdateStatusIsPrivateAndVersionAware(t *testing.T) {
	cache := t.TempDir()
	status := AppUpdateStatus{Version: "0.1.28", InstalledAt: time.Unix(1800000000, 0).UTC()}
	if err := recordAppUpdateStatus(cache, status); err != nil {
		t.Fatal(err)
	}
	got, err := ReadAppUpdateStatus(cache, "0.1.27")
	if err != nil || got == nil || got.Version != status.Version {
		t.Fatalf("status=%#v err=%v", got, err)
	}
	if got, err := ReadAppUpdateStatus(cache, "0.1.28"); err != nil || got != nil {
		t.Fatalf("current app saw stale restart status=%#v err=%v", got, err)
	}
	statePath := filepath.Join(cache, appUpdateStatusName)
	if err := os.Chmod(statePath, 0o622); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadAppUpdateStatus(cache, "0.1.27"); err == nil {
		t.Fatal("writable app update state was accepted")
	}
}

func TestNativeRollbackHoldSuppressesOnlyBlockedVersionBanner(t *testing.T) {
	cache := t.TempDir()
	status := AppUpdateStatus{Version: "0.1.29", InstalledAt: time.Unix(1800000000, 0).UTC()}
	if err := recordAppUpdateStatus(cache, status); err != nil {
		t.Fatal(err)
	}
	hold := AppRollbackStatus{
		BlockedVersion: "0.1.29",
		RolledBackTo:   "0.1.28",
		RolledBackAt:   time.Unix(1800000001, 0).UTC(),
	}
	if err := recordAppRollbackStatus(cache, hold); err != nil {
		t.Fatal(err)
	}
	if got, err := ReadAppUpdateStatus(cache, "0.1.28"); err != nil || got != nil {
		t.Fatalf("blocked update banner=%#v err=%v", got, err)
	}
	if err := recordAppUpdateStatus(cache, AppUpdateStatus{
		Version: "0.1.30", InstalledAt: time.Unix(1800000002, 0).UTC(),
	}); err != nil {
		t.Fatal(err)
	}
	if got, err := ReadAppUpdateStatus(cache, "0.1.28"); err != nil || got == nil || got.Version != "0.1.30" {
		t.Fatalf("newer update banner=%#v err=%v", got, err)
	}
}

func TestAppStagePIDAcceptsOnlyExactPositiveDecimalSuffix(t *testing.T) {
	for _, value := range []string{".hmux-app-stage-1", ".hmux-app-stage-2147483647"} {
		if _, ok := appStagePID(value); !ok {
			t.Fatalf("valid stage name rejected: %q", value)
		}
	}
	for _, value := range []string{
		".hmux-app-stage-", ".hmux-app-stage-0", ".hmux-app-stage--1",
		".hmux-app-stage-1/child", ".hmux-app-stage-2147483648", "hmux-app-stage-1",
	} {
		if _, ok := appStagePID(value); ok {
			t.Fatalf("unsafe stage name accepted: %q", value)
		}
	}
}

type appTestZipEntry struct {
	name string
	mode os.FileMode
	data string
}

func appTestArchive(t *testing.T, entries []appTestZipEntry) []byte {
	t.Helper()
	var data bytes.Buffer
	writer := zip.NewWriter(&data)
	for _, entry := range entries {
		header := &zip.FileHeader{Name: entry.name, Method: zip.Store}
		header.SetMode(entry.mode)
		file, err := writer.CreateHeader(header)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := file.Write([]byte(entry.data)); err != nil {
			t.Fatal(err)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	return data.Bytes()
}

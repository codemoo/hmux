package tabstate

import (
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

const testLauncher = "0123456789abcdef0123456789abcdef"

func TestStoreTracksOpenOrderPerLauncher(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	client := "/dev/ttys101"
	for _, id := range []string{"$7", "$2", "$9", "$2"} {
		if err := store.OpenFrame(testLauncher, client, client, id); err != nil {
			t.Fatal(err)
		}
	}
	frame, err := store.Frame(testLauncher)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(frame.Sessions, []string{"$7", "$2", "$9"}) ||
		frame.CurrentID != "$2" || frame.ClientName != client {
		t.Fatalf("frame=%#v", frame)
	}
}

func TestUpdateFrameTracksCurrentTabWithoutTmuxOptions(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	client := "/dev/ttys404"
	for _, id := range []string{"$1", "$2", "$3"} {
		if err := store.OpenFrame(testLauncher, client, client, id); err != nil {
			t.Fatal(err)
		}
	}
	if err := store.UpdateFrame(
		testLauncher, client, []string{"$1", "$3"}, "$1",
	); err != nil {
		t.Fatal(err)
	}
	frame, err := store.Frame(testLauncher)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(frame.Sessions, []string{"$1", "$3"}) ||
		frame.CurrentID != "$1" {
		t.Fatalf("frame=%#v", frame)
	}
	if err := store.UpdateFrame(
		testLauncher, "/dev/ttys999", []string{"$1"}, "$1",
	); err == nil {
		t.Fatal("stale frame client updated launcher state")
	}
}

func TestTabsRemainReadableAfterFrameCloses(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	client := "/dev/ttys505"
	for _, id := range []string{"$3", "$8"} {
		if err := store.OpenFrame(testLauncher, client, client, id); err != nil {
			t.Fatal(err)
		}
	}
	tabs, err := store.Tabs(testLauncher)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(tabs.Sessions, []string{"$3", "$8"}) ||
		tabs.CurrentID != "$8" {
		t.Fatalf("tabs=%#v", tabs)
	}
	if err := store.UpdateFrame(testLauncher, client, nil, ""); err != nil {
		t.Fatal(err)
	}
	tabs, err = store.Tabs(testLauncher)
	if err != nil {
		t.Fatal(err)
	}
	if len(tabs.Sessions) != 0 || tabs.CurrentID != "" {
		t.Fatalf("closed tabs=%#v", tabs)
	}
}

func TestStoreSeparatesLaunchersAndCleanupKeepsLockInode(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	other := "fedcba9876543210fedcba9876543210"
	if err := store.OpenFrame(testLauncher, "/dev/ttys1", "/dev/ttys1", "$1"); err != nil {
		t.Fatal(err)
	}
	if err := store.OpenFrame(other, "/dev/ttys2", "/dev/ttys2", "$2"); err != nil {
		t.Fatal(err)
	}
	one, err := store.Frame(testLauncher)
	if err != nil {
		t.Fatal(err)
	}
	two, err := store.Frame(other)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(one.Sessions, []string{"$1"}) ||
		!reflect.DeepEqual(two.Sessions, []string{"$2"}) {
		t.Fatalf("one=%v two=%v", one.Sessions, two.Sessions)
	}
	if err := store.Cleanup(testLauncher); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Frame(testLauncher); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("cleaned launcher remained visible: %v", err)
	}
	info, err := os.Lstat(store.lockPath(testLauncher))
	if err != nil || !info.Mode().IsRegular() {
		t.Fatalf("launcher lock inode was removed or replaced: %v", err)
	}
}

func TestStoreRejectsUnsafeInputsAndSymlinkTargets(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	for _, launcher := range []string{"", "ABCDEF0123456789ABCDEF0123456789", "../bad"} {
		if err := store.OpenFrame(launcher, "/dev/ttys1", "/dev/ttys1", "$1"); err == nil {
			t.Fatalf("unsafe launcher accepted: %q", launcher)
		}
	}
	if err := store.OpenFrame(testLauncher, "client;bad", "/dev/ttys1", "$1"); err == nil {
		t.Fatal("unsafe client accepted")
	}
	if err := store.OpenFrame(testLauncher, "/dev/ttys1", "/dev/ttys1", "$1;bad"); err == nil {
		t.Fatal("unsafe session accepted")
	}
	if err := store.OpenFrame(testLauncher, "/dev/ttys1", "/dev/ttys1", "$1"); err != nil {
		t.Fatal(err)
	}
	target := store.launcherPath(testLauncher)
	if err := os.Remove(target); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(t.TempDir(), "target"), target); err != nil {
		t.Fatal(err)
	}
	if err := store.UpdateFrame(
		testLauncher, "/dev/ttys1", []string{"$2"}, "$2",
	); err == nil {
		t.Fatal("symlinked state target accepted")
	}
}

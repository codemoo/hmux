package safefile

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

func TestValidateJSONArrayLimitRejectsBeforeSliceAllocation(t *testing.T) {
	if err := ValidateJSONArrayLimit([]byte(`[{},{},{}]`), 2); err == nil {
		t.Fatal("oversized array was accepted")
	}
	if err := ValidateJSONArrayLimit([]byte(`[{"ok":true},{}]`), 2); err != nil {
		t.Fatal(err)
	}
	if err := ValidateJSONArrayLimit([]byte(`[] {}`), 2); err == nil {
		t.Fatal("trailing JSON was accepted")
	}
}

func TestReadRejectsSymlinkFIFOAndOversizeWithoutBlocking(t *testing.T) {
	directory := t.TempDir()
	regular := filepath.Join(directory, "regular.json")
	if err := os.WriteFile(regular, []byte("{}"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(directory, "link.json")
	if err := os.Symlink(regular, link); err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(directory, "state.fifo")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	large := filepath.Join(directory, "large.json")
	if err := os.WriteFile(large, []byte("12345"), 0o600); err != nil {
		t.Fatal(err)
	}

	for _, path := range []string{link, fifo, large} {
		started := time.Now()
		if _, err := Read(path, 4); err == nil {
			t.Fatalf("unsafe file accepted: %s", filepath.Base(path))
		}
		if elapsed := time.Since(started); elapsed > time.Second {
			t.Fatalf("unsafe file read blocked for %v", elapsed)
		}
	}
	if snapshot, err := Read(regular, 4); err != nil || string(snapshot.Data) != "{}" {
		t.Fatalf("regular snapshot=%q err=%v", snapshot.Data, err)
	}
	if _, err := Read(filepath.Join(directory, "missing"), 4); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing error=%v", err)
	}
}

func TestInspectRejectsWritableState(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	if err := os.WriteFile(path, []byte("{}"), 0o622); err != nil {
		t.Fatal(err)
	}
	// WriteFile honors the process umask, so force the unsafe mode that this
	// test is specifically intended to exercise.
	if err := os.Chmod(path, 0o622); err != nil {
		t.Fatal(err)
	}
	if _, err := Inspect(path, 1024); err == nil {
		t.Fatal("group/world-writable state was accepted")
	}
}

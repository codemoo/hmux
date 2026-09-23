package homeservice

import (
	"encoding/binary"
	"os"
	"reflect"
	"testing"

	"golang.org/x/sys/unix"
)

func TestDarwinConnectorCandidateHandlesKernelCStringPadding(t *testing.T) {
	var info unix.KinfoProc
	info.Eproc.Pcred.P_ruid = uint32(os.Getuid())
	info.Proc.P_pid = int32(os.Getpid() + 1)
	copy(info.Proc.P_comm[:], "hmux-web\x000\x00n\x00rap")
	if !darwinConnectorCandidate(info) {
		t.Fatal("missed connector with nonzero bytes after the first NUL")
	}
	info.Eproc.Pcred.P_ruid++
	if darwinConnectorCandidate(info) {
		t.Fatal("selected another user's process")
	}
	info.Eproc.Pcred.P_ruid = uint32(os.Getuid())
	info.Proc.P_pid = int32(os.Getpid())
	if darwinConnectorCandidate(info) {
		t.Fatal("selected the installer")
	}
	info.Proc.P_pid++
	copy(info.Proc.P_comm[:], "hmux-web-other\x00")
	if darwinConnectorCandidate(info) {
		t.Fatal("selected a different command with the same prefix")
	}
}

func TestDarwinArgumentsKeepSpacesAndDoNotReturnEnvironment(t *testing.T) {
	want := []string{"/Users/test user/hmux-web", "connect", "--token-file", "/Users/test user/private/token"}
	raw := make([]byte, 4)
	binary.NativeEndian.PutUint32(raw, uint32(len(want)))
	raw = append(raw, []byte("/Users/test user/hmux-web\x00\x00")...)
	for _, arg := range want {
		raw = append(raw, []byte(arg)...)
		raw = append(raw, 0)
	}
	raw = append(raw, []byte("PRIVATE_TEST_TOKEN=must-not-return\x00")...)
	got, err := darwinArguments(raw)
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatal(got, err)
	}
	if _, err := darwinArguments(raw[:8]); err == nil {
		t.Fatal("accepted truncated process metadata")
	}
}

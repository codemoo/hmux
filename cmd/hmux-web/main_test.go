package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestCommandValidationBeforeRuntimeAccess(t *testing.T) {
	cases := []struct {
		args []string
		want string
	}{
		{nil, "usage:"},
		{[]string{"unknown"}, "unknown command"},
		{[]string{"serve", "unexpected"}, "unexpected arguments"},
		{[]string{"serve", "--listen", "0.0.0.0:8088"}, "loopback"},
		{[]string{"init"}, "--credentials and --token-file required"},
	}
	for _, tc := range cases {
		if err := run(tc.args); err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Fatalf("%v: got %v, want %q", tc.args, err, tc.want)
		}
	}
}
func TestInitNeverOverwritesExistingSecret(t *testing.T) {
	root := t.TempDir()
	credentials := filepath.Join(root, "credentials.json")
	token := filepath.Join(root, "connector.token")
	const previous = "synthetic-existing-secret\n"
	if err := os.WriteFile(credentials, []byte(previous), 0600); err != nil {
		t.Fatal(err)
	}
	err := run([]string{"init", "--credentials", credentials, "--token-file", token})
	if err == nil || !strings.Contains(err.Error(), "refusing to overwrite") {
		t.Fatal(err)
	}
	got, err := os.ReadFile(credentials)
	if err != nil || string(got) != previous {
		t.Fatal("existing credentials changed")
	}
	if _, err := os.Lstat(token); !os.IsNotExist(err) {
		t.Fatal("partial token unexpectedly created")
	}
}

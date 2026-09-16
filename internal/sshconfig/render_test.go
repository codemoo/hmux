package sshconfig

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

func TestRenderSecurityAndProxyJump(t *testing.T) {
	inventory := model.Inventory{
		SchemaVersion: 1, Revision: "test",
		Clients:      []model.Client{{ID: "home-mac", Role: "home"}},
		IdentityRefs: []model.IdentityRef{{ID: "key", Path: "~/.ssh/EXAMPLE_TEST_KEY"}},
		Hosts: []model.Host{
			{ID: "dmz", SSHAlias: "hmux-dmz", Address: "dmz.invalid", User: "u", Port: 22, IdentityRef: "key"},
			{ID: "home", SSHAlias: "hmux-home", Address: "home.invalid", User: "u", Port: 22, IdentityRef: "key", ProxyJump: "dmz"},
		},
		Profiles: []model.Profile{{ID: "shell", Label: "Shell", DefaultDirectory: "~", Command: []string{"sh"}}},
	}
	data, err := Render(inventory)
	if err != nil {
		t.Fatal(err)
	}
	text := string(data)
	for _, expected := range []string{
		"Host hmux-home", "ProxyJump hmux-dmz", "ForwardAgent no", "IdentitiesOnly yes", "StrictHostKeyChecking ask",
	} {
		if !strings.Contains(text, expected) {
			t.Errorf("missing %q", expected)
		}
	}
	for _, forbidden := range []string{"StrictHostKeyChecking no", "UserKnownHostsFile /dev/null", "RemoteCommand"} {
		if strings.Contains(text, forbidden) {
			t.Errorf("contains forbidden %q", forbidden)
		}
	}
}

func TestOpenSSHFirstObtainedValueOrdering(t *testing.T) {
	ssh, err := exec.LookPath("ssh")
	if err != nil {
		t.Skip("ssh unavailable")
	}
	inventory := model.Inventory{
		SchemaVersion: 1, Revision: "test",
		Clients:      []model.Client{{ID: "home-mac", Role: "home"}},
		IdentityRefs: []model.IdentityRef{{ID: "key", Path: "~/.ssh/EXAMPLE_TEST_KEY"}},
		Hosts: []model.Host{{
			ID: "home", SSHAlias: "hmux-home", Address: "generated.invalid",
			User: "user", Port: 22, IdentityRef: "key",
		}},
		Profiles: []model.Profile{{ID: "shell", Label: "Shell", DefaultDirectory: "~", Command: []string{"sh"}}},
	}
	generated, err := Render(inventory)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	generatedPath := filepath.Join(dir, "generated.conf")
	if err := os.WriteFile(generatedPath, generated, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name string
		data string
		want string
	}{
		{
			name: "override before include wins",
			data: "Host hmux-home\n  HostName override.invalid\nInclude " + generatedPath + "\n",
			want: "hostname override.invalid",
		},
		{
			name: "generated before override wins",
			data: "Include " + generatedPath + "\nHost hmux-home\n  HostName override.invalid\n",
			want: "hostname generated.invalid",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			configPath := filepath.Join(dir, strings.ReplaceAll(test.name, " ", "-")+".conf")
			if err := os.WriteFile(configPath, []byte(test.data), 0o600); err != nil {
				t.Fatal(err)
			}
			output, err := exec.Command(ssh, "-G", "-F", configPath, "hmux-home").Output()
			if err != nil {
				t.Fatal(err)
			}
			if !strings.Contains(string(output), test.want+"\n") {
				t.Fatalf("effective config does not contain %q", test.want)
			}
		})
	}
}

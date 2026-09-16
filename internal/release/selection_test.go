package release

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestIsNewerVersionRejectsDowngradesAndDevelopmentBuilds(t *testing.T) {
	cases := []struct {
		candidate string
		current   string
		want      bool
	}{
		{"0.1.28", "0.1.27", true},
		{"0.2.0", "0.1.99", true},
		{"0.1.27", "0.1.27", false},
		{"0.1.26", "0.1.27", false},
		{"0.1.28-rc1", "0.1.28", false},
		{"0.1.28", "0.1.28-rc1", true},
		{"0.1.28-rc.10", "0.1.28-rc.2", true},
		{"0.1.28-alpha.1", "0.1.28-alpha.beta", false},
		{"0.1.28+build.9", "0.1.28+build.1", false},
		{"0.1.29", "dev", false},
	}
	for _, item := range cases {
		if got := IsNewerVersion(item.candidate, item.current); got != item.want {
			t.Errorf("IsNewerVersion(%q, %q)=%v want %v", item.candidate, item.current, got, item.want)
		}
	}
}

func TestAppUpdateCheckStateIsPrivateBoundedAndRateLimited(t *testing.T) {
	cache := t.TempDir()
	now := time.Unix(1800000000, 0)
	if !AppUpdateCheckDue(cache, now, time.Hour) {
		t.Fatal("missing check state was not due")
	}
	if err := RecordAppUpdateCheck(cache, now); err != nil {
		t.Fatal(err)
	}
	if AppUpdateCheckDue(cache, now.Add(59*time.Minute), time.Hour) {
		t.Fatal("update check ignored its interval")
	}
	if !AppUpdateCheckDue(cache, now.Add(time.Hour), time.Hour) {
		t.Fatal("expired update check was not due")
	}
	path := filepath.Join(cache, updateCheckStateName)
	if err := os.Chmod(path, 0o622); err != nil {
		t.Fatal(err)
	}
	if !AppUpdateCheckDue(cache, now.Add(time.Minute), time.Hour) {
		t.Fatal("unsafe update state suppressed a check")
	}
}

func TestAgentUpdateCheckUsesIndependentPrivateState(t *testing.T) {
	state := t.TempDir()
	now := time.Unix(1800000000, 0)
	if err := RecordAppUpdateCheck(state, now); err != nil {
		t.Fatal(err)
	}
	if !AgentUpdateCheckDue(state, now, time.Hour) {
		t.Fatal("app update state suppressed the independent agent check")
	}
	if err := RecordAgentUpdateCheck(state, now); err != nil {
		t.Fatal(err)
	}
	if AgentUpdateCheckDue(state, now.Add(30*time.Minute), time.Hour) {
		t.Fatal("agent update check ignored its interval")
	}
	info, err := os.Stat(filepath.Join(state, agentUpdateCheckStateName))
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("agent update state mode=%v", info.Mode().Perm())
	}
}

package catalog

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

type rustProcessFixture struct {
	Name          string `json:"name"`
	Rows          string `json:"rows"`
	Pane          int    `json:"pane"`
	ProviderPID   int    `json:"provider_pid"`
	Status        string `json:"status"`
	CandidatePID  int    `json:"candidate_pid"`
	Process       string `json:"process"`
	State         string `json:"state"`
	Wrappers      []int  `json:"wrappers"`
	WrapperStatus string `json:"wrapper_status"`
}

func TestRustProcessOracle(t *testing.T) {
	fixtures := []rustProcessFixture{
		{Name: "shell", Rows: "1 0 S 0 zsh\n2 1 R+ 2.4 python", Pane: 1},
		{Name: "foreground", Rows: "1 0 S 0 zsh\n2 1 S 0 codex\n3 1 S+ 0 claude", Pane: 1},
		{Name: "ambiguous", Rows: "1 0 S 0 zsh\n2 1 S+ 0 codex\n3 1 S+ 0 claude", Pane: 1},
		{Name: "nested", Rows: "1 0 S 0 zsh\n2 1 S+ 0 codex\n3 2 R+ 2 claude", Pane: 1},
		{Name: "wrapper", Rows: "1 0 S 0 zsh\n2 1 S+ 0 codex\n3 2 S 0 node\n4 3 S 0 sh", Pane: 1},
		{Name: "wrapper-branch", Rows: "1 0 S 0 zsh\n2 1 S+ 0 codex\n3 2 S 0 node\n4 2 S 0 sh", Pane: 1},
		{Name: "claude-version", Rows: "1 0 S 0 zsh\n2 1 S+ 0 /synthetic/.local/share/claude/versions/2.1.263", Pane: 1},
		{Name: "missing", Rows: "1 0 S 0 zsh", Pane: 10},
	}
	status := func(value sessionBindingStatus) string {
		switch value {
		case sessionBindingReady:
			return "ready"
		case sessionBindingAmbiguous:
			return "ambiguous"
		default:
			return "unavailable"
		}
	}
	for i := range fixtures {
		f := &fixtures[i]
		nodes, err := parseProcessTable([]byte(f.Rows))
		if err != nil {
			t.Fatal(err)
		}
		children := processChildren(nodes)
		pid, kind := nearestSessionProvider(nodes, children, f.Pane)
		f.ProviderPID = pid
		f.Status = status(kind)
		if candidate, ok := selectProcessCandidate(nodes, children, f.Pane); ok {
			f.CandidatePID = candidate.Node.PID
			f.Process = candidate.Node.Process
			f.State = inferAgentState(candidate.Node)
		}
		f.Wrappers = []int{}
		f.WrapperStatus = "unavailable"
		if pid > 0 {
			chain, kind := sessionWrapperChain(nodes, children, pid)
			f.Wrappers = append(f.Wrappers, chain...)
			f.WrapperStatus = status(kind)
		}
	}
	raw, err := json.MarshalIndent(fixtures, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "catalog-v1", "go-process.json")
	if os.Getenv("UPDATE_HMUX_RUST_PROCESS_FIXTURE") == "1" {
		if err := os.WriteFile(path, raw, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go process graph changed; review synthetic oracle")
	}
}

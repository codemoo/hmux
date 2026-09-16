package catalog

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"unicode"
	"unicode/utf8"
)

// ResumeReference stays on Home. It contains only the exact provider identity
// and configuration location; never a prompt, transcript, command line or token.
type ResumeReference struct {
	Provider  string `json:"provider"`
	SessionID string `json:"session_id"`
	ConfigDir string `json:"config_dir"`
}

func (r ResumeReference) Validate() error {
	if (r.Provider != "codex" && r.Provider != "claude") || !safeSessionTokenPattern.MatchString(r.SessionID) {
		return errors.New("invalid provider resume identity")
	}
	if !utf8.ValidString(r.ConfigDir) || !filepath.IsAbs(r.ConfigDir) || filepath.Clean(r.ConfigDir) != r.ConfigDir || len(r.ConfigDir) > 4096 || strings.IndexFunc(r.ConfigDir, unicode.IsControl) >= 0 {
		return errors.New("invalid provider configuration directory")
	}
	return nil
}

// ResolveResumeReferences uses the same authority as catalog and conversation.
// A second pass excludes provider replacement while the checkpoint is collected.
func ResolveResumeReferences(ctx context.Context, panes []int) (map[int]ResumeReference, error) {
	if len(panes) > maximumPanePIDs {
		return nil, errors.New("pane process count exceeds limit")
	}
	s := systemProcessInspector{}
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}
	nodes, err := s.processSnapshot(ctx)
	if err != nil {
		return nil, err
	}
	first := s.resolveSessionBindings(ctx, nodes, panes, home)
	nodes, err = s.processSnapshot(ctx)
	if err != nil {
		return nil, err
	}
	second := s.resolveSessionBindings(ctx, nodes, panes, home)
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return stableResumeReferences(first, second), nil
}

func stableResumeReferences(first, second map[int]sessionBinding) map[int]ResumeReference {
	out := map[int]ResumeReference{}
	for pane, a := range first {
		b := second[pane]
		if a.status != sessionBindingReady || b.status != sessionBindingReady || a.provider != b.provider || a.providerPID != b.providerPID || a.filePID != b.filePID || a.path != b.path || a.root != b.root || a.recordID != b.recordID {
			continue
		}
		ref := ResumeReference{Provider: a.provider, SessionID: a.recordID, ConfigDir: filepath.Dir(a.root)}
		if ref.Validate() == nil {
			out[pane] = ref
		}
	}
	return out
}

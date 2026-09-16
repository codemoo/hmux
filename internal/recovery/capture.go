package recovery

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"sort"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sessionstate"
)

const recoverySeparator = "|:hmux-recovery-v1:|"

var paneFormat = strings.Join([]string{
	"#{session_id}", "#{window_id}", "#{window_index}", "#{window_name}",
	"#{window_layout}", "#{window_active}", "#{pane_id}", "#{pane_index}",
	"#{pane_active}", "#{pane_current_path}", "#{pane_pid}",
}, recoverySeparator)

type captureWindow struct {
	value savedWindow
	id    string
}

func (s Store) capture(ctx context.Context) (snapshot, error) {
	runner := s.runner()
	value, err := catalog.ReadBasic(ctx, runner)
	if err != nil {
		return snapshot{}, err
	}
	metadata := sessionstate.Store{StateDir: s.StateDir}
	if err := metadata.Apply(&value); err != nil {
		return snapshot{}, fmt.Errorf("apply recovery metadata: %w", err)
	}
	if err := metadata.ApplyVisibility(&value); err != nil {
		return snapshot{}, fmt.Errorf("apply recovery visibility: %w", err)
	}
	if len(value.Sessions) == 0 {
		return snapshot{Sessions: []savedSession{}}, nil
	}
	if len(value.Sessions) > maximumSessions {
		return snapshot{}, errors.New("recovery session count exceeds limit")
	}

	raw, err := runner.Output(ctx, "list-panes", "-a", "-F", paneFormat)
	if err != nil {
		return snapshot{}, fmt.Errorf("tmux list-panes for recovery: %w", err)
	}
	if len(raw) > maximumStateBytes*2 {
		return snapshot{}, errors.New("tmux recovery pane output exceeds limit")
	}

	sessions := make(map[string]*savedSession, len(value.Sessions))
	windows := make(map[string]map[string]*captureWindow, len(value.Sessions))
	for _, item := range value.Sessions {
		saved := &savedSession{
			Identity: model.SessionIdentity{ID: item.ID, CreatedAt: item.CreatedAt},
			Name:     item.Name, Alias: item.Alias, Hidden: item.Hidden,
			Profile: item.Profile, Label: item.Label, Tags: append([]string(nil), item.Tags...),
		}
		sessions[item.ID] = saved
		windows[item.ID] = map[string]*captureWindow{}
	}

	seenPanes := map[string]bool{}
	pidToPane := map[int]*savedPane{}
	lines := strings.Split(strings.TrimSuffix(string(raw), "\n"), "\n")
	if len(lines) > maximumPanes {
		return snapshot{}, errors.New("recovery pane count exceeds limit")
	}
	for _, line := range lines {
		fields := strings.Split(strings.TrimSuffix(line, "\r"), recoverySeparator)
		if len(fields) != 11 {
			return snapshot{}, fmt.Errorf("malformed recovery pane row: got %d fields", len(fields))
		}
		session := sessions[fields[0]]
		if session == nil {
			continue // native app view or a session that changed during collection
		}
		if !validTmuxID(fields[1], '@') || !validTmuxID(fields[6], '%') || seenPanes[fields[6]] {
			return snapshot{}, errors.New("invalid or duplicate tmux recovery pane identity")
		}
		seenPanes[fields[6]] = true
		windowIndex, err := parseNonnegative(fields[2], "window_index")
		if err != nil {
			return snapshot{}, err
		}
		paneIndex, err := parseNonnegative(fields[7], "pane_index")
		if err != nil {
			return snapshot{}, err
		}
		activeWindow, err := parseFlag(fields[5], "window_active")
		if err != nil {
			return snapshot{}, err
		}
		activePane, err := parseFlag(fields[8], "pane_active")
		if err != nil {
			return snapshot{}, err
		}
		pid, err := strconv.Atoi(fields[10])
		if err != nil || pid < 1 || pid > 1<<30 {
			return snapshot{}, errors.New("invalid recovery pane pid")
		}
		byWindow := windows[fields[0]]
		window := byWindow[fields[1]]
		if window == nil {
			window = &captureWindow{id: fields[1], value: savedWindow{
				Index: windowIndex, Name: fields[3], Layout: fields[4], Active: activeWindow,
			}}
			byWindow[fields[1]] = window
		} else if window.value.Index != windowIndex || window.value.Name != fields[3] || window.value.Layout != fields[4] || window.value.Active != activeWindow {
			return snapshot{}, errors.New("inconsistent recovery window metadata")
		}
		pane := savedPane{Index: paneIndex, Cwd: fields[9], Active: activePane}
		window.value.Panes = append(window.value.Panes, pane)
		pidToPane[pid] = &window.value.Panes[len(window.value.Panes)-1]
	}

	// Appending may reallocate a window's pane slice, so rebuild PID pointers in
	// stable row order before binding. This also catches duplicate PIDs.
	pidToPane = map[int]*savedPane{}
	var pids []int
	for _, line := range lines {
		fields := strings.Split(strings.TrimSuffix(line, "\r"), recoverySeparator)
		if len(fields) != 11 || sessions[fields[0]] == nil {
			continue
		}
		pid, _ := strconv.Atoi(fields[10])
		paneIndex, _ := strconv.Atoi(fields[7])
		window := windows[fields[0]][fields[1]]
		for index := range window.value.Panes {
			if window.value.Panes[index].Index == paneIndex {
				if pidToPane[pid] != nil {
					return snapshot{}, errors.New("duplicate recovery pane pid")
				}
				pidToPane[pid] = &window.value.Panes[index]
				pids = append(pids, pid)
				break
			}
		}
	}
	if s.Bind == nil {
		return snapshot{}, errors.New("recovery resume resolver is not configured")
	}
	bindings, err := s.Bind(ctx, pids)
	if err != nil {
		return snapshot{}, fmt.Errorf("resolve recovery resume references: %w", err)
	}
	for pid, reference := range bindings {
		pane := pidToPane[pid]
		if pane == nil {
			return snapshot{}, errors.New("resume resolver returned an unknown pane pid")
		}
		if err := reference.Validate(); err != nil {
			return snapshot{}, err
		}
		copy := reference
		pane.Resume = &copy
	}

	// Verify the tmux lifetime and topology around provider discovery. A switched
	// window/pane, recycled session or replaced PID invalidates the whole capture.
	again, err := catalog.ReadBasic(ctx, s.runner())
	if err != nil {
		return snapshot{}, err
	}
	if len(again.Sessions) != len(value.Sessions) {
		return snapshot{}, catalog.ErrSessionChanged
	}
	for _, item := range again.Sessions {
		old := sessions[item.ID]
		if old == nil || old.Identity.CreatedAt != item.CreatedAt || old.Name != item.Name {
			return snapshot{}, catalog.ErrSessionChanged
		}
	}
	rawAgain, err := s.runner().Output(ctx, "list-panes", "-a", "-F", paneFormat)
	if err != nil {
		return snapshot{}, err
	}
	if !bytes.Equal(raw, rawAgain) {
		return snapshot{}, catalog.ErrSessionChanged
	}

	result := snapshot{Sessions: make([]savedSession, 0, len(sessions))}
	for _, item := range value.Sessions {
		session := sessions[item.ID]
		for _, window := range windows[item.ID] {
			sort.Slice(window.value.Panes, func(i, j int) bool { return window.value.Panes[i].Index < window.value.Panes[j].Index })
			session.Windows = append(session.Windows, window.value)
		}
		sort.Slice(session.Windows, func(i, j int) bool { return session.Windows[i].Index < session.Windows[j].Index })
		result.Sessions = append(result.Sessions, *session)
	}
	sort.Slice(result.Sessions, func(i, j int) bool { return result.Sessions[i].Identity.ID < result.Sessions[j].Identity.ID })
	if err := validateSnapshot(result); err != nil {
		return snapshot{}, err
	}
	return result, nil
}

func parseFlag(raw, field string) (bool, error) {
	if raw == "0" {
		return false, nil
	}
	if raw == "1" {
		return true, nil
	}
	return false, fmt.Errorf("invalid %s", field)
}

func validTmuxID(value string, prefix byte) bool {
	if len(value) < 2 || len(value) > 14 || value[0] != prefix {
		return false
	}
	for _, r := range value[1:] {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}

func mergeMissingResumeReferences(current *snapshot, previous snapshot) {
	type paneKey struct {
		session string
		window  int
		pane    int
	}
	old := map[paneKey]*catalog.ResumeReference{}
	for si := range previous.Sessions {
		session := &previous.Sessions[si]
		for wi := range session.Windows {
			window := &session.Windows[wi]
			for pi := range window.Panes {
				pane := &window.Panes[pi]
				if pane.Resume != nil {
					key := paneKey{identityKey(session.Identity), window.Index, pane.Index}
					old[key] = pane.Resume
				}
			}
		}
	}
	for si := range current.Sessions {
		session := &current.Sessions[si]
		for wi := range session.Windows {
			window := &session.Windows[wi]
			for pi := range window.Panes {
				pane := &window.Panes[pi]
				if pane.Resume != nil {
					continue // a stable current binding always wins, including /new
				}
				key := paneKey{identityKey(session.Identity), window.Index, pane.Index}
				if reference := old[key]; reference != nil {
					copy := *reference
					pane.Resume = &copy
				}
			}
		}
	}
}

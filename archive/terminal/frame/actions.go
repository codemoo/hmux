package frame

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/archive/terminal/ui"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sessionstate"
	"github.com/codemoo/hmux/internal/tabstate"
	"github.com/codemoo/hmux/internal/workflow"
)

type actionResult uint8

const (
	actionStay actionResult = iota
	actionClose
	actionLeave
	actionQuit
)

func Status(ctx context.Context, stateDir, launcherID string) (string, error) {
	frameState, sessions, err := frameSessions(ctx, stateDir, launcherID)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return "", nil
		}
		return "", err
	}
	return ui.FormatTabs(sessions, frameState.CurrentID, 9), nil
}

func Action(
	ctx context.Context, stateDir, launcherID, action string,
) (actionResult, error) {
	store := tabstate.Store{StateDir: stateDir}
	frameState, sessions, err := frameSessions(ctx, stateDir, launcherID)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return actionStay, nil
		}
		return actionStay, err
	}
	switch action {
	case "list":
		return actionLeave, nil
	case "quit":
		return actionQuit, nil
	case "close":
		remaining, nextID, err := tabsAfterClose(sessions, frameState.CurrentID)
		if err != nil {
			return actionStay, err
		}
		remainingIDs := sessionIDs(remaining)
		if nextID == "" {
			if err := store.UpdateFrame(
				launcherID, frameState.ClientName, remainingIDs, "",
			); err != nil {
				return actionStay, err
			}
			return actionClose, nil
		}
		if err := store.UpdateFrame(
			launcherID, frameState.ClientName, remainingIDs, nextID,
		); err != nil {
			return actionStay, err
		}
		syncOuterStatus(ctx, stateDir, launcherID, frameState.ControlName)
		if err := switchTarget(ctx, frameState.ClientName, nextID); err != nil {
			_ = store.UpdateFrame(
				launcherID, frameState.ClientName,
				frameState.Sessions, frameState.CurrentID,
			)
			syncOuterStatus(ctx, stateDir, launcherID, frameState.ControlName)
			return actionStay, err
		}
		return actionStay, nil
	default:
		number, err := strconv.Atoi(action)
		if err != nil || number < 1 || number > 9 {
			return actionStay, errors.New(
				"frame action must be list, close, quit or a tab number",
			)
		}
		if number > len(sessions) {
			return actionStay, nil
		}
		session := sessions[number-1]
		if session.ID == frameState.CurrentID {
			return actionStay, nil
		}
		if err := store.UpdateFrame(
			launcherID, frameState.ClientName, frameState.Sessions, session.ID,
		); err != nil {
			return actionStay, err
		}
		syncOuterStatus(ctx, stateDir, launcherID, frameState.ControlName)
		if err := switchTarget(ctx, frameState.ClientName, session.ID); err != nil {
			_ = store.UpdateFrame(
				launcherID, frameState.ClientName,
				frameState.Sessions, frameState.CurrentID,
			)
			syncOuterStatus(ctx, stateDir, launcherID, frameState.ControlName)
			return actionStay, err
		}
		return actionStay, nil
	}
}

// Click maps only the user-defined ranges emitted by the disposable frame
// status line. Tab clicks reuse the same validated action path as Cmd-1..9.
// The list range publishes the existing private frontend completion status so
// the parent closes the disposable client before an [exited] pane can render.
func Click(
	ctx context.Context, stateDir, launcherID, statusFile, mouseRange string,
) error {
	if err := validateFrameStatusPath(stateDir, launcherID, statusFile); err != nil {
		return err
	}
	switch mouseRange {
	case "list":
		if _, ready := readReadyStatus(statusFile); ready {
			return nil
		}
		return writeStatus(statusFile, 0)
	case "tab1", "tab2", "tab3", "tab4", "tab5",
		"tab6", "tab7", "tab8", "tab9":
		result, err := Action(
			ctx, stateDir, launcherID, strings.TrimPrefix(mouseRange, "tab"),
		)
		if err != nil {
			return err
		}
		if result != actionStay {
			return errors.New("tab click produced an invalid frame result")
		}
		return nil
	default:
		return nil
	}
}

func frameSessions(
	ctx context.Context, stateDir, launcherID string,
) (tabstate.FrameState, []model.Session, error) {
	store := tabstate.Store{StateDir: stateDir}
	frameState, err := store.Frame(launcherID)
	if err != nil {
		return tabstate.FrameState{}, nil, err
	}
	value, err := catalog.ReadBasic(ctx, targetTmuxRunner())
	if err != nil {
		return tabstate.FrameState{}, nil, err
	}
	if err := (sessionstate.Store{StateDir: stateDir}).Apply(&value); err != nil {
		return tabstate.FrameState{}, nil, err
	}
	// Workflow state is optional presentation metadata. Frame navigation and
	// close/switch actions must remain available if that overlay is corrupt.
	_ = (workflow.Store{StateDir: stateDir}).Apply(&value)
	byID := make(map[string]model.Session, len(value.Sessions))
	for _, session := range value.Sessions {
		byID[session.ID] = session
	}
	sessions := make([]model.Session, 0, len(frameState.Sessions))
	for _, id := range frameState.Sessions {
		if session, exists := byID[id]; exists {
			sessions = append(sessions, session)
		}
	}
	return frameState, sessions, nil
}

func switchTarget(ctx context.Context, clientName, sessionID string) error {
	return catalog.SwitchClient(ctx, targetTmuxRunner(), clientName, sessionID)
}

func targetTmuxRunner() catalog.TmuxRunner {
	return catalog.TmuxRunner{Env: withoutTmuxEnvironment(os.Environ())}
}

func syncOuterStatus(
	ctx context.Context,
	stateDir, launcherID, controlName string,
) {
	frameState, sessions, err := frameSessions(ctx, stateDir, launcherID)
	if err != nil {
		return
	}
	routing := os.Getenv(envOuterTMUX)
	if routing == "" {
		routing = os.Getenv("TMUX")
	}
	if validateTMUXRouting(routing) != nil ||
		tabstate.ValidateClientName(controlName) != nil {
		return
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return
	}
	environment := append(withoutTmuxEnvironment(os.Environ()), "TMUX="+routing)
	args := make([]string, 0, 9*5+6)
	for index := 0; index < 9; index++ {
		value := ""
		if index < len(sessions) {
			value = ui.FormatTab(sessions[index], index+1, frameState.CurrentID)
			if index > 0 {
				value = "#[bg=#100f0f] " + value
			}
		}
		if index > 0 {
			args = append(args, ";")
		}
		args = append(
			args,
			"set-option", "-g",
			fmt.Sprintf("@hmux_frame_tab%d", index+1),
			value,
		)
	}
	args = append(
		args, ";", "refresh-client", "-t", controlName, "-S",
	)
	command := exec.CommandContext(ctx, tmuxPath, args...)
	command.Env = environment
	_ = command.Run()
}

func tabsAfterClose(
	sessions []model.Session, currentID string,
) (remaining []model.Session, nextID string, err error) {
	currentIndex := -1
	for index := range sessions {
		if sessions[index].ID == currentID {
			currentIndex = index
			break
		}
	}
	if currentIndex < 0 {
		return nil, "", errors.New("current session is not an open tab")
	}
	remaining = append(remaining, sessions[:currentIndex]...)
	remaining = append(remaining, sessions[currentIndex+1:]...)
	if len(remaining) == 0 {
		return remaining, "", nil
	}
	nextIndex := currentIndex
	if nextIndex >= len(remaining) {
		nextIndex = len(remaining) - 1
	}
	return remaining, remaining[nextIndex].ID, nil
}

func sessionIDs(sessions []model.Session) []string {
	ids := make([]string, 0, len(sessions))
	for _, session := range sessions {
		ids = append(ids, session.ID)
	}
	return ids
}

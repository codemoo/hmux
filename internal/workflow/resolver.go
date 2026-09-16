package workflow

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/safeexec"
)

var panePattern = regexp.MustCompile(`^%[0-9]{1,12}$`)

// ResolveBinding maps the inherited tmux pane to a stable session identity.
// display-message is read-only and receives every value as a separate argv
// element. Hooks outside tmux return an error and are intentionally ignored by
// the fail-open command wrapper.
func ResolveBinding(ctx context.Context) (Binding, error) {
	if id := os.Getenv("HMUX_TMUX_SESSION_ID"); id != "" {
		created, err := strconv.ParseInt(os.Getenv("HMUX_TMUX_SESSION_CREATED_AT"), 10, 64)
		if err != nil {
			return Binding{}, errors.New("invalid HMUX tmux creation time override")
		}
		binding := Binding{SessionID: id, CreatedAt: created}
		return binding, validateBinding(binding)
	}
	pane := os.Getenv("TMUX_PANE")
	if !panePattern.MatchString(pane) {
		return Binding{}, errors.New("hook is not running in a validated tmux pane")
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return Binding{}, err
	}
	output, err := safeexec.Output(exec.CommandContext(
		ctx, tmuxPath, "display-message", "-p", "-t", pane,
		"#{session_id}\t#{session_created}",
	), 4096)
	if err != nil {
		return Binding{}, fmt.Errorf("resolve tmux workflow binding: %w", err)
	}
	fields := strings.Split(strings.TrimSpace(string(output)), "\t")
	if len(fields) != 2 {
		return Binding{}, errors.New("tmux workflow binding has an invalid shape")
	}
	created, err := strconv.ParseInt(fields[1], 10, 64)
	if err != nil {
		return Binding{}, errors.New("tmux workflow creation time is invalid")
	}
	binding := Binding{SessionID: fields[0], CreatedAt: created}
	return binding, validateBinding(binding)
}

package client

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/creack/pty"
)

// AppViewPTY owns only its disposable grouped view and attached client. Home's
// original session and provider processes survive Close and context cancellation.
type AppViewPTY struct {
	*os.File
	Done   <-chan struct{}
	cancel context.CancelFunc
	once   sync.Once
	runner catalog.Runner
	name   string
	pid    int
}

func (p *AppViewPTY) Close() error {
	var err error
	p.once.Do(func() { err = p.File.Close(); p.cancel() })
	return err
}
func (p *AppViewPTY) Resize(cols, rows uint16) error {
	if cols < 2 || cols > 500 || rows < 2 || rows > 250 {
		return errors.New("invalid terminal size")
	}
	return pty.Setsize(p.File, &pty.Winsize{Cols: cols, Rows: rows})
}

// Refresh asks the foreground program in this view's active pane to redraw,
// then repaints its existing tmux client. No input or layout mutation is needed.
func (p *AppViewPTY) Refresh(ctx context.Context) error {
	return p.refresh(ctx, foregroundPaneGroup, func(group int) error { return syscall.Kill(-group, syscall.SIGWINCH) })
}

const refreshClientFormat = "#{client_pid} #{client_tty} #{pane_id} #{pane_tty} #{pane_pid}"

func (p *AppViewPTY) refresh(ctx context.Context, foreground func(context.Context, int, string) (int, error), signalGroup func(int) error) error {
	select {
	case <-p.Done:
		return errors.New("terminal closed")
	default:
	}
	if p.runner == nil || p.name == "" || p.pid < 1 {
		return errors.New("invalid terminal view")
	}
	bounded, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	row, err := p.refreshTarget(bounded)
	if err != nil {
		return err
	}
	panePID, _ := strconv.Atoi(row[4])
	group, err := foreground(bounded, panePID, row[3])
	if err != nil {
		return err
	}
	if group <= 1 {
		return errors.New("invalid foreground group")
	}
	// A tab/window switch or pane replacement during discovery must not target
	// the former program. Tmux alone supplies all process and device identities.
	current, err := p.refreshTarget(bounded)
	if err != nil {
		return err
	}
	if strings.Join(current, " ") != strings.Join(row, " ") {
		return errors.New("terminal pane changed")
	}
	if err = signalGroup(group); err != nil {
		return err
	}
	_, err = p.runner.Output(bounded, "refresh-client", "-t", row[1])
	return err
}

func (p *AppViewPTY) refreshTarget(ctx context.Context) ([]string, error) {
	out, err := p.runner.Output(ctx, "list-clients", "-t", p.name, "-F", refreshClientFormat)
	if err != nil {
		return nil, err
	}
	for _, line := range strings.Split(string(out), "\n") {
		fields := strings.Fields(line)
		if len(fields) != 5 || fields[0] != strconv.Itoa(p.pid) {
			continue
		}
		panePID, err := strconv.Atoi(fields[4])
		if !validRefreshTTY(fields[1]) || !validRefreshTTY(fields[3]) || len(fields[2]) < 2 || fields[2][0] != '%' || err != nil || panePID <= 1 {
			return nil, errors.New("invalid terminal target")
		}
		if _, err = strconv.ParseUint(fields[2][1:], 10, 64); err != nil {
			return nil, errors.New("invalid terminal pane")
		}
		return fields, nil
	}
	return nil, errors.New("terminal client unavailable")
}
func validRefreshTTY(tty string) bool {
	if !strings.HasPrefix(tty, "/dev/") || len(tty) > 128 {
		return false
	}
	for _, c := range tty[5:] {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '/') {
			return false
		}
	}
	return len(tty) > 5
}

// TIOCGPGRP on another session's slave tty is not portable. ps exposes the
// controlling terminal's foreground group even when pane_pid is its shell.
func foregroundPaneGroup(ctx context.Context, pid int, tty string) (int, error) {
	if pid <= 1 || !validRefreshTTY(tty) {
		return 0, errors.New("invalid pane process")
	}
	out, err := exec.CommandContext(ctx, "/bin/ps", "-p", strconv.Itoa(pid), "-o", "pid=,tpgid=,tty=").Output()
	if err != nil {
		return 0, err
	}
	fields := strings.Fields(string(out))
	if len(fields) != 3 || fields[0] != strconv.Itoa(pid) {
		return 0, errors.New("pane process unavailable")
	}
	// macOS ps prints ttys000 as s000; Linux prints pts/0 unchanged.
	expected := strings.TrimPrefix(tty, "/dev/")
	if fields[2] != expected && fields[2] != strings.TrimPrefix(expected, "tty") {
		return 0, errors.New("pane terminal changed")
	}
	group, err := strconv.Atoi(fields[1])
	if err != nil || group <= 1 {
		return 0, errors.New("invalid pane foreground group")
	}
	return group, nil
}
func OpenAppViewPTY(ctx context.Context, cfg config.ClientConfig, identity model.SessionIdentity, cols, rows uint16) (*AppViewPTY, error) {
	if cfg.Role != "home" || model.ValidateSessionID(identity.ID) != nil || identity.CreatedAt < 1 || cols < 2 || cols > 500 || rows < 2 || rows > 250 {
		return nil, errors.New("invalid Home terminal request")
	}
	runner := catalog.TmuxRunner{}
	name, err := newAppViewName()
	if err != nil {
		return nil, err
	}
	setup, cancel := context.WithTimeout(ctx, 15*time.Second)
	err = createExpectedAppView(setup, runner, identity.ID, identity.CreatedAt, name)
	cancel()
	if err != nil {
		return nil, err
	}
	// A tmux-owned hook also removes an attached web view if this connector is
	// killed abruptly. The random name and marker scope cleanup to this view only.
	hookCtx, hookCancel := context.WithTimeout(ctx, 5*time.Second)
	hook := "if-shell -F -t " + name + " '#{&&:#{==:#{@hmux_app_view},1},#{==:#{session_attached},0}}' 'kill-session -t " + name + "'"
	_, err = runner.Output(hookCtx, "set-hook", "-t", name, "client-detached", hook)
	hookCancel()
	if err != nil {
		cleanupAppView(runner, name)
		return nil, err
	}
	args, err := catalog.AppViewAttachArgs(name, true)
	if err != nil {
		cleanupAppView(runner, name)
		return nil, err
	}
	path, err := catalog.TmuxPath()
	if err != nil {
		cleanupAppView(runner, name)
		return nil, err
	}
	childCtx, stop := context.WithCancel(ctx)
	command := exec.CommandContext(childCtx, path, args...)
	for _, entry := range os.Environ() {
		keep := true
		for _, key := range []string{"TMUX=", "TMUX_PANE=", "TERM=", "COLORTERM="} {
			if len(entry) >= len(key) && entry[:len(key)] == key {
				keep = false
			}
		}
		if keep {
			command.Env = append(command.Env, entry)
		}
	}
	command.Env = append(command.Env, "TERM=xterm-256color", "COLORTERM=truecolor")
	terminal, err := pty.StartWithSize(command, &pty.Winsize{Cols: cols, Rows: rows})
	if err != nil {
		stop()
		cleanupAppView(runner, name)
		return nil, err
	}
	done := make(chan struct{})
	view := &AppViewPTY{File: terminal, Done: done, cancel: stop, runner: runner, name: name, pid: command.Process.Pid}
	go func() { _ = command.Wait(); _ = view.Close(); cleanupAppView(runner, name); close(done) }()
	return view, nil
}

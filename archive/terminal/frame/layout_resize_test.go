package frame

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/creack/pty"
)

func TestFramedPaneLayoutTracksClientResize(t *testing.T) {
	if os.Getenv("HMUX_RUN_TMUX_RESIZE_TEST") != "1" {
		t.Skip("isolated tmux resize integration is opt-in")
	}
	tmuxPath, err := exec.LookPath("tmux")
	if err != nil {
		t.Skip("tmux is unavailable")
	}
	root := t.TempDir()
	socket := filepath.Join("/tmp", fmt.Sprintf(
		"hmux-e2e-frame-resize-%d-%d.sock", os.Getpid(), time.Now().UnixNano(),
	))
	t.Cleanup(func() { _ = os.Remove(socket) })
	logPath := filepath.Join(root, "sizes.log")
	observer := filepath.Join(root, "observe-size.sh")
	spacer := filepath.Join(root, "spacer.sh")
	if err := os.WriteFile(observer, []byte(
		"#!/bin/sh\nset -eu\nwhile :; do stty size >>\"$1\"; sleep 0.05; done\n",
	), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(spacer, []byte(
		"#!/bin/sh\ntrap 'exit 0' HUP INT TERM\nwhile :; do sleep 10; done\n",
	), 0o700); err != nil {
		t.Fatal(err)
	}
	_, source, _, _ := runtime.Caller(0)
	configPath := filepath.Join(filepath.Dir(source), "..", "..", "config", "frame-ui.tmux.conf")
	server := func(args ...string) *exec.Cmd {
		return exec.Command(tmuxPath, append([]string{"-S", socket}, args...)...)
	}
	const session = "hmux-e2e-frame-resize"
	attach := server(
		"-f", configPath, "new-session", "-s", session,
		shellQuote(observer)+" "+shellQuote(logPath),
	)
	attach.Env = append(os.Environ(), "TERM=xterm-256color")
	master, err := pty.StartWithSize(attach, &pty.Winsize{Rows: 24, Cols: 80})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = server("kill-server").Run() })
	t.Cleanup(func() {
		_ = master.Close()
		_ = attach.Process.Kill()
		_ = attach.Wait()
	})
	go func() { _, _ = io.Copy(io.Discard, master) }()
	waitForResizeTestClient(t, server)

	targetOutput, err := server("list-panes", "-t", session, "-F", "#{pane_id}").Output()
	if err != nil {
		t.Fatal(err)
	}
	targetPane := strings.TrimSpace(string(targetOutput))
	if !validPaneID(targetPane) {
		t.Fatalf("invalid target pane %q", targetPane)
	}
	spacerCommand := shellQuote(spacer)
	for _, args := range [][]string{
		{"split-window", "-d", "-v", "-l", "1", "-t", targetPane, spacerCommand},
		{"split-window", "-d", "-h", "-b", "-l", "1", "-t", targetPane, spacerCommand},
		{"split-window", "-d", "-h", "-l", "1", "-t", targetPane, spacerCommand},
	} {
		if output, err := server(args...).CombinedOutput(); err != nil {
			t.Fatalf("create isolated frame layout: %v: %s", err, output)
		}
	}
	if err := server("select-pane", "-t", targetPane, "-T", "LIVE SESSION").Run(); err != nil {
		t.Fatal(err)
	}

	time.Sleep(100 * time.Millisecond)

	beforeRows, beforeColumns := waitForResizeSample(t, logPath, 0, 0)
	if err := pty.Setsize(master, &pty.Winsize{Rows: 40, Cols: 120}); err != nil {
		t.Fatal(err)
	}
	afterRows, afterColumns := waitForResizeSample(
		t, logPath, beforeRows, beforeColumns,
	)
	if afterRows <= beforeRows || afterColumns <= beforeColumns {
		t.Fatalf(
			"framed target did not grow after client resize: %dx%d -> %dx%d",
			beforeColumns, beforeRows, afterColumns, afterRows,
		)
	}
}

func waitForResizeTestClient(t *testing.T, server func(...string) *exec.Cmd) {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		output, err := server("list-clients", "-F", "#{client_name}").Output()
		if err == nil && strings.TrimSpace(string(output)) != "" {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("isolated tmux client did not become ready")
}

func waitForResizeSample(t *testing.T, path string, oldRows, oldColumns int) (int, int) {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		file, err := os.Open(path)
		if err == nil {
			scanner := bufio.NewScanner(file)
			rows, columns := 0, 0
			for scanner.Scan() {
				fields := strings.Fields(scanner.Text())
				if len(fields) != 2 {
					continue
				}
				rows, _ = strconv.Atoi(fields[0])
				columns, _ = strconv.Atoi(fields[1])
			}
			_ = file.Close()
			if rows > 0 && columns > 0 && (rows != oldRows || columns != oldColumns) {
				return rows, columns
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("no resized frame sample after %dx%d", oldColumns, oldRows)
	return 0, 0
}

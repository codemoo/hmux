package client

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"bytes"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"reflect"
	"sync"
)

func TestWebAppViewWithIsolatedTmux(t *testing.T) {
	if os.Getenv("HMUX_RUN_WEB_TMUX_TEST") != "1" {
		t.Skip("isolated opt-in live tmux")
	}
	real, err := exec.LookPath("tmux")
	if err != nil {
		t.Skip("tmux unavailable")
	}
	socket := fmt.Sprintf("hmux-e2e-web-%d", os.Getpid())
	dir := t.TempDir()
	// Every command is pinned to a dedicated socket, and every temporary name is
	// rewritten to the required test prefix. No pre-existing user sessions are touched.
	quotedReal, _ := json.Marshal(real)
	quotedSocket, _ := json.Marshal(socket)
	wrapper := "#!/usr/bin/python3\nimport os,sys\nos.execv(" + string(quotedReal) + ", [\"tmux\",\"-L\"," + string(quotedSocket) + ",\"-f\",\"/dev/null\"]+[a.replace(\"hmux-app-view-\",\"hmux-e2e-view-\") for a in sys.argv[1:]])\n"
	if err := os.WriteFile(filepath.Join(dir, "tmux"), []byte(wrapper), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("TMUX", "")
	run := func(args ...string) []byte {
		t.Helper()
		c := exec.Command(filepath.Join(dir, "tmux"), args...)
		out, e := c.CombinedOutput()
		if e != nil {
			t.Fatalf("isolated tmux %s: %v %s", args[0], e, out)
		}
		return out
	}
	t.Cleanup(func() { _ = exec.Command(real, "-L", socket, "kill-server").Run() })
	// A test TUI only repairs its stale display when its foreground process
	// receives the normal terminal resize notification; no input is needed.
	fixture := filepath.Join(dir, "redraw.py")
	countPath := filepath.Join(dir, "redraw-count")
	if err := os.WriteFile(fixture, []byte(`import signal,time,sys,os
count=0
path=sys.argv[1]
def save():
 with open(path+'.next','w') as f: f.write(str(count))
 os.replace(path+'.next',path)
def redraw(*_):
 global count
 count+=1
 save()
 print('\x1b[2J\x1b[HREDRAW-'+str(count),flush=True)
signal.signal(signal.SIGWINCH,redraw)
save()
print('REDRAW-READY',flush=True)
while True: time.sleep(.1)
`), 0600); err != nil {
		t.Fatal(err)
	}
	out := strings.TrimSpace(string(run("new-session", "-d", "-s", "hmux-e2e-original", "-P", "-F", "#{session_id}:#{session_created}", "/usr/bin/python3", fixture, countPath)))
	parts := strings.Split(out, ":")
	created, _ := strconv.ParseInt(parts[1], 10, 64)
	id := model.SessionIdentity{ID: parts[0], CreatedAt: created}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	view, err := OpenAppViewPTY(ctx, config.DefaultClientConfig(), id, 80, 24)
	if err != nil {
		t.Fatal(err)
	}
	var outputMu sync.Mutex
	var output bytes.Buffer
	go func() {
		buf := make([]byte, 4096)
		for {
			n, err := view.Read(buf)
			if n > 0 {
				outputMu.Lock()
				output.Write(buf[:n])
				outputMu.Unlock()
			}
			if err != nil {
				return
			}
		}
	}()
	if err = view.Resize(100, 30); err != nil {
		t.Fatal(err)
	}

	// Wait for the disposable client to attach, then redraw without replacing it.
	deadline := time.Now().Add(3 * time.Second)
	for {
		err = view.Refresh(ctx)
		if err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal(err)
		}
		time.Sleep(20 * time.Millisecond)
	}
	// Drain size-change output before requiring a new application repaint.
	time.Sleep(150 * time.Millisecond)
	countBytes, err := os.ReadFile(countPath)
	if err != nil {
		t.Fatal(err)
	}
	previous, err := strconv.Atoi(string(countBytes))
	if err != nil {
		t.Fatal(err)
	}
	outputMu.Lock()
	output.Reset()
	outputMu.Unlock()
	if err = view.Refresh(ctx); err != nil {
		t.Fatal(err)
	}
	deadline = time.Now().Add(2 * time.Second)
	for {
		outputMu.Lock()
		got := strings.Contains(output.String(), fmt.Sprintf("REDRAW-%d", previous+1))
		outputMu.Unlock()
		if got {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("refresh did not repaint the running TUI")
		}
		time.Sleep(20 * time.Millisecond)
	}
	select {
	case <-view.Done:
		t.Fatal("refresh detached view")
	default:
	}
	_ = view.Close()
	select {
	case <-view.Done:
	case <-time.After(8 * time.Second):
		t.Fatal("view did not release")
	}
	names := strings.TrimSpace(string(run("list-sessions", "-F", "#{session_name}")))
	if names != "hmux-e2e-original" {
		t.Fatalf("unexpected survivors: %s", names)
	}
	stale := id
	stale.CreatedAt++
	if p, err := OpenAppViewPTY(ctx, config.DefaultClientConfig(), stale, 80, 24); err == nil {
		p.Close()
		t.Fatal("stale identity attached")
	}
	names = strings.TrimSpace(string(run("list-sessions", "-F", "#{session_name}")))
	if names != "hmux-e2e-original" {
		t.Fatal("stale open changed original")
	}
}

type refreshRunner struct {
	listing string
	calls   [][]string
}

func (r *refreshRunner) Output(_ context.Context, args ...string) ([]byte, error) {
	r.calls = append(r.calls, args)
	if args[0] == "list-clients" {
		return []byte(r.listing), nil
	}
	return nil, nil
}
func TestAppViewRefreshTargetsOnlyOwnedClient(t *testing.T) {
	for _, tc := range []struct {
		name, listing string
		ok            bool
	}{
		{"owned", "77 /dev/other %3 /dev/ttys777 123\n42 /dev/ttys123 %4 /dev/ttys124 456\n", true},
		{"other only", "77 /dev/other %3 /dev/ttys777 123\n", false},
		{"invalid client tty", "42 -a %4 /dev/ttys124 456\n", false},
		{"invalid pane tty", "42 /dev/ttys123 %4 /tmp/tty 456\n", false},
		{"invalid pane pid", "42 /dev/ttys123 %4 /dev/ttys124 1\n", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := &refreshRunner{listing: tc.listing}
			p := &AppViewPTY{Done: make(chan struct{}), runner: r, name: "hmux-e2e-view-test", pid: 42}
			signalled := false
			err := p.refresh(context.Background(), func(_ context.Context, pid int, tty string) (int, error) {
				if pid != 456 || tty != "/dev/ttys124" {
					t.Fatal("wrong pane", pid, tty)
				}
				return 789, nil
			}, func(group int) error {
				if group != 789 {
					t.Fatal(group)
				}
				signalled = true
				return nil
			})
			if (err == nil) != tc.ok || signalled != tc.ok {
				t.Fatal(err, signalled)
			}
			if !reflect.DeepEqual(r.calls[0], []string{"list-clients", "-t", p.name, "-F", refreshClientFormat}) {
				t.Fatal(r.calls)
			}
			if tc.ok {
				if len(r.calls) != 3 || !reflect.DeepEqual(r.calls[2], []string{"refresh-client", "-t", "/dev/ttys123"}) {
					t.Fatal(r.calls)
				}
			} else if len(r.calls) != 1 {
				t.Fatal("touched foreign client", r.calls)
			}
		})
	}
}
func TestAppViewRefreshRejectsChangedPane(t *testing.T) {
	r := &refreshRunner{listing: "42 /dev/ttys123 %4 /dev/ttys124 456\n"}
	p := &AppViewPTY{Done: make(chan struct{}), runner: r, name: "hmux-e2e-view-test", pid: 42}
	err := p.refresh(context.Background(), func(context.Context, int, string) (int, error) {
		r.listing = "42 /dev/ttys123 %5 /dev/ttys125 457\n"
		return 789, nil
	}, func(int) error { t.Fatal("signalled stale pane"); return nil })
	if err == nil || len(r.calls) != 2 {
		t.Fatal(err, r.calls)
	}
}

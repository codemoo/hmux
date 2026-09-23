package homeservice

import (
	"bytes"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"syscall"
	"time"
)

type process struct {
	PID         int
	Birth       string
	Args        []string
	Environment map[string]string
}

// Only fixed path/locale keys leave the native process metadata reader. Token
// bytes and unrelated environment values are never retained or logged.
func processEnvironment(raw []byte) map[string]string {
	allowed := map[string]bool{"HOME": true}
	for _, key := range serviceEnvKeys {
		allowed[key] = true
	}
	env := make(map[string]string)
	for len(raw) > 0 {
		at := bytes.IndexByte(raw, 0)
		if at < 0 {
			break
		}
		entry := raw[:at]
		raw = raw[at+1:]
		eq := bytes.IndexByte(entry, '=')
		if eq < 0 {
			continue
		}
		key := string(entry[:eq])
		if _, exists := env[key]; allowed[key] && !exists {
			env[key] = string(entry[eq+1:])
		}
	}
	return env
}

func connectorOptions(p process) (endpoint, token, configPath string, err error) {
	if len(p.Args) < 2 || filepath.Base(p.Args[0]) != "hmux-web" || p.Args[1] != "connect" {
		return "", "", "", errors.New("process is not a Home connector")
	}
	f := flag.NewFlagSet("connect", flag.ContinueOnError)
	f.SetOutput(io.Discard)
	f.StringVar(&endpoint, "url", "", "")
	f.StringVar(&token, "token-file", "", "")
	f.StringVar(&configPath, "config", "", "")
	f.String("log-file", "", "")
	if err = f.Parse(p.Args[2:]); err != nil || f.NArg() != 0 {
		return "", "", "", errors.New("cannot adopt connector with unrecognized arguments")
	}
	// Relative files depend on an unknown working directory; never guess it.
	if !filepath.IsAbs(token) || (configPath != "" && !filepath.IsAbs(configPath)) {
		return "", "", "", errors.New("adoption requires absolute token/config paths; stop the old connector and install with explicit options")
	}
	return
}

func sameProcess(a, b process) bool {
	return a.PID == b.PID && a.Birth == b.Birth && reflect.DeepEqual(a.Args, b.Args) && reflect.DeepEqual(a.Environment, b.Environment)
}

func stopAdopted(ctx context.Context, p process) error {
	current, err := readProcess(p.PID)
	if err != nil || !sameProcess(current, p) {
		return errors.New("connector identity changed; no process was signalled")
	}
	// Positive PID only: never signal a process group or touch a tmux process.
	if p.PID <= 1 || p.PID == os.Getpid() {
		return errors.New("invalid connector PID")
	}
	if err := syscall.Kill(p.PID, syscall.SIGTERM); err != nil {
		return fmt.Errorf("signal verified connector: %w", err)
	}
	timer := time.NewTimer(20 * time.Second)
	defer timer.Stop()
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		current, err := readProcess(p.PID)
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ESRCH) || (err == nil && !sameProcess(current, p)) {
			return nil
		}
		// macOS can deny procargs while a process is exiting but before it is
		// reported as a zombie/gone. Retry without treating that denial as proof
		// of exit; the deadline still prevents starting a competing connector.
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
			return errors.New("connector did not stop within 20 seconds; left it untouched, service not started")
		case <-tick.C:
		}
	}
}

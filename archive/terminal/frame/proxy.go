package frame

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sessionstate"
	"github.com/codemoo/hmux/internal/tabstate"
	"github.com/creack/pty"
	"golang.org/x/term"
)

const privateSequenceDelay = 35 * time.Millisecond

var privateSequences = map[string]string{
	"\x1b[5;9010~": "list",
	"\x1b[5;9011~": "close",
	"\x1b[5;9012~": "quit",
	"\x1b[5;9013~": "alias",
	"\x1b[5;9021~": "1",
	"\x1b[5;9022~": "2",
	"\x1b[5;9023~": "3",
	"\x1b[5;9024~": "4",
	"\x1b[5;9025~": "5",
	"\x1b[5;9026~": "6",
	"\x1b[5;9027~": "7",
	"\x1b[5;9028~": "8",
	"\x1b[5;9029~": "9",
}

type proxyInput struct {
	data []byte
	err  error
}

type privateDecoder struct {
	pending []byte
}

type decodedInput struct {
	data   []byte
	action string
}

type aliasEditor struct {
	active bool
	value  []byte
	error  string
}

func runTargetProxy(
	command *exec.Cmd,
	tmuxPath string,
	values environment,
	controlName string,
) (error, bool) {
	columns, rows := terminalSize(os.Stdin)
	master, err := pty.StartWithSize(command, &pty.Winsize{
		Rows: uint16(rows), Cols: uint16(columns), // #nosec G115 -- terminalSize is bounded.
	})
	if err != nil {
		return err, false
	}
	defer master.Close()

	clientName, err := waitForTargetClient(tmuxPath, command.Process.Pid)
	if err != nil {
		_ = command.Process.Kill()
		_ = command.Wait()
		return err, false
	}
	store := tabstate.Store{StateDir: values.stateDir}
	if err := store.OpenFrame(
		values.launcherID, clientName, controlName, values.sessionID,
	); err != nil {
		_ = command.Process.Kill()
		_ = command.Wait()
		return err, false
	}
	refreshCtx, refreshCancel := context.WithTimeout(context.Background(), time.Second)
	syncOuterStatus(
		refreshCtx, values.stateDir, values.launcherID, controlName,
	)
	refreshCancel()

	oldState, err := term.MakeRaw(int(os.Stdin.Fd()))
	if err != nil {
		_ = command.Process.Kill()
		_ = command.Wait()
		return fmt.Errorf("enable frame input proxy: %w", err), false
	}
	defer term.Restore(int(os.Stdin.Fd()), oldState) //nolint:errcheck

	completed := make(chan error, 1)
	go func() { completed <- command.Wait() }()
	output := make(chan proxyInput, 16)
	outputDone := make(chan struct{})
	defer close(outputDone)
	go readProxyOutput(master, output, outputDone)
	input := make(chan proxyInput, 8)
	go readProxyInput(os.Stdin, input)
	resize := make(chan os.Signal, 1)
	signal.Notify(resize, syscall.SIGWINCH)
	defer signal.Stop(resize)

	decoder := &privateDecoder{}
	editor := &aliasEditor{}
	timer := time.NewTimer(time.Hour)
	if !timer.Stop() {
		<-timer.C
	}
	defer timer.Stop()

	for {
		select {
		case err := <-completed:
			return err, false
		case event := <-output:
			if len(event.data) > 0 {
				if _, err := os.Stdout.Write(event.data); err != nil {
					_ = command.Process.Kill()
					<-completed
					return fmt.Errorf("write target output: %w", err), false
				}
			}
			// A PTY commonly reports EIO when its slave closes. command.Wait
			// is the authoritative exit result; most importantly, no output
			// goroutine is allowed to race RunInner's final screen clear.
		case event := <-input:
			if len(event.data) > 0 {
				pending := event.data
				for len(pending) > 0 {
					if editor.active {
						var finished bool
						pending, finished = handleAliasInput(
							editor, pending, values, clientName, columns, rows,
						)
						if !finished {
							break
						}
						resetPrivateTimer(timer, false)
						continue
					}
					for _, decoded := range decoder.Feed(pending) {
						if len(decoded.data) > 0 {
							if _, err := master.Write(decoded.data); err != nil {
								return fmt.Errorf("forward target input: %w", err), false
							}
						}
						if decoded.action != "" {
							if decoded.action == "alias" {
								editor.active = true
								editor.value = editor.value[:0]
								editor.error = ""
								drawAliasEditor(editor, columns, rows)
								continue
							}
							actionCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
							result, actionErr := Action(
								actionCtx, values.stateDir, values.launcherID, decoded.action,
							)
							cancel()
							if actionErr != nil {
								editor.error = model.SafeText(actionErr.Error(), 160)
								drawTransientFrameError(editor.error, columns, rows)
								refreshTargetClient(clientName)
								continue
							}
							switch result {
							case actionClose, actionLeave:
								_ = command.Process.Kill()
								return <-completed, true
							case actionQuit:
								_ = command.Process.Kill()
								<-completed
								return &requestedExitError{status: 130}, false
							}
						}
					}
					pending = nil
					resetPrivateTimer(timer, len(decoder.pending) > 0)
				}
			}
			if event.err != nil {
				if len(decoder.pending) > 0 {
					_, _ = master.Write(decoder.Flush())
				}
				if errors.Is(event.err, io.EOF) {
					_ = command.Process.Kill()
					return <-completed, false
				}
				return event.err, false
			}
		case <-timer.C:
			if len(decoder.pending) > 0 {
				_, _ = master.Write(decoder.Flush())
			}
		case <-resize:
			_ = pty.InheritSize(os.Stdin, master)
			columns, rows = terminalSize(os.Stdin)
			if editor.active {
				drawAliasEditor(editor, columns, rows)
			}
		}
	}
}

func readProxyOutput(input io.Reader, events chan<- proxyInput, done <-chan struct{}) {
	buffer := make([]byte, 4096)
	for {
		count, err := input.Read(buffer)
		event := proxyInput{err: err}
		if count > 0 {
			event.data = append([]byte(nil), buffer[:count]...)
		}
		select {
		case events <- event:
		case <-done:
			return
		}
		if err != nil {
			return
		}
	}
}

func readProxyInput(input io.Reader, events chan<- proxyInput) {
	buffer := make([]byte, 4096)
	for {
		count, err := input.Read(buffer)
		event := proxyInput{err: err}
		if count > 0 {
			event.data = append([]byte(nil), buffer[:count]...)
		}
		events <- event
		if err != nil {
			return
		}
	}
}

func (d *privateDecoder) Feed(input []byte) []decodedInput {
	var result []decodedInput
	for _, value := range input {
		d.pending = append(d.pending, value)
		for len(d.pending) > 0 {
			if action, exact := privateSequences[string(d.pending)]; exact {
				result = append(result, decodedInput{action: action})
				d.pending = d.pending[:0]
				break
			}
			if privateSequencePrefix(d.pending) {
				break
			}
			result = append(result, decodedInput{data: []byte{d.pending[0]}})
			d.pending = d.pending[1:]
		}
	}
	return result
}

func (d *privateDecoder) Flush() []byte {
	result := append([]byte(nil), d.pending...)
	d.pending = d.pending[:0]
	return result
}

func privateSequencePrefix(value []byte) bool {
	for sequence := range privateSequences {
		if len(value) < len(sequence) && bytes.HasPrefix([]byte(sequence), value) {
			return true
		}
	}
	return false
}

func resetPrivateTimer(timer *time.Timer, active bool) {
	if !timer.Stop() {
		select {
		case <-timer.C:
		default:
		}
	}
	if active {
		timer.Reset(privateSequenceDelay)
	}
}

func waitForTargetClient(tmuxPath string, processID int) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()
	expectedPID := strconv.Itoa(processID)
	runner := catalog.TmuxRunner{
		Path: tmuxPath, Env: withoutTmuxEnvironment(os.Environ()),
	}
	for {
		output, err := runner.Output(
			ctx, "list-clients", "-F", "#{client_pid}\t#{client_name}",
		)
		if err == nil {
			for _, line := range strings.Split(strings.TrimSpace(string(output)), "\n") {
				fields := strings.Split(line, "\t")
				if len(fields) == 2 && fields[0] == expectedPID &&
					tabstate.ValidateClientName(fields[1]) == nil {
					return fields[1], nil
				}
			}
		}
		select {
		case <-ctx.Done():
			return "", errors.New("target tmux client did not become ready")
		case <-ticker.C:
		}
	}
}

func handleAliasInput(
	editor *aliasEditor,
	input []byte,
	values environment,
	clientName string,
	columns, rows int,
) ([]byte, bool) {
	for index := 0; index < len(input); index++ {
		value := input[index]
		switch value {
		case 0x1b:
			editor.active = false
			editor.value = editor.value[:0]
			editor.error = ""
			refreshTargetClient(clientName)
			// Escape can be the first byte of a terminal key sequence. Cancel
			// the editor without forwarding the rest of this read to the target.
			return nil, true
		case '\r', '\n':
			alias := string(editor.value)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			err := SetAlias(ctx, values.stateDir, values.launcherID, alias)
			cancel()
			if err != nil {
				editor.error = model.SafeText(err.Error(), 160)
				drawAliasEditor(editor, columns, rows)
				continue
			}
			editor.active = false
			editor.value = editor.value[:0]
			editor.error = ""
			refreshTargetClient(clientName)
			// A paste or fast key sequence may share this read with Enter.
			// Reprocess those bytes as normal target/private-sequence input.
			return input[index+1:], true
		case 0x7f, 0x08:
			if len(editor.value) > 0 {
				_, size := utf8.DecodeLastRune(editor.value)
				if size < 1 {
					size = 1
				}
				editor.value = editor.value[:len(editor.value)-size]
			}
		case 0x15:
			editor.value = editor.value[:0]
		default:
			if value >= 0x20 && len(editor.value) < 512 {
				editor.value = append(editor.value, value)
			}
		}
	}
	drawAliasEditor(editor, columns, rows)
	return nil, false
}

func SetAlias(ctx context.Context, stateDir, launcherID, alias string) error {
	frameState, sessions, err := frameSessions(ctx, stateDir, launcherID)
	if err != nil {
		return err
	}
	for _, session := range sessions {
		if session.ID == frameState.CurrentID {
			if err := (sessionstate.Store{StateDir: stateDir}).SetAlias(session, alias); err != nil {
				return err
			}
			syncOuterStatus(ctx, stateDir, launcherID, frameState.ControlName)
			return nil
		}
	}
	return errors.New("current frame session no longer exists")
}

func drawAliasEditor(editor *aliasEditor, columns, rows int) {
	label := " ALIAS  "
	suffix := "  blank restores original · Esc cancels "
	body := model.SafeText(string(editor.value), 128)
	if editor.error != "" {
		body = "ERROR · " + editor.error
	}
	text := truncateBytes(label+body+suffix, max(1, columns-1))
	_, _ = fmt.Fprintf(
		os.Stdout,
		"\x1b[%d;1H\x1b[48;2;40;39;38m\x1b[38;2;206;205;195m\x1b[2K%s\x1b[0m",
		rows, text,
	)
}

func drawTransientFrameError(message string, columns, rows int) {
	text := truncateBytes(" HMUX · "+message+" ", max(1, columns-1))
	_, _ = fmt.Fprintf(
		os.Stdout,
		"\x1b[%d;1H\x1b[48;2;40;39;38m\x1b[38;2;218;112;44m\x1b[2K%s\x1b[0m",
		rows, text,
	)
}

func refreshTargetClient(clientName string) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, _ = targetTmuxRunner().Output(ctx, "refresh-client", "-S", "-t", clientName)
}

func terminalSize(file *os.File) (int, int) {
	columns, rows, err := term.GetSize(int(file.Fd()))
	if err != nil || columns < 20 || rows < 6 {
		return 80, 24
	}
	if columns > 1000 {
		columns = 1000
	}
	if rows > 500 {
		rows = 500
	}
	return columns, rows
}

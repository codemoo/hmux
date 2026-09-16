package safeexec

import (
	"bytes"
	"errors"
	"fmt"
	"os/exec"
)

var errOutputLimit = errors.New("command output limit exceeded")

type CommandError struct {
	Err    error
	Stderr string
}

func (e *CommandError) Error() string { return e.Err.Error() }
func (e *CommandError) Unwrap() error { return e.Err }

func Stderr(err error) string {
	var commandErr *CommandError
	if errors.As(err, &commandErr) {
		return commandErr.Stderr
	}
	return ""
}

type limitedBuffer struct {
	buffer   bytes.Buffer
	max      int64
	exceeded bool
}

func (b *limitedBuffer) Write(data []byte) (int, error) {
	remaining := b.max - int64(b.buffer.Len())
	if remaining <= 0 {
		b.exceeded = true
		return 0, errOutputLimit
	}
	if int64(len(data)) <= remaining {
		return b.buffer.Write(data)
	}
	written, _ := b.buffer.Write(data[:int(remaining)])
	b.exceeded = true
	return written, errOutputLimit
}

func Output(command *exec.Cmd, maxBytes int64) ([]byte, error) {
	return output(command, maxBytes, 0)
}

// OutputOnPartialExit preserves bounded stdout only for the caller's explicitly
// accepted partial-result exit code. Cancellation and output limits stay fatal.
func OutputOnPartialExit(command *exec.Cmd, maxBytes int64, partialExitCode int) ([]byte, error) {
	if partialExitCode < 1 {
		return nil, errors.New("invalid partial exit code")
	}
	return output(command, maxBytes, partialExitCode)
}

func output(command *exec.Cmd, maxBytes int64, partialExitCode int) ([]byte, error) {
	if maxBytes < 1 {
		return nil, errors.New("command output limit must be positive")
	}
	var output limitedBuffer
	output.max = maxBytes
	command.Stdout = &output
	var stderr limitedBuffer
	if command.Stderr == nil {
		stderr.max = 64 * 1024
		command.Stderr = &stderr
	}
	err := command.Run()
	if output.exceeded {
		return nil, fmt.Errorf("command output exceeds %d bytes", maxBytes)
	}
	if stderr.exceeded {
		return nil, errors.New("command stderr exceeds 65536 bytes")
	}
	if err != nil {
		var exit *exec.ExitError
		if partialExitCode > 0 && errors.As(err, &exit) && exit.ExitCode() == partialExitCode {
			return output.buffer.Bytes(), nil
		}
		if stderr.max > 0 {
			return nil, &CommandError{Err: err, Stderr: stderr.buffer.String()}
		}
		return nil, err
	}
	return output.buffer.Bytes(), nil
}

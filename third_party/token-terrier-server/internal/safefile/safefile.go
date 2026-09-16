// Package safefile reads small user-owned state files without following
// symlinks or blocking on special files such as FIFOs.
package safefile

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"syscall"
)

type Snapshot struct {
	Data []byte
	Info os.FileInfo
}

// ValidateJSONArrayLimit counts top-level elements with a streaming decoder
// before an untrusted JSON array is unmarshaled into a Go slice.
func ValidateJSONArrayLimit(data []byte, maximumElements int) error {
	if maximumElements < 0 {
		return errors.New("invalid JSON array limit")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	opening, err := decoder.Token()
	if err != nil || opening != json.Delim('[') {
		return errors.New("JSON value must be an array")
	}
	count := 0
	for decoder.More() {
		count++
		if count > maximumElements {
			return errors.New("JSON array element count exceeds limit")
		}
		var discard json.RawMessage
		if err := decoder.Decode(&discard); err != nil {
			return err
		}
	}
	closing, err := decoder.Token()
	if err != nil || closing != json.Delim(']') {
		return errors.New("JSON array is incomplete")
	}
	if _, err := decoder.Token(); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("JSON array contains trailing data")
		}
		return err
	}
	return nil
}

func Inspect(path string, maximumBytes int64) (os.FileInfo, error) {
	file, info, err := Open(path, maximumBytes)
	if err != nil {
		return nil, err
	}
	if err := file.Close(); err != nil {
		return nil, err
	}
	return info, nil
}

func Read(path string, maximumBytes int64) (Snapshot, error) {
	var snapshot Snapshot
	file, before, err := Open(path, maximumBytes)
	if err != nil {
		return snapshot, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, maximumBytes+1))
	if err != nil {
		return snapshot, err
	}
	if int64(len(data)) > maximumBytes {
		return snapshot, errors.New("state file exceeds size limit")
	}
	after, err := file.Stat()
	if err != nil {
		return snapshot, err
	}
	if !os.SameFile(before, after) || before.Size() != after.Size() ||
		before.ModTime() != after.ModTime() || after.Size() != int64(len(data)) {
		return snapshot, errors.New("state file changed while reading")
	}
	return Snapshot{Data: data, Info: after}, nil
}

// Open returns an already-validated nonblocking descriptor for a private,
// user-owned regular file. The descriptor never follows a final symlink.
func Open(path string, maximumBytes int64) (*os.File, os.FileInfo, error) {
	if path == "" || maximumBytes < 1 {
		return nil, nil, errors.New("invalid state file lookup")
	}
	fd, err := syscall.Open(path, syscall.O_RDONLY|syscall.O_CLOEXEC|syscall.O_NOFOLLOW|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, nil, err
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = syscall.Close(fd)
		return nil, nil, errors.New("open state file")
	}
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, nil, err
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 ||
		info.Mode().Perm()&0o022 != 0 || info.Size() < 0 || info.Size() > maximumBytes ||
		!ok || int(stat.Uid) != os.Getuid() || stat.Nlink != 1 {
		file.Close()
		return nil, nil, fmt.Errorf("state file must be a small private user-owned regular file")
	}
	return file, info, nil
}

package homeservice

import (
	"bytes"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
)

func readProcFile(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	raw, err := io.ReadAll(io.LimitReader(f, limit+1))
	if int64(len(raw)) > limit {
		return nil, errors.New("process metadata exceeds limit")
	}
	return raw, err
}

func readProcess(pid int) (process, error) {
	base := filepath.Join("/proc", strconv.Itoa(pid))
	info, err := os.Stat(base)
	if err != nil {
		return process{}, err
	}
	if st, ok := info.Sys().(*syscall.Stat_t); !ok || int(st.Uid) != os.Getuid() {
		return process{}, errors.New("connector belongs to a different user")
	}
	raw, err := readProcFile(filepath.Join(base, "stat"), 65536)
	if err != nil {
		return process{}, err
	}
	at := strings.LastIndexByte(string(raw), ')')
	if at < 0 {
		return process{}, errors.New("invalid process stat")
	}
	fields := strings.Fields(string(raw[at+1:]))
	if len(fields) < 20 {
		return process{}, errors.New("invalid process stat")
	}
	if fields[0] == "Z" {
		return process{}, os.ErrNotExist
	}
	raw, err = readProcFile(filepath.Join(base, "cmdline"), 65536)
	if err != nil {
		return process{}, err
	}
	if len(raw) > 65536 {
		return process{}, errors.New("process arguments exceed limit")
	}
	parts := bytes.Split(bytes.TrimSuffix(raw, []byte{0}), []byte{0})
	args := make([]string, len(parts))
	for i := range parts {
		args[i] = string(parts[i])
	}
	raw, err = readProcFile(filepath.Join(base, "environ"), 2<<20)
	if err != nil {
		return process{}, err
	}
	return process{PID: pid, Birth: fields[19], Args: args, Environment: processEnvironment(raw)}, nil
}

func connectors() ([]process, error) {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return nil, err
	}
	var found []process
	for _, entry := range entries {
		pid, err := strconv.Atoi(entry.Name())
		if err != nil || pid <= 1 || pid == os.Getpid() {
			continue
		}
		base := filepath.Join("/proc", entry.Name())
		info, err := os.Stat(base)
		if err != nil {
			continue
		}
		if st, ok := info.Sys().(*syscall.Stat_t); !ok || int(st.Uid) != os.Getuid() {
			continue
		}
		comm, err := os.ReadFile(filepath.Join(base, "comm"))
		if err != nil || strings.TrimSpace(string(comm)) != "hmux-web" {
			continue
		}
		p, err := readProcess(pid)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		if len(p.Args) >= 2 && p.Args[1] == "connect" {
			found = append(found, p)
		}
	}
	return found, nil
}

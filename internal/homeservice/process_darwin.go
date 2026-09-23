package homeservice

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

func darwinArguments(raw []byte) ([]string, error) {
	args, _, err := darwinProcessData(raw)
	return args, err
}

func darwinProcessData(raw []byte) ([]string, map[string]string, error) {
	if len(raw) < 4 {
		return nil, nil, errors.New("invalid process arguments")
	}
	n := int(binary.NativeEndian.Uint32(raw[:4]))
	if n < 1 || n > 128 {
		return nil, nil, errors.New("invalid process argument count")
	}
	raw = raw[4:]
	at := bytes.IndexByte(raw, 0) // executable path precedes argv
	if at < 0 {
		return nil, nil, errors.New("missing executable path")
	}
	raw = bytes.TrimLeft(raw[at:], "\x00")
	args := make([]string, 0, n)
	for i := 0; i < n; i++ {
		at = bytes.IndexByte(raw, 0)
		if at < 0 {
			return nil, nil, errors.New("truncated process arguments")
		}
		args = append(args, string(raw[:at]))
		raw = raw[at+1:]
	}
	return args, processEnvironment(raw), nil
}

func readProcess(pid int) (process, error) {
	info, err := unix.SysctlKinfoProc("kern.proc.pid", pid)
	if err != nil {
		return process{}, err
	}
	if info.Proc.P_pid == 0 || info.Proc.P_stat == 5 {
		return process{}, os.ErrNotExist
	}
	if int(info.Eproc.Pcred.P_ruid) != os.Getuid() || int(info.Eproc.Ucred.Uid) != os.Getuid() {
		return process{}, errors.New("connector belongs to a different user")
	}
	raw, err := unix.SysctlRaw("kern.procargs2", pid)
	if err != nil {
		return process{}, err
	}
	args, env, err := darwinProcessData(raw)
	if err != nil {
		return process{}, err
	}
	return process{PID: pid, Birth: fmt.Sprintf("%d:%d", info.Proc.P_starttime.Sec, info.Proc.P_starttime.Usec), Args: args, Environment: env}, nil
}

func darwinConnectorCandidate(info unix.KinfoProc) bool {
	// P_comm is a C string; kernel bytes after its first NUL can contain stale
	// data. Trimming only trailing NULs can miss a real running connector.
	return int(info.Eproc.Pcred.P_ruid) == os.Getuid() &&
		unix.ByteSliceToString(info.Proc.P_comm[:]) == "hmux-web" &&
		int(info.Proc.P_pid) != os.Getpid()
}

func connectors() ([]process, error) {
	all, err := unix.SysctlKinfoProcSlice("kern.proc.all")
	if err != nil {
		return nil, err
	}
	var found []process
	for _, info := range all {
		if !darwinConnectorCandidate(info) {
			continue
		}
		p, err := readProcess(int(info.Proc.P_pid))
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, unix.ESRCH) {
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

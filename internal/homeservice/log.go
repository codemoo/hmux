package homeservice

import (
	"errors"
	"os"
	"path/filepath"
	"sync"
	"syscall"
)

const LogLimit = 1 << 20

// Log keeps at most one current file and one previous file, each <= 1 MiB.
// Callers log connection categories only, never remote errors or terminal data.
type Log struct {
	mu   sync.Mutex
	path string
	file *os.File
	size int64
}

func OpenLog(path string) (*Log, error) {
	if err := trustedDirectory(filepath.Dir(path), true); err != nil {
		return nil, err
	}
	l := &Log{path: path}
	if err := l.open(); err != nil {
		return nil, err
	}
	return l, nil
}

func (l *Log) open() error {
	fd, err := syscall.Open(l.path, syscall.O_WRONLY|syscall.O_APPEND|syscall.O_CREAT|syscall.O_NOFOLLOW|syscall.O_NONBLOCK|syscall.O_CLOEXEC, 0600)
	if err != nil {
		return err
	}
	f := os.NewFile(uintptr(fd), l.path)
	info, err := f.Stat()
	if err == nil {
		st, ok := info.Sys().(*syscall.Stat_t)
		if !ok || !info.Mode().IsRegular() || int(st.Uid) != os.Getuid() || st.Nlink != 1 || info.Mode().Perm()&0077 != 0 || info.Size() > LogLimit {
			err = errors.New("unsafe service log")
		}
	}
	if err != nil {
		_ = f.Close()
		return err
	}
	l.file, l.size = f, info.Size()
	return nil
}

func (l *Log) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.file == nil {
		return 0, os.ErrClosed
	}
	if len(p) > LogLimit {
		return 0, errors.New("log entry exceeds limit")
	}
	if l.size+int64(len(p)) > LogLimit {
		if _, err := os.Lstat(l.path + ".1"); err == nil {
			if _, err := readOwned(l.path+".1", LogLimit, true); err != nil {
				return 0, err
			}
		} else if !errors.Is(err, os.ErrNotExist) {
			return 0, err
		}
		if err := l.file.Close(); err != nil {
			return 0, err
		}
		l.file = nil
		if err := os.Rename(l.path, l.path+".1"); err != nil {
			return 0, err
		}
		if err := l.open(); err != nil {
			return 0, err
		}
	}
	n, err := l.file.Write(p)
	l.size += int64(n)
	return n, err
}

func (l *Log) Close() error {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.file == nil {
		return nil
	}
	err := l.file.Close()
	l.file = nil
	return err
}

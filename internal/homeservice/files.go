package homeservice

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

func trustedDirectory(path string, create bool) error {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return errors.New("service directory must be a clean absolute path")
	}
	for at := path; ; at = filepath.Dir(at) {
		info, err := os.Lstat(at)
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		if err == nil {
			st, ok := info.Sys().(*syscall.Stat_t)
			if !ok || !info.IsDir() || info.Mode()&os.ModeSymlink != 0 ||
				(st.Uid != 0 && int(st.Uid) != os.Getuid()) ||
				(info.Mode().Perm()&0022 != 0 && !(st.Uid == 0 && info.Mode()&os.ModeSticky != 0)) {
				return fmt.Errorf("untrusted service directory: %s", at)
			}
		}
		if filepath.Dir(at) == at {
			break
		}
	}
	if create {
		if err := os.MkdirAll(path, 0700); err != nil {
			return err
		}
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if st, ok := info.Sys().(*syscall.Stat_t); !ok || int(st.Uid) != os.Getuid() {
		return errors.New("service directory must belong to current user")
	}
	return nil
}

func systemExecutable(path string) bool {
	info, err := os.Stat(path)
	if err != nil {
		return false
	}
	st, ok := info.Sys().(*syscall.Stat_t)
	return ok && st.Uid == 0 && info.Mode().IsRegular() && info.Mode().Perm()&0022 == 0 && info.Mode().Perm()&0111 != 0
}

func readOwned(path string, max int64, private bool) ([]byte, error) {
	fd, err := syscall.Open(path, syscall.O_RDONLY|syscall.O_NOFOLLOW|syscall.O_NONBLOCK|syscall.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(fd), path)
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	st, ok := info.Sys().(*syscall.Stat_t)
	mask := os.FileMode(0022)
	if private {
		mask = 0077
	}
	if !ok || int(st.Uid) != os.Getuid() || !info.Mode().IsRegular() || info.Mode().Perm()&mask != 0 || st.Nlink != 1 || info.Size() > max {
		return nil, errors.New("service file must be a bounded owner-controlled regular file")
	}
	raw, err := io.ReadAll(io.LimitReader(f, max+1))
	if len(raw) > int(max) {
		return nil, errors.New("service file exceeds limit")
	}
	return raw, err
}

func writeBackedUp(path string, raw []byte, mode os.FileMode) error {
	if err := trustedDirectory(filepath.Dir(path), true); err != nil {
		return err
	}
	if _, err := os.Lstat(path); err == nil {
		if _, err := readOwned(path, 256<<20, false); err != nil {
			return err
		}
		if _, err := config.Backup(path, time.Now()); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return config.AtomicWrite(path, raw, mode)
}

// LockConnector lasts for the process lifetime. The file is never unlinked:
// deleting it would let a second process lock a different inode at the same path.
func LockConnector(stateDir string) (*os.File, error) {
	if err := trustedDirectory(stateDir, true); err != nil {
		return nil, err
	}
	return lockFile(filepath.Join(stateDir, "home-connector.lock"))
}

func lockFile(path string) (*os.File, error) {
	fd, err := syscall.Open(path, syscall.O_RDWR|syscall.O_CREAT|syscall.O_NOFOLLOW|syscall.O_NONBLOCK|syscall.O_CLOEXEC, 0600)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(fd), path)
	info, err := f.Stat()
	if err == nil {
		st, ok := info.Sys().(*syscall.Stat_t)
		if !ok || !info.Mode().IsRegular() || int(st.Uid) != os.Getuid() || st.Nlink != 1 || info.Mode().Perm()&0077 != 0 {
			err = errors.New("unsafe connector lock")
		}
	}
	if err == nil {
		err = syscall.Flock(fd, syscall.LOCK_EX|syscall.LOCK_NB)
	}
	if err != nil {
		_ = f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) || errors.Is(err, syscall.EAGAIN) {
			return nil, errors.New("a Home connector is already running for this state directory")
		}
		return nil, err
	}
	return f, nil
}

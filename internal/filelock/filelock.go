package filelock

import (
	"context"
	"errors"
	"fmt"
	"os"
	"syscall"
	"time"
)

const pollInterval = 25 * time.Millisecond

// ErrBusy reports that a bounded lock acquisition expired before the lock
// became available. Callers should leave the protected state unchanged.
var ErrBusy = errors.New("file lock is busy")

// Acquire obtains an exclusive advisory lock without ever blocking inside
// flock(2). The caller context and maxWait both bound acquisition; a non-
// positive maxWait relies only on the caller context.
func Acquire(ctx context.Context, file *os.File, maxWait time.Duration) error {
	if ctx == nil || file == nil {
		return errors.New("file lock is unavailable")
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	var timer *time.Timer
	var timeout <-chan time.Time
	if maxWait > 0 {
		timer = time.NewTimer(maxWait)
		defer timer.Stop()
		timeout = timer.C
	}
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()
	for {
		err := syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			return nil
		}
		if errors.Is(err, syscall.EINTR) {
			continue
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) && !errors.Is(err, syscall.EAGAIN) {
			return fmt.Errorf("acquire file lock: %w", err)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timeout:
			return ErrBusy
		case <-ticker.C:
		}
	}
}

func Unlock(file *os.File) {
	if file != nil {
		_ = syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
	}
}

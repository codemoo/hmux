// hmux-flock-oracle is a synthetic test helper, never a shipped runtime tool.
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/filelock"
)

func main() {
	if len(os.Args) != 3 || (os.Args[1] != "hold" && os.Args[1] != "try") || !filepath.IsAbs(os.Args[2]) {
		os.Exit(2)
	}
	file, err := os.OpenFile(os.Args[2], os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0600)
	if err != nil {
		os.Exit(3)
	}
	defer file.Close()
	err = filelock.Acquire(context.Background(), file, 25*time.Millisecond)
	if errors.Is(err, filelock.ErrBusy) {
		fmt.Println("busy")
		return
	}
	if err != nil {
		os.Exit(4)
	}
	defer filelock.Unlock(file)
	fmt.Println("locked")
	if os.Args[1] == "hold" {
		var end [1]byte
		_, _ = os.Stdin.Read(end[:])
	}
}

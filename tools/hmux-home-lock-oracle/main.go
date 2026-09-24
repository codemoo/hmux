// Command hmux-home-lock-oracle exposes the actual Go Home connector lock to
// isolated, cross-language process tests.
package main

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/codemoo/hmux/internal/homeservice"
)

const busyError = "a Home connector is already running for this state directory"

func main() {
	if len(os.Args) != 3 || (os.Args[1] != "hold" && os.Args[1] != "try") ||
		!filepath.IsAbs(os.Args[2]) || filepath.Clean(os.Args[2]) != os.Args[2] {
		fail()
	}
	lock, err := homeservice.LockConnector(os.Args[2])
	if err != nil {
		if err.Error() == busyError {
			fmt.Fprintln(os.Stdout, "busy")
			return
		}
		fail()
	}
	defer lock.Close()
	fmt.Fprintln(os.Stdout, "locked")
	if os.Args[1] == "hold" {
		var signal [1]byte
		_, _ = os.Stdin.Read(signal[:])
	}
}

func fail() {
	fmt.Fprintln(os.Stderr, "error")
	os.Exit(2)
}

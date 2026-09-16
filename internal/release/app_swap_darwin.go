//go:build darwin

package release

import "golang.org/x/sys/unix"

func atomicSwapPaths(left, right string) error {
	return unix.RenamexNp(left, right, unix.RENAME_SWAP)
}

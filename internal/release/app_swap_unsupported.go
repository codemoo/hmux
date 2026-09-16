//go:build !darwin

package release

import "errors"

func atomicSwapPaths(_, _ string) error {
	return errors.New("atomic app bundle exchange is unsupported on this platform")
}

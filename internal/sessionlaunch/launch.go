// Package sessionlaunch owns the provider-to-interactive-shell lifecycle.
package sessionlaunch

import (
	"os"
	"path/filepath"
)

// Shell chooses an executable absolute shell path without evaluating shell text.
func Shell() string {
	for _, path := range []string{os.Getenv("SHELL"), "/bin/zsh", "/bin/bash", "/bin/sh"} {
		if !filepath.IsAbs(path) {
			continue
		}
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode().Perm()&0111 != 0 {
			return path
		}
	}
	return "/bin/sh"
}

// Keep the runner alive even when a terminal interrupt ends the provider.
// The subshell resets signal dispositions before exec, so the provider still
// receives Ctrl+C normally. No untrusted data is interpolated into this script.
const providerScript = `shell=$1; shift
trap ':' INT QUIT
(trap - INT QUIT; exec "$@")
exec "$shell" -i`

// Provider runs the exact argv once, then starts a shell on success or failure.
func Provider(command []string) []string {
	return append([]string{"/bin/sh", "-c", providerScript, "hmux-provider", Shell()}, command...)
}

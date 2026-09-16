package recovery

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"runtime"
	"strings"

	"github.com/codemoo/hmux/internal/safeexec"
)

var darwinBootTimePattern = regexp.MustCompile(`sec = ([0-9]{1,20}), usec = ([0-9]{1,6})`)

func systemBootID(ctx context.Context) (string, error) {
	switch runtime.GOOS {
	case "linux":
		raw, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
		if err != nil {
			return "", err
		}
		if len(raw) > 128 {
			return "", errors.New("Linux boot identity exceeds limit")
		}
		return "linux-" + strings.TrimSpace(string(raw)), nil
	case "darwin":
		raw, err := safeexec.Output(exec.CommandContext(ctx, "/usr/sbin/sysctl", "-n", "kern.boottime"), 1024)
		if err != nil {
			return "", err
		}
		match := darwinBootTimePattern.FindSubmatch(raw)
		if len(match) != 3 {
			return "", errors.New("macOS boot identity is unavailable")
		}
		return fmt.Sprintf("darwin-%s-%s", match[1], match[2]), nil
	default:
		return "", fmt.Errorf("boot identity is unsupported on %s", runtime.GOOS)
	}
}

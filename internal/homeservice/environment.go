package homeservice

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
	"unicode"

	"github.com/codemoo/hmux/internal/safeexec"
)

func cleanValue(value string) bool {
	return value != "" && len(value) <= 32768 && strings.IndexFunc(value, unicode.IsControl) < 0
}

func serviceEnvironment(home string) (map[string]string, error) {
	source := make(map[string]string)
	for _, key := range serviceEnvKeys {
		source[key] = os.Getenv(key)
	}
	return serviceEnvironmentFrom(home, source)
}

func serviceEnvironmentFrom(home string, source map[string]string) (map[string]string, error) {
	if source["HOME"] != "" && source["HOME"] != home {
		return nil, errors.New("connector HOME differs from the installation account; use the original account/environment")
	}
	if !systemExecutable("/usr/bin/id") || !systemExecutable("/usr/bin/env") {
		return nil, errors.New("trusted system id/env executables are required")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	raw, err := safeexec.Output(exec.CommandContext(ctx, "/usr/bin/id", "-un"), 256)
	name := strings.TrimSpace(string(raw))
	if err != nil || !cleanValue(name) {
		return nil, errors.New("cannot resolve the service account name")
	}
	env := map[string]string{"HOME": home, "USER": name, "LOGNAME": name}
	for _, key := range serviceEnvKeys {
		if value := source[key]; value != "" {
			if !cleanValue(value) {
				return nil, fmt.Errorf("invalid service environment value for %s", key)
			}
			if key != "PATH" && key != "LANG" && !strings.HasPrefix(key, "LC_") && !filepath.IsAbs(value) {
				return nil, fmt.Errorf("%s must be an absolute path", key)
			}
			env[key] = value
		}
	}
	if env["PATH"] == "" {
		return nil, errors.New("PATH is required; install from the terminal where tmux and provider CLIs work")
	}
	for _, part := range filepath.SplitList(env["PATH"]) {
		if !filepath.IsAbs(part) {
			return nil, errors.New("service PATH must contain only absolute directories")
		}
	}
	return env, nil
}

func executableInPath(name, path string) bool {
	for _, dir := range filepath.SplitList(path) {
		if info, err := os.Stat(filepath.Join(dir, name)); err == nil && info.Mode().IsRegular() && info.Mode().Perm()&0111 != 0 {
			return true
		}
	}
	return false
}

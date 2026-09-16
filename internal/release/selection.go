package release

import (
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

const (
	updateCheckStateName       = "last-app-update-check"
	agentUpdateCheckStateName  = "last-agent-update-check"
	nativeUpdateCheckStateName = "last-native-app-update-check"
)

// SelectedVersion returns only a fully reverified immutable cached release.
func SelectedVersion(cfg config.ClientConfig) (string, error) {
	base := filepath.Clean(cfg.CacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return "", errors.New("unsafe cache directory")
	}
	var version string
	err := withUpdateLock(base, func() error {
		var err error
		version, err = selectedVersionUnlocked(cfg)
		return err
	})
	return version, err
}

func selectedVersionUnlocked(cfg config.ClientConfig) (string, error) {
	base := filepath.Clean(cfg.CacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return "", errors.New("unsafe cache directory")
	}
	target, err := os.Readlink(filepath.Join(base, "current"))
	if err != nil {
		return "", err
	}
	version := filepath.Base(filepath.Dir(target))
	if target != filepath.Join("releases", version, "hmux") || !ValidVersion(version) {
		return "", errors.New("current release link has an invalid target")
	}
	if err := verifyCachedUnlocked(cfg, version); err != nil {
		return "", err
	}
	return version, nil
}

// SelectedExecutable returns the exact immutable path that was reverified
// under the update lock. Callers must not reopen the mutable current symlink.
func SelectedExecutable(cfg config.ClientConfig) (string, string, error) {
	base := filepath.Clean(cfg.CacheDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return "", "", errors.New("unsafe cache directory")
	}
	var version string
	err := withUpdateLock(base, func() error {
		var err error
		version, err = selectedVersionUnlocked(cfg)
		return err
	})
	if err != nil {
		return "", "", err
	}
	return version, filepath.Join(base, "releases", version, "hmux"), nil
}

// IsNewerVersion intentionally refuses non-release versions and downgrades.
func IsNewerVersion(candidate, current string) bool {
	if !ValidVersion(candidate) || !ValidVersion(current) {
		return false
	}
	candidateCore, candidatePrerelease := versionPrecedenceParts(candidate)
	currentCore, currentPrerelease := versionPrecedenceParts(current)
	for index := 0; index < 4; index++ {
		left := versionNumber(candidateCore, index)
		right := versionNumber(currentCore, index)
		if left != right {
			return left > right
		}
	}
	if candidatePrerelease == currentPrerelease {
		return false
	}
	// A stable release is newer than a prerelease with the same numeric core.
	if candidatePrerelease == "" {
		return true
	}
	if currentPrerelease == "" {
		return false
	}
	return comparePrerelease(candidatePrerelease, currentPrerelease) > 0
}

func AppUpdateCheckDue(cacheDir string, now time.Time, interval time.Duration) bool {
	return updateCheckDue(cacheDir, updateCheckStateName, now, interval)
}

func AgentUpdateCheckDue(stateDir string, now time.Time, interval time.Duration) bool {
	return updateCheckDue(stateDir, agentUpdateCheckStateName, now, interval)
}

func NativeAppUpdateCheckDue(cacheDir string, now time.Time, interval time.Duration) bool {
	return updateCheckDue(cacheDir, nativeUpdateCheckStateName, now, interval)
}

func updateCheckDue(baseDir, stateName string, now time.Time, interval time.Duration) bool {
	if interval < time.Minute {
		return false
	}
	base := filepath.Clean(baseDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return true
	}
	path := filepath.Join(base, stateName)
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return true
	}
	if err != nil || !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 ||
		info.Mode().Perm()&0o022 != 0 || info.Size() < 1 || info.Size() > 64 {
		return true
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return true
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return true
	}
	unix, err := strconv.ParseInt(strings.TrimSpace(string(data)), 10, 64)
	if err != nil || unix < 1 {
		return true
	}
	last := time.Unix(unix, 0)
	return last.After(now.Add(5*time.Minute)) || now.Sub(last) >= interval
}

func RecordAppUpdateCheck(cacheDir string, now time.Time) error {
	return recordUpdateCheck(cacheDir, updateCheckStateName, now)
}

func RecordAgentUpdateCheck(stateDir string, now time.Time) error {
	return recordUpdateCheck(stateDir, agentUpdateCheckStateName, now)
}

func RecordNativeAppUpdateCheck(cacheDir string, now time.Time) error {
	return recordUpdateCheck(cacheDir, nativeUpdateCheckStateName, now)
}

func recordUpdateCheck(baseDir, stateName string, now time.Time) error {
	base := filepath.Clean(baseDir)
	if !filepath.IsAbs(base) || base == string(os.PathSeparator) {
		return errors.New("unsafe update state directory")
	}
	return config.AtomicWrite(
		filepath.Join(base, stateName),
		[]byte(strconv.FormatInt(now.Unix(), 10)+"\n"),
		0o600,
	)
}

func versionPrecedenceParts(value string) (string, string) {
	withoutBuild := value
	if index := strings.IndexByte(withoutBuild, '+'); index >= 0 {
		withoutBuild = withoutBuild[:index]
	}
	if index := strings.IndexByte(withoutBuild, '-'); index >= 0 {
		return withoutBuild[:index], withoutBuild[index+1:]
	}
	return withoutBuild, ""
}

func comparePrerelease(left, right string) int {
	leftParts := strings.Split(left, ".")
	rightParts := strings.Split(right, ".")
	count := min(len(leftParts), len(rightParts))
	for index := 0; index < count; index++ {
		leftNumber, leftNumeric := prereleaseNumber(leftParts[index])
		rightNumber, rightNumeric := prereleaseNumber(rightParts[index])
		switch {
		case leftNumeric && rightNumeric && leftNumber != rightNumber:
			if leftNumber > rightNumber {
				return 1
			}
			return -1
		case leftNumeric != rightNumeric:
			if leftNumeric {
				return -1
			}
			return 1
		case !leftNumeric && leftParts[index] != rightParts[index]:
			if leftParts[index] > rightParts[index] {
				return 1
			}
			return -1
		}
	}
	if len(leftParts) > len(rightParts) {
		return 1
	}
	if len(leftParts) < len(rightParts) {
		return -1
	}
	return 0
}

func prereleaseNumber(value string) (int64, bool) {
	if value == "" {
		return 0, false
	}
	for _, char := range value {
		if char < '0' || char > '9' {
			return 0, false
		}
	}
	number, err := strconv.ParseInt(value, 10, 64)
	return number, err == nil
}

func versionNumber(core string, index int) int64 {
	parts := strings.Split(core, ".")
	if index >= len(parts) {
		return 0
	}
	value, _ := strconv.ParseInt(parts[index], 10, 64)
	return value
}

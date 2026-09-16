package wire

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"unicode"
)

const (
	minimumBearerTokenLength = 32
	maxBearerTokenFileSize   = 64 << 10
)

// BearerTokens holds the daemon's per-provider HTTP route tokens.
type BearerTokens struct {
	Claude string `json:"claude"`
	Codex  string `json:"codex"`
}

// Token returns the token for a provider.
func (b BearerTokens) Token(provider Provider) string {
	switch provider {
	case ProviderClaude:
		return b.Claude
	case ProviderCodex:
		return b.Codex
	}
	return ""
}

// IsAuthorized checks an Authorization header value against an expected
// token. Uses constant-time compare so the daemon doesn't leak token bytes
// via response-time timing — Swift used `==` which would, but the route is
// protected by network reachability so the practical exposure was small.
// Now it's just safe.
func IsAuthorized(authorizationHeader, expectedToken string) bool {
	const prefix = "Bearer "
	if !isUsableBearerToken(expectedToken) || !strings.HasPrefix(authorizationHeader, prefix) {
		return false
	}
	given := authorizationHeader[len(prefix):]
	return subtle.ConstantTimeCompare([]byte(given), []byte(expectedToken)) == 1
}

// LoadOrCreateBearerTokens reads per-provider environment overrides and falls
// back to ~/.config/token-usage/tokens.json. A token file is not touched when
// both valid overrides are present. Invalid explicit overrides fail closed
// instead of silently falling back to a file.
//
// Returns (tokens, created, path, err). Path is empty when only environment
// overrides are used.
func LoadOrCreateBearerTokens() (BearerTokens, bool, string, error) {
	envClaude, hasEnvClaude, err := environmentBearerToken("TOKEN_USAGE_CLAUDE_TOKEN", "Claude")
	if err != nil {
		return BearerTokens{}, false, "", err
	}
	envCodex, hasEnvCodex, err := environmentBearerToken("TOKEN_USAGE_CODEX_TOKEN", "Codex")
	if err != nil {
		return BearerTokens{}, false, "", err
	}

	if hasEnvClaude && hasEnvCodex {
		tokens := BearerTokens{Claude: envClaude, Codex: envCodex}
		if err := validateBearerTokens(tokens, "environment overrides"); err != nil {
			return BearerTokens{}, false, "", err
		}
		return tokens, false, "", nil
	}

	path, err := defaultBearerTokenPath()
	if err != nil {
		return BearerTokens{}, false, "", err
	}

	fileTokens, created, err := loadOrCreateBearerTokensFile(path)
	if err != nil {
		return BearerTokens{}, false, path, err
	}

	tokens := BearerTokens{
		Claude: pickIfSet(envClaude, hasEnvClaude, fileTokens.Claude),
		Codex:  pickIfSet(envCodex, hasEnvCodex, fileTokens.Codex),
	}
	if err := validateBearerTokens(tokens, "effective bearer token configuration"); err != nil {
		return BearerTokens{}, false, path, err
	}
	return tokens, created, path, nil
}

func environmentBearerToken(envName, provider string) (string, bool, error) {
	value, present := os.LookupEnv(envName)
	if !present {
		return "", false, nil
	}
	if err := validateBearerToken(value, provider+" environment override"); err != nil {
		return "", true, err
	}
	return value, true, nil
}

func pickIfSet(override string, present bool, fallback string) string {
	if present {
		return override
	}
	return fallback
}

func validateBearerTokens(tokens BearerTokens, source string) error {
	if err := validateBearerToken(tokens.Claude, source+" Claude token"); err != nil {
		return err
	}
	if err := validateBearerToken(tokens.Codex, source+" Codex token"); err != nil {
		return err
	}
	if subtle.ConstantTimeCompare([]byte(tokens.Claude), []byte(tokens.Codex)) == 1 {
		return fmt.Errorf("%s: Claude and Codex tokens must be different", source)
	}
	return nil
}

func validateBearerToken(token, label string) error {
	switch {
	case token == "" || strings.TrimSpace(token) == "":
		return fmt.Errorf("%s is empty", label)
	case strings.IndexFunc(token, unicode.IsSpace) >= 0:
		return fmt.Errorf("%s contains whitespace", label)
	case len(token) < minimumBearerTokenLength:
		return fmt.Errorf("%s is too short (minimum %d bytes)", label, minimumBearerTokenLength)
	default:
		return nil
	}
}

func isUsableBearerToken(token string) bool {
	return validateBearerToken(token, "bearer token") == nil
}

func defaultBearerTokenPath() (string, error) {
	home := os.Getenv("HOME")
	if home == "" {
		var err error
		home, err = os.UserHomeDir()
		if err != nil {
			return "", err
		}
	}
	return filepath.Join(home, ".config", "token-usage", "tokens.json"), nil
}

func loadOrCreateBearerTokensFile(path string) (BearerTokens, bool, error) {
	tokens, err := readBearerTokensFile(path)
	if err == nil {
		return tokens, false, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return BearerTokens{}, false, err
	}
	return generateBearerTokensFile(path)
}

func readBearerTokensFile(path string) (BearerTokens, error) {
	data, err := readPrivateRegularFile(path)
	if err != nil {
		return BearerTokens{}, err
	}
	var t BearerTokens
	if err := json.Unmarshal(data, &t); err != nil {
		return BearerTokens{}, fmt.Errorf("parse %s: %w", path, err)
	}
	if err := validateBearerTokens(t, "bearer token file"); err != nil {
		return BearerTokens{}, fmt.Errorf("validate %s: %w", path, err)
	}
	return t, nil
}

func readPrivateRegularFile(path string) ([]byte, error) {
	pathInfo, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !pathInfo.Mode().IsRegular() {
		return nil, fmt.Errorf("bearer token path %s is not a regular file", path)
	}

	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	fileInfo, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !fileInfo.Mode().IsRegular() || !os.SameFile(pathInfo, fileInfo) {
		return nil, fmt.Errorf("bearer token file %s changed while opening", path)
	}

	// Windows protects files with ACLs and does not expose meaningful
	// group/world permission bits through os.FileMode. macOS, Linux, and other
	// Unix targets require an owner-only file; an insecure mode is repaired
	// before any token bytes are read.
	if runtime.GOOS != "windows" && fileInfo.Mode().Perm()&0o077 != 0 {
		if err := file.Chmod(0o600); err != nil {
			return nil, fmt.Errorf("secure bearer token file %s: %w", path, err)
		}
		fileInfo, err = file.Stat()
		if err != nil {
			return nil, err
		}
		if fileInfo.Mode().Perm() != 0o600 {
			return nil, fmt.Errorf("secure bearer token file %s: mode is %#o after chmod", path, fileInfo.Mode().Perm())
		}
	}

	data, err := io.ReadAll(io.LimitReader(file, maxBearerTokenFileSize+1))
	if err != nil {
		return nil, fmt.Errorf("read bearer token file %s: %w", path, err)
	}
	if len(data) > maxBearerTokenFileSize {
		return nil, fmt.Errorf("read bearer token file %s: exceeds maximum size of %d bytes", path, maxBearerTokenFileSize)
	}
	return data, nil
}

func generateBearerTokensFile(path string) (BearerTokens, bool, error) {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return BearerTokens{}, false, err
	}
	tokens := BearerTokens{
		Claude: mustRandomToken(),
		Codex:  mustRandomToken(),
	}
	if err := validateBearerTokens(tokens, "generated bearer tokens"); err != nil {
		return BearerTokens{}, false, err
	}
	data, err := json.MarshalIndent(tokens, "", "  ")
	if err != nil {
		return BearerTokens{}, false, err
	}
	data = append(data, '\n')

	// CreateTemp uses O_CREATE|O_EXCL and mode 0600. Publishing with a hard
	// link is an atomic no-replace operation: exactly one concurrent starter
	// can install its complete, fsynced file. os.Rename cannot be used here
	// because it may overwrite another starter's tokens on Unix.
	temp, err := os.CreateTemp(dir, "."+filepath.Base(path)+".tmp-")
	if err != nil {
		return BearerTokens{}, false, err
	}
	tempPath := temp.Name()
	defer os.Remove(tempPath)
	tempClosed := false
	defer func() {
		if !tempClosed {
			_ = temp.Close()
		}
	}()

	if err := temp.Chmod(0o600); err != nil {
		return BearerTokens{}, false, err
	}
	if _, err := temp.Write(data); err != nil {
		return BearerTokens{}, false, err
	}
	if err := temp.Sync(); err != nil {
		return BearerTokens{}, false, err
	}
	if err := temp.Close(); err != nil {
		return BearerTokens{}, false, err
	}
	tempClosed = true

	if err := os.Link(tempPath, path); err != nil {
		if errors.Is(err, os.ErrExist) {
			winner, readErr := readBearerTokensFile(path)
			if readErr != nil {
				return BearerTokens{}, false, readErr
			}
			return winner, false, nil
		}
		return BearerTokens{}, false, fmt.Errorf("atomically publish bearer token file %s: %w", path, err)
	}
	if err := syncDirectory(dir); err != nil {
		return BearerTokens{}, false, err
	}
	return tokens, true, nil
}

func syncDirectory(path string) error {
	// Windows directory handles require different APIs; token contents are
	// already fsynced before publication, and ACL-backed permission handling is
	// likewise delegated to Windows above.
	if runtime.GOOS == "windows" {
		return nil
	}

	dir, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("open bearer token directory %s for sync: %w", path, err)
	}
	defer dir.Close()
	if err := dir.Sync(); err != nil {
		// Some Unix filesystems report EINVAL for directory fsync. Treat that as
		// unsupported; all other errors may indicate a real durability failure.
		if errors.Is(err, fs.ErrInvalid) {
			return nil
		}
		return fmt.Errorf("sync bearer token directory %s: %w", path, err)
	}
	return nil
}

func mustRandomToken() string {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		// Fail loudly: the daemon's auth depends on this. Caller will
		// surface the panic via main; rand.Read failure is a kernel
		// CSPRNG fault and continuing with a weak token is worse than
		// crashing.
		panic("crypto/rand failed: " + err.Error())
	}
	return hex.EncodeToString(b[:])
}

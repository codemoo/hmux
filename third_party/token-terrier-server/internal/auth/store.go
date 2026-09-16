package auth

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/safefile"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const maximumCredentialBytes = 4 * 1024 * 1024

// CredentialStore loads OAuth credentials. It hides where they live so the
// rest of the daemon doesn't care about the concrete file layout.
type CredentialStore struct {
	mu     sync.Mutex
	source ReadSource

	// Cached parses keyed by provider — the underlying file rarely
	// changes; reloading on every snapshot is wasteful. Cache invalidates
	// on demand (Reload) and after successful refresh.
	cache         map[wire.Provider]OAuthCredential
	cacheRevision map[wire.Provider]SourceRevision
}

// ReadSource abstracts how the daemon reaches the credential bytes. Used as
// a swappable interface so tests can use temporary local files.
type ReadSource interface {
	Read(ctx context.Context, provider wire.Provider) ([]byte, error)
	// Write persists refreshed credentials back to the source so Claude
	// Code/Codex CLI can continue to use the rotated tokens.
	Write(ctx context.Context, provider wire.Provider, body []byte) error
}

// SourceRevision identifies the credential file currently reachable at its
// configured path. Inode is essential because CLI tools replace credentials
// atomically and can preserve size and coarse modification timestamps.
type SourceRevision struct {
	Device      uint64
	Inode       uint64
	Size        int64
	ModTimeNano int64
}

// RevisionSource lets the store detect an external CLI login/account switch
// before waiting for an upstream 401 to invalidate its in-memory credential.
type RevisionSource interface {
	Revision(ctx context.Context, provider wire.Provider) (SourceRevision, error)
}

// NewCredentialStore wires the store to the given source.
func NewCredentialStore(src ReadSource) *CredentialStore {
	return &CredentialStore{
		source:        src,
		cache:         map[wire.Provider]OAuthCredential{},
		cacheRevision: map[wire.Provider]SourceRevision{},
	}
}

// Load returns the parsed credential for a provider. Caches in-memory; call
// Reload to force a re-read.
func (s *CredentialStore) Load(ctx context.Context, provider wire.Provider) (OAuthCredential, error) {
	s.mu.Lock()
	cached, cachedOK := s.cache[provider]
	cachedRevision, revisionOK := s.cacheRevision[provider]
	s.mu.Unlock()
	if cachedOK {
		revisionSource, tracksRevision := s.source.(RevisionSource)
		if !tracksRevision {
			return cached, nil
		}
		current, err := revisionSource.Revision(ctx, provider)
		if err == nil && revisionOK && current == cachedRevision {
			return cached, nil
		}
		// Missing/unreadable/replaced files all go through Reload so callers
		// receive the authoritative credential error rather than stale cache.
	}
	return s.Reload(ctx, provider)
}

// Reload forces an upstream read + parse, updating the cache.
func (s *CredentialStore) Reload(ctx context.Context, provider wire.Provider) (OAuthCredential, error) {
	revisionSource, tracksRevision := s.source.(RevisionSource)
	for attempt := 0; attempt < 2; attempt++ {
		var before SourceRevision
		var err error
		if tracksRevision {
			before, err = revisionSource.Revision(ctx, provider)
			if err != nil {
				return OAuthCredential{}, err
			}
		}
		data, err := s.source.Read(ctx, provider)
		if err != nil {
			return OAuthCredential{}, err
		}
		parsed, err := parseProvider(provider, data)
		if err != nil {
			return OAuthCredential{}, err
		}
		if tracksRevision {
			after, err := revisionSource.Revision(ctx, provider)
			if err != nil {
				return OAuthCredential{}, err
			}
			if before != after {
				continue
			}
			before = after
		}
		s.mu.Lock()
		s.cache[provider] = parsed
		if tracksRevision {
			s.cacheRevision[provider] = before
		}
		s.mu.Unlock()
		return parsed, nil
	}
	return OAuthCredential{}, fmt.Errorf("credential changed repeatedly while reading")
}

// Replace updates the in-memory cache after a refresh round-trip. The caller
// is expected to also persist the new credential via the source's Write.
func (s *CredentialStore) Replace(provider wire.Provider, c OAuthCredential) {
	s.mu.Lock()
	s.cache[provider] = c
	delete(s.cacheRevision, provider)
	s.mu.Unlock()
}

// CurrentAccountKey returns the cached credential's accountKey or "" if
// nothing is loaded yet. Lock-light for the cache hit path so the hot
// snapshot path doesn't serialize on credential I/O.
func (s *CredentialStore) CurrentAccountKey(ctx context.Context, provider wire.Provider) string {
	c, err := s.Load(ctx, provider)
	if err != nil {
		return ""
	}
	return c.AccountKey()
}

func parseProvider(provider wire.Provider, data []byte) (OAuthCredential, error) {
	switch provider {
	case wire.ProviderClaude:
		return ParseClaude(data)
	case wire.ProviderCodex:
		return ParseCodex(data)
	default:
		return OAuthCredential{}, fmt.Errorf("unsupported provider: %s", provider)
	}
}

// LocalSource reads/writes credentials from the local filesystem. Useful
// for tests and for the standalone server.
type LocalSource struct {
	ClaudePath string
	CodexPath  string
}

// Read implements ReadSource using local file IO.
func (l *LocalSource) Read(ctx context.Context, provider wire.Provider) ([]byte, error) {
	path := l.pathFor(provider)
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	snapshot, err := safefile.Read(path, maximumCredentialBytes)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, CredentialFileError{Kind: "not_found", Message: path}
		}
		return nil, err
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return snapshot.Data, nil
}

// Revision returns metadata for external account-switch detection.
func (l *LocalSource) Revision(ctx context.Context, provider wire.Provider) (SourceRevision, error) {
	path := l.pathFor(provider)
	if err := ctx.Err(); err != nil {
		return SourceRevision{}, err
	}
	info, err := safefile.Inspect(path, maximumCredentialBytes)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return SourceRevision{}, CredentialFileError{Kind: "not_found", Message: path}
		}
		return SourceRevision{}, err
	}
	revision := SourceRevision{Size: info.Size(), ModTimeNano: info.ModTime().UnixNano()}
	if stat, ok := info.Sys().(*syscall.Stat_t); ok {
		revision.Device = uint64(stat.Dev)
		revision.Inode = uint64(stat.Ino)
	}
	return revision, nil
}

// Write implements ReadSource using local file IO with atomic rename.
func (l *LocalSource) Write(ctx context.Context, provider wire.Provider, body []byte) error {
	return l.withCredentialLock(ctx, provider, func() error {
		return l.writeCredentialLocked(provider, body)
	})
}

const credentialLockTimeout = 45 * time.Second

func (l *LocalSource) withCredentialLock(ctx context.Context, provider wire.Provider, body func() error) error {
	path := l.pathFor(provider)
	if path == "" {
		return fmt.Errorf("unsupported provider: %s", provider)
	}
	directory := filepath.Dir(path)
	if err := os.MkdirAll(directory, 0o700); err != nil {
		return fmt.Errorf("create credential directory: %w", err)
	}
	lockPath := credentialLockPath(path)
	file, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return fmt.Errorf("open credential lock: %w", err)
	}
	defer file.Close()
	if err := file.Chmod(0o600); err != nil {
		return fmt.Errorf("secure credential lock: %w", err)
	}

	deadline := time.NewTimer(credentialLockTimeout)
	defer deadline.Stop()
	for {
		err = syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			break
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) && !errors.Is(err, syscall.EAGAIN) {
			return fmt.Errorf("lock credential: %w", err)
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("lock credential: %w", ctx.Err())
		case <-deadline.C:
			return fmt.Errorf("lock credential: timeout after %s", credentialLockTimeout)
		case <-time.After(50 * time.Millisecond):
		}
	}
	defer syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
	return body()
}

func (l *LocalSource) writeCredentialLocked(provider wire.Provider, body []byte) (retErr error) {
	path := l.pathFor(provider)
	if path == "" {
		return fmt.Errorf("unsupported provider: %s", provider)
	}
	if len(body) < 1 || len(body) > maximumCredentialBytes {
		return errors.New("credential payload exceeds size limit")
	}
	directory := filepath.Dir(path)
	temporary, err := os.CreateTemp(directory, "."+filepath.Base(path)+".token-terrier-*")
	if err != nil {
		return fmt.Errorf("create credential temp: %w", err)
	}
	temporaryPath := temporary.Name()
	closed := false
	defer func() {
		if !closed {
			if closeErr := temporary.Close(); retErr == nil && closeErr != nil {
				retErr = fmt.Errorf("close credential temp: %w", closeErr)
			}
		}
		if retErr != nil {
			_ = os.Remove(temporaryPath)
		}
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return fmt.Errorf("secure credential temp: %w", err)
	}
	if _, err := io.Copy(temporary, bytes.NewReader(body)); err != nil {
		return fmt.Errorf("write credential temp: %w", err)
	}
	if err := temporary.Sync(); err != nil {
		return fmt.Errorf("sync credential temp: %w", err)
	}
	closeErr := temporary.Close()
	closed = true
	if closeErr != nil {
		return fmt.Errorf("close credential temp: %w", closeErr)
	}
	if err := os.Rename(temporaryPath, path); err != nil {
		return fmt.Errorf("replace credential: %w", err)
	}
	dir, err := os.Open(directory)
	if err != nil {
		return fmt.Errorf("open credential directory: %w", err)
	}
	syncErr := dir.Sync()
	dirCloseErr := dir.Close()
	if syncErr != nil {
		return fmt.Errorf("sync credential directory: %w", syncErr)
	}
	if dirCloseErr != nil {
		return fmt.Errorf("close credential directory: %w", dirCloseErr)
	}
	return nil
}

func credentialLockPath(path string) string {
	extension := filepath.Ext(path)
	return strings.TrimSuffix(path, extension) + ".lock"
}

func (l *LocalSource) pathFor(provider wire.Provider) string {
	switch provider {
	case wire.ProviderClaude:
		return l.ClaudePath
	case wire.ProviderCodex:
		return l.CodexPath
	}
	return ""
}

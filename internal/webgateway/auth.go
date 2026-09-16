package webgateway

import (
	"bytes"
	"crypto/hmac"
	"crypto/pbkdf2"
	"crypto/rand"
	"crypto/sha1"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base32"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/config"
)

const passwordIterations = 600000
const cookieName = "__Host-hmux"
const loginLifetime = 7 * 24 * time.Hour
const seenPersistInterval = 5 * time.Minute
const sessionFileVersion = 1
const sessionFileLimit = 128 << 10
const maxSessionsPerAccount = 8

func derivePassword(password string, salt []byte) ([]byte, error) {
	return pbkdf2.Key(sha256.New, password, salt, passwordIterations, 32)
}

type Credentials struct {
	Username     string `json:"username"`
	Salt         []byte `json:"salt"`
	Hash         []byte `json:"hash"`
	TOTPSecret   string `json:"totp_secret"`
	LastStep     int64  `json:"last_step"`
	TOTPDisabled bool   `json:"totp_disabled,omitempty"`
}

func NewCredentials(username, password string) (Credentials, error) {
	if len(username) < 1 || len(username) > 80 || strings.TrimSpace(username) != username || len(password) < 8 || len(password) > 128 {
		return Credentials{}, errors.New("username required; password must be 8–128 bytes")
	}
	salt := make([]byte, 32)
	if _, err := rand.Read(salt); err != nil {
		return Credentials{}, err
	}
	secret := make([]byte, 20)
	if _, err := rand.Read(secret); err != nil {
		return Credentials{}, err
	}
	hash, err := pbkdf2.Key(sha256.New, password, salt, passwordIterations, 32)
	return Credentials{Username: username, Salt: salt, Hash: hash, TOTPSecret: base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(secret)}, err
}
func (c Credentials) EnrollmentURI() string {
	q := url.Values{"secret": {c.TOTPSecret}, "issuer": {"HMux"}, "algorithm": {"SHA1"}, "digits": {"6"}, "period": {"30"}}
	return "otpauth://totp/" + url.PathEscape("HMux:"+c.Username) + "?" + q.Encode()
}
func (c Credentials) MatchCode(code string, now time.Time) int64 {
	if len(code) != 6 {
		return -1
	}
	for _, r := range code {
		if r < '0' || r > '9' {
			return -1
		}
	}
	key, err := base32.StdEncoding.WithPadding(base32.NoPadding).DecodeString(c.TOTPSecret)
	if err != nil {
		return -1
	}
	for delta := int64(-1); delta <= 1; delta++ {
		step := now.Unix()/30 + delta
		var buf [8]byte
		binary.BigEndian.PutUint64(buf[:], uint64(step))
		mac := hmac.New(sha1.New, key)
		_, _ = mac.Write(buf[:])
		sum := mac.Sum(nil)
		offset := sum[len(sum)-1] & 15
		number := (binary.BigEndian.Uint32(sum[offset:offset+4]) & 0x7fffffff) % 1000000
		if subtle.ConstantTimeCompare([]byte(fmt.Sprintf("%06d", number)), []byte(code)) == 1 {
			return step
		}
	}
	return -1
}
func WriteCredentials(path string, c Credentials) error {
	raw, err := json.Marshal(c)
	if err != nil {
		return err
	}
	return config.AtomicWrite(path, raw, 0600)
}
func readPrivate(path string, limit int64) ([]byte, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > limit {
		return nil, errors.New("secret file must be a private regular file (0600)")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Getuid() {
		return nil, errors.New("secret file must be owned by current user")
	}
	return os.ReadFile(path)
}
func LoadCredentials(path string) (Credentials, error) {
	var c Credentials
	raw, err := readPrivate(path, 4096)
	if err != nil {
		return c, err
	}
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	if err = d.Decode(&c); err != nil {
		return c, err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return c, errors.New("trailing credentials")
	}
	secret, e := base32.StdEncoding.WithPadding(base32.NoPadding).DecodeString(c.TOTPSecret)
	if c.Username == "" || len(c.Salt) != 32 || len(c.Hash) != 32 || e != nil || len(secret) != 20 {
		return c, errors.New("invalid credentials file")
	}
	return c, nil
}
func LoadToken(path string) (string, error) {
	raw, err := readPrivate(path, 256)
	if err != nil {
		return "", err
	}
	token := strings.TrimSpace(string(raw))
	b, err := base64.RawURLEncoding.DecodeString(token)
	if err != nil || len(b) != 32 {
		return "", errors.New("connector token must encode 32 random bytes")
	}
	return token, nil
}
func RandomToken() string {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	return base64.RawURLEncoding.EncodeToString(b[:])
}

type loginSession struct {
	id, username, profile string
	browser, ip           string
	created, expires      time.Time
	seen, persistedSeen   time.Time
	fingerprint           string
	done                  chan struct{}
	doneOnce              *sync.Once
}

func (s *loginSession) cancel() {
	if s.doneOnce == nil {
		s.doneOnce = &sync.Once{}
	}
	s.doneOnce.Do(func() { close(s.done) })
}

type persistedSession struct {
	TokenHash             string    `json:"token_hash"`
	ID                    string    `json:"id"`
	Username              string    `json:"username"`
	Profile               string    `json:"profile"`
	CredentialFingerprint string    `json:"credential_fingerprint"`
	Browser               string    `json:"browser"`
	IP                    string    `json:"ip"`
	CreatedAt             time.Time `json:"created_at"`
	LastSeenAt            time.Time `json:"last_seen_at"`
	ExpiresAt             time.Time `json:"expires_at"`
}

type persistedSessionFile struct {
	Version  int                `json:"version"`
	Sessions []persistedSession `json:"sessions"`
}

type sessionInfo struct {
	ID         string    `json:"id"`
	Browser    string    `json:"browser"`
	IP         string    `json:"ip"`
	Location   string    `json:"location,omitempty"`
	CreatedAt  time.Time `json:"created_at"`
	LastSeenAt time.Time `json:"last_seen_at"`
	ExpiresAt  time.Time `json:"expires_at"`
	Current    bool      `json:"current"`
}
type additionalAccount struct {
	credentials Credentials
	path        string
	profile     string
}
type authStore struct {
	accounts         map[string]additionalAccount
	mu               sync.Mutex
	credentials      Credentials
	path             string
	sessionPath      string
	sessions         map[string]*loginSession
	attempts         map[string][]time.Time
	securityAttempts map[string][]time.Time
	hashing          chan struct{}
	hashPassword     func(string, []byte) ([]byte, error)
	storageErr       error
}

func newAuth(path string) (*authStore, error) {
	c, err := LoadCredentials(path)
	if err != nil {
		return nil, err
	}
	a := &authStore{credentials: c, path: path, sessionPath: path + ".sessions", accounts: map[string]additionalAccount{}, sessions: map[string]*loginSession{}, attempts: map[string][]time.Time{}, securityAttempts: map[string][]time.Time{}, hashing: make(chan struct{}, 2), hashPassword: derivePassword}
	dir := path + ".users"
	entries, err := os.ReadDir(dir)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	if err == nil {
		info, err := os.Lstat(dir)
		if err != nil {
			return nil, err
		}
		stat, ok := info.Sys().(*syscall.Stat_t)
		if !ok || int(stat.Uid) != os.Getuid() || !info.IsDir() || info.Mode().Perm()&0077 != 0 {
			return nil, errors.New("account directory must be private and owner controlled")
		}
		if len(entries) > 8 {
			return nil, errors.New("too many configured accounts")
		}
		for _, entry := range entries {
			if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" {
				return nil, errors.New("invalid account file")
			}
			filename := filepath.Join(dir, entry.Name())
			credentials, err := LoadCredentials(filename)
			if err != nil {
				return nil, err
			}
			if credentials.Username == c.Username {
				return nil, errors.New("duplicate account")
			}
			if _, exists := a.accounts[credentials.Username]; exists {
				return nil, errors.New("duplicate account")
			}
			digest := sha256.Sum256([]byte(credentials.Username))
			a.accounts[credentials.Username] = additionalAccount{credentials: credentials, path: filename, profile: fmt.Sprintf("%x", digest)}
		}
	}
	if err := a.loadSessions(time.Now().UTC()); err != nil {
		return nil, err
	}
	return a, nil
}
func sessionKey(token string) string { hash := sha256.Sum256([]byte(token)); return string(hash[:]) }

func csrfToken(token string) string {
	digest := sha256.Sum256(append([]byte("hmux csrf\x00"), []byte(token)...))
	return base64.RawURLEncoding.EncodeToString(digest[:])
}

func credentialFingerprint(c Credentials) string {
	stable := struct {
		Username     string `json:"username"`
		Salt         []byte `json:"salt"`
		Hash         []byte `json:"hash"`
		TOTPSecret   string `json:"totp_secret"`
		TOTPDisabled bool   `json:"totp_disabled,omitempty"`
	}{c.Username, c.Salt, c.Hash, c.TOTPSecret, c.TOTPDisabled}
	raw, _ := json.Marshal(stable)
	digest := sha256.Sum256(raw)
	return base64.RawURLEncoding.EncodeToString(digest[:])
}

func (a *authStore) configuredAccount(username string) (Credentials, string, bool) {
	if username == a.credentials.Username {
		return a.credentials, "", true
	}
	account, ok := a.accounts[username]
	return account.credentials, account.profile, ok
}

func (a *authStore) setCredentialsLocked(username, profile string, credentials Credentials) {
	if profile == "" {
		a.credentials = credentials
		return
	}
	account := a.accounts[username]
	account.credentials = credentials
	a.accounts[username] = account
}

func validSessionText(value string, max int) bool {
	if value == "" || len(value) > max || !utf8.ValidString(value) {
		return false
	}
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}

func (a *authStore) loadSessions(now time.Time) error {
	raw, err := readPrivate(a.sessionPath, sessionFileLimit)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	var file persistedSessionFile
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	if err := d.Decode(&file); err != nil {
		return err
	}
	if d.Decode(&struct{}{}) != io.EOF || file.Version != sessionFileVersion {
		return errors.New("invalid session file")
	}
	dirty := false
	ids := map[string]bool{}
	counts := map[string]int{}
	for _, stored := range file.Sessions {
		hash, hashErr := base64.RawURLEncoding.DecodeString(stored.TokenHash)
		publicID, idErr := base64.RawURLEncoding.DecodeString(stored.ID)
		credentials, profile, configured := a.configuredAccount(stored.Username)
		validIP := stored.IP == "unknown" || net.ParseIP(stored.IP) != nil
		validTimes := !stored.CreatedAt.IsZero() && stored.ExpiresAt.Equal(stored.CreatedAt.Add(loginLifetime)) &&
			!stored.LastSeenAt.Before(stored.CreatedAt) && !stored.LastSeenAt.After(stored.ExpiresAt) &&
			!stored.CreatedAt.After(now.Add(5*time.Minute))
		if hashErr != nil || len(hash) != sha256.Size || idErr != nil || len(publicID) != 32 || ids[stored.ID] ||
			!validSessionText(stored.Browser, 80) || !validIP || !validTimes {
			return errors.New("invalid session file")
		}
		ids[stored.ID] = true
		if !configured || stored.Profile != profile || stored.CredentialFingerprint != credentialFingerprint(credentials) || !now.Before(stored.ExpiresAt) {
			dirty = true
			continue
		}
		accountKey := stored.Username + "\x00" + stored.Profile
		counts[accountKey]++
		if counts[accountKey] > maxSessionsPerAccount {
			return errors.New("too many persisted sessions for account")
		}
		key := string(hash)
		if _, duplicate := a.sessions[key]; duplicate {
			return errors.New("duplicate persisted session")
		}
		a.sessions[key] = &loginSession{
			id: stored.ID, username: stored.Username, profile: stored.Profile,
			browser: stored.Browser, ip: stored.IP, fingerprint: stored.CredentialFingerprint,
			created: stored.CreatedAt.UTC(), seen: stored.LastSeenAt.UTC(), persistedSeen: stored.LastSeenAt.UTC(),
			expires: stored.ExpiresAt.UTC(), done: make(chan struct{}), doneOnce: &sync.Once{},
		}
	}
	if dirty {
		return a.saveSessionMap(a.sessions)
	}
	return nil
}

func cloneSessionMap(source map[string]*loginSession) map[string]*loginSession {
	result := make(map[string]*loginSession, len(source))
	for key, session := range source {
		result[key] = session
	}
	return result
}

func (a *authStore) saveSessionMap(sessions map[string]*loginSession) error {
	if _, err := os.Lstat(a.sessionPath); err == nil {
		if _, err := readPrivate(a.sessionPath, sessionFileLimit); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	file := persistedSessionFile{Version: sessionFileVersion, Sessions: make([]persistedSession, 0, len(sessions))}
	for key, session := range sessions {
		file.Sessions = append(file.Sessions, persistedSession{
			TokenHash: base64.RawURLEncoding.EncodeToString([]byte(key)), ID: session.id,
			Username: session.username, Profile: session.profile, CredentialFingerprint: session.fingerprint,
			Browser: session.browser, IP: session.ip, CreatedAt: session.created.UTC(),
			LastSeenAt: session.seen.UTC(), ExpiresAt: session.expires.UTC(),
		})
	}
	sort.Slice(file.Sessions, func(i, j int) bool { return file.Sessions[i].ID < file.Sessions[j].ID })
	raw, err := json.Marshal(file)
	if err != nil {
		return err
	}
	if len(raw) > sessionFileLimit {
		return errors.New("session file too large")
	}
	return config.AtomicWrite(a.sessionPath, raw, 0600)
}

func (a *authStore) failStorageLocked(err error) {
	if err == nil {
		return
	}
	a.storageErr = err
	for _, session := range a.sessions {
		session.cancel()
	}
}

type loginStatus uint8

const (
	loginInvalid loginStatus = iota
	loginSucceeded
	loginTOTPRequired
	loginRateLimited
	loginStorageUnavailable
)

func (a *authStore) login(username, password, code string, now time.Time, source ...string) (string, bool) {
	token, status := a.loginWithChallenge(username, password, code, now, source...)
	return token, status == loginSucceeded
}

func (a *authStore) loginWithChallenge(username, password, code string, now time.Time, source ...string) (string, loginStatus) {
	origin := "local"
	ip := "unknown"
	browser := "Unknown browser"
	if len(source) > 0 {
		origin = source[0]
		ip = source[0]
	}
	if len(source) > 1 {
		ip = source[1]
	}
	if len(source) > 2 {
		browser = source[2]
	}
	if len(origin) > 80 {
		origin = "unknown"
	}
	if ip != "unknown" && net.ParseIP(ip) == nil {
		ip = "unknown"
	}
	if !validSessionText(browser, 80) {
		browser = "Unknown browser"
	}
	if len(password) > 128 || len(username) > 80 {
		return "", loginInvalid
	}
	now = now.UTC()
	a.mu.Lock()
	if a.storageErr != nil {
		a.mu.Unlock()
		return "", loginStorageUnavailable
	}
	for key, attempts := range a.attempts {
		if len(attempts) == 0 || now.Sub(attempts[len(attempts)-1]) >= time.Minute {
			delete(a.attempts, key)
		}
	}
	fresh := a.attempts[origin][:0]
	for _, t := range a.attempts[origin] {
		if now.Sub(t) < time.Minute {
			fresh = append(fresh, t)
		}
	}
	if len(fresh) >= 5 {
		a.attempts[origin] = fresh
		a.mu.Unlock()
		return "", loginRateLimited
	}
	// Bounded source table; changing IPs cannot allocate unlimited server memory.
	if len(a.attempts) >= 1024 && len(fresh) == 0 {
		a.mu.Unlock()
		return "", loginRateLimited
	}
	a.attempts[origin] = append(fresh, now)
	credentials := a.credentials
	accountPath := a.path
	profile := ""
	if account, ok := a.accounts[username]; ok {
		credentials = account.credentials
		accountPath = account.path
		profile = account.profile
	}
	snapshotFingerprint := credentialFingerprint(credentials)
	a.mu.Unlock()
	// Never let password hashing block existing authenticated sessions.
	select {
	case a.hashing <- struct{}{}:
	default:
		return "", loginRateLimited
	}
	defer func() { <-a.hashing }()
	hash, err := a.hashPassword(password, credentials.Salt)
	if err != nil || subtle.ConstantTimeCompare(hash, credentials.Hash) != 1 || subtle.ConstantTimeCompare([]byte(username), []byte(credentials.Username)) != 1 {
		return "", loginInvalid
	}
	step := int64(-1)
	if !credentials.TOTPDisabled && code != "" {
		step = credentials.MatchCode(code, now)
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil {
		return "", loginStorageUnavailable
	}
	current, currentProfile, configured := a.configuredAccount(username)
	if !configured || currentProfile != profile || credentialFingerprint(current) != snapshotFingerprint {
		return "", loginInvalid
	}
	if !current.TOTPDisabled {
		if code == "" {
			return "", loginTOTPRequired
		}
		if step < 0 || step <= current.LastStep {
			return "", loginInvalid
		}
	}
	next := current
	if !current.TOTPDisabled {
		next.LastStep = step
		if WriteCredentials(accountPath, next) != nil {
			return "", loginStorageUnavailable
		}
		a.setCredentialsLocked(username, profile, next)
	}
	if err := a.pruneLocked(now); err != nil {
		return "", loginStorageUnavailable
	}
	count := 0
	for _, v := range a.sessions {
		if v.username == username {
			count++
		}
	}
	candidate := cloneSessionMap(a.sessions)
	var evicted *loginSession
	var evictedKey string
	if count >= maxSessionsPerAccount {
		var oldest string
		var seen time.Time
		for k, v := range a.sessions {
			if v.username != username {
				continue
			}
			if oldest == "" || v.seen.Before(seen) {
				oldest = k
				seen = v.seen
			}
		}
		evictedKey = oldest
		evicted = a.sessions[oldest]
		delete(candidate, oldest)
	}
	var token, key string
	for {
		token = RandomToken()
		key = sessionKey(token)
		if _, exists := candidate[key]; !exists {
			break
		}
	}
	var id string
	for {
		id = RandomToken()
		duplicate := false
		for _, session := range candidate {
			if session.id == id {
				duplicate = true
				break
			}
		}
		if !duplicate {
			break
		}
	}
	created := now
	session := &loginSession{id: id, username: username, profile: profile, browser: browser, ip: ip,
		fingerprint: credentialFingerprint(next), created: created, expires: created.Add(loginLifetime),
		seen: created, persistedSeen: created, done: make(chan struct{}), doneOnce: &sync.Once{}}
	candidate[key] = session
	if err := a.saveSessionMap(candidate); err != nil {
		a.failStorageLocked(err)
		return "", loginStorageUnavailable
	}
	if evicted != nil {
		evicted.cancel()
		delete(a.sessions, evictedKey)
	}
	a.sessions[key] = session
	return token, loginSucceeded
}

func (a *authStore) prune(now time.Time) {
	a.mu.Lock()
	defer a.mu.Unlock()
	_ = a.pruneLocked(now.UTC())
}

func (a *authStore) pruneLocked(now time.Time) error {
	var expired []string
	for k, s := range a.sessions {
		if !now.Before(s.expires) {
			expired = append(expired, k)
		}
	}
	if len(expired) == 0 {
		return nil
	}
	candidate := cloneSessionMap(a.sessions)
	for _, key := range expired {
		delete(candidate, key)
	}
	if err := a.saveSessionMap(candidate); err != nil {
		for _, key := range expired {
			a.sessions[key].cancel()
			delete(a.sessions, key)
		}
		a.failStorageLocked(err)
		return err
	}
	for _, key := range expired {
		a.sessions[key].cancel()
		delete(a.sessions, key)
	}
	return nil
}
func (a *authStore) get(token string, touch bool) (string, <-chan struct{}, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil {
		return "", nil, false
	}
	now := time.Now().UTC()
	if a.pruneLocked(now) != nil {
		return "", nil, false
	}
	s := a.sessions[sessionKey(token)]
	if s == nil {
		return "", nil, false
	}
	if touch {
		if now.Sub(s.persistedSeen) >= seenPersistInterval {
			next := *s
			next.seen = now
			next.persistedSeen = now
			candidate := cloneSessionMap(a.sessions)
			candidate[sessionKey(token)] = &next
			if err := a.saveSessionMap(candidate); err != nil {
				a.failStorageLocked(err)
				return "", nil, false
			}
			a.sessions[sessionKey(token)] = &next
			s = &next
		} else {
			s.seen = now
		}
	}
	return csrfToken(token), s.done, true
}
func (a *authStore) logout(token string) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil {
		return a.storageErr
	}
	key := sessionKey(token)
	if s := a.sessions[key]; s != nil {
		candidate := cloneSessionMap(a.sessions)
		delete(candidate, key)
		if err := a.saveSessionMap(candidate); err != nil {
			a.failStorageLocked(err)
			return err
		}
		s.cancel()
		delete(a.sessions, key)
	}
	return nil
}

func (a *authStore) identity(token string) (string, string, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil || a.pruneLocked(time.Now().UTC()) != nil {
		return "", "", false
	}
	session := a.sessions[sessionKey(token)]
	if session == nil {
		return "", "", false
	}
	return session.username, session.profile, true
}

func (a *authStore) listSessions(token string) ([]sessionInfo, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil || a.pruneLocked(time.Now().UTC()) != nil {
		return nil, false
	}
	key := sessionKey(token)
	current := a.sessions[key]
	if current == nil {
		return nil, false
	}
	result := make([]sessionInfo, 0, maxSessionsPerAccount)
	for candidateKey, session := range a.sessions {
		if session.username != current.username || session.profile != current.profile {
			continue
		}
		result = append(result, sessionInfo{ID: session.id, Browser: session.browser, IP: session.ip,
			CreatedAt: session.created, LastSeenAt: session.seen, ExpiresAt: session.expires, Current: candidateKey == key})
	}
	sort.Slice(result, func(i, j int) bool {
		if result[i].Current != result[j].Current {
			return result[i].Current
		}
		if !result[i].LastSeenAt.Equal(result[j].LastSeenAt) {
			return result[i].LastSeenAt.After(result[j].LastSeenAt)
		}
		return result[i].ID < result[j].ID
	})
	return result, true
}

var errSessionNotFound = errors.New("session not found")

func (a *authStore) revoke(token, id string) (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil {
		return false, a.storageErr
	}
	if a.pruneLocked(time.Now().UTC()) != nil {
		return false, a.storageErr
	}
	if decoded, err := base64.RawURLEncoding.DecodeString(id); err != nil || len(decoded) != 32 {
		return false, errSessionNotFound
	}
	currentKey := sessionKey(token)
	current := a.sessions[currentKey]
	if current == nil {
		return false, errSessionNotFound
	}
	var targetKey string
	var target *loginSession
	for key, session := range a.sessions {
		if session.id == id && session.username == current.username && session.profile == current.profile {
			targetKey, target = key, session
			break
		}
	}
	if target == nil {
		return false, errSessionNotFound
	}
	candidate := cloneSessionMap(a.sessions)
	delete(candidate, targetKey)
	if err := a.saveSessionMap(candidate); err != nil {
		a.failStorageLocked(err)
		return false, err
	}
	delete(a.sessions, targetKey)
	target.cancel()
	return targetKey == currentKey, nil
}

func (a *authStore) closeConnections() {
	a.mu.Lock()
	defer a.mu.Unlock()
	for _, session := range a.sessions {
		session.cancel()
	}
}

package webgateway

import (
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

const maxCredentialBackupsPerAccount = 16
const maxCredentialBackups = 9 * maxCredentialBackupsPerAccount

type accountSecurityStatus uint8

const (
	accountSecurityOK accountSecurityStatus = iota
	accountSecurityForbidden
	accountSecurityRateLimited
	accountSecurityStorageUnavailable
	accountSecurityStaleSession
)

func (a *authStore) totpEnabled(token string) (bool, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.storageErr != nil || a.pruneLocked(time.Now().UTC()) != nil {
		return false, false
	}
	session := a.sessions[sessionKey(token)]
	if session == nil {
		return false, false
	}
	credentials, profile, ok := a.configuredAccount(session.username)
	if !ok || profile != session.profile || session.fingerprint != credentialFingerprint(credentials) {
		return false, false
	}
	return !credentials.TOTPDisabled, true
}

func (a *authStore) setTOTPEnabled(token, password, code string, enabled bool, now time.Time) accountSecurityStatus {
	if len(password) > 128 || len(code) > 16 {
		return accountSecurityForbidden
	}
	now = now.UTC()
	key := sessionKey(token)
	a.mu.Lock()
	if a.storageErr != nil || a.pruneLocked(now) != nil {
		a.mu.Unlock()
		return accountSecurityStorageUnavailable
	}
	session := a.sessions[key]
	if session == nil {
		a.mu.Unlock()
		return accountSecurityStaleSession
	}
	credentials, profile, configured := a.configuredAccount(session.username)
	if !configured || profile != session.profile || session.fingerprint != credentialFingerprint(credentials) {
		a.mu.Unlock()
		return accountSecurityStaleSession
	}
	accountKey := session.username + "\x00" + session.profile
	if !a.allowSecurityAttemptLocked(accountKey, now) {
		a.mu.Unlock()
		return accountSecurityRateLimited
	}
	username := session.username
	accountPath := a.path
	if profile != "" {
		accountPath = a.accounts[username].path
	}
	snapshotFingerprint := credentialFingerprint(credentials)
	a.mu.Unlock()

	select {
	case a.hashing <- struct{}{}:
	default:
		return accountSecurityRateLimited
	}
	defer func() { <-a.hashing }()
	hash, err := a.hashPassword(password, credentials.Salt)
	step := credentials.MatchCode(code, now)
	if err != nil || subtle.ConstantTimeCompare(hash, credentials.Hash) != 1 || step < 0 {
		return accountSecurityForbidden
	}

	a.mu.Lock()
	defer a.mu.Unlock()
	recheckNow := time.Now().UTC()
	if now.After(recheckNow) {
		recheckNow = now
	}
	if a.storageErr != nil || a.pruneLocked(recheckNow) != nil {
		return accountSecurityStorageUnavailable
	}
	currentSession := a.sessions[key]
	if currentSession == nil || currentSession.username != username || currentSession.profile != profile {
		return accountSecurityStaleSession
	}
	current, currentProfile, configured := a.configuredAccount(username)
	if !configured || currentProfile != profile || credentialFingerprint(current) != snapshotFingerprint ||
		currentSession.fingerprint != snapshotFingerprint {
		return accountSecurityStaleSession
	}
	if step <= current.LastStep {
		return accountSecurityForbidden
	}
	if enabled == !current.TOTPDisabled {
		return accountSecurityOK
	}

	next := current
	next.TOTPDisabled = !enabled
	next.LastStep = step
	if err := a.backupCredentials(accountPath, username, current, now); err != nil {
		a.failStorageLocked(err)
		return accountSecurityStorageUnavailable
	}
	if err := WriteCredentials(accountPath, next); err != nil {
		a.failStorageLocked(err)
		return accountSecurityStorageUnavailable
	}
	a.setCredentialsLocked(username, profile, next)

	candidate := cloneSessionMap(a.sessions)
	updatedCurrent := *currentSession
	updatedCurrent.fingerprint = credentialFingerprint(next)
	candidate[key] = &updatedCurrent
	var revoked []*loginSession
	for candidateKey, candidateSession := range candidate {
		if candidateKey != key && candidateSession.username == username && candidateSession.profile == profile {
			revoked = append(revoked, candidateSession)
			delete(candidate, candidateKey)
		}
	}
	if err := a.persistSessionsLocked(candidate); err != nil {
		return accountSecurityStorageUnavailable
	}
	a.sessions = candidate
	for _, revokedSession := range revoked {
		revokedSession.cancel()
	}
	return accountSecurityOK
}

func (a *authStore) allowSecurityAttemptLocked(accountKey string, now time.Time) bool {
	for key, attempts := range a.securityAttempts {
		if len(attempts) == 0 || now.Sub(attempts[len(attempts)-1]) >= time.Minute {
			delete(a.securityAttempts, key)
		}
	}
	fresh := a.securityAttempts[accountKey][:0]
	for _, attempt := range a.securityAttempts[accountKey] {
		if now.Sub(attempt) < time.Minute {
			fresh = append(fresh, attempt)
		}
	}
	if len(fresh) >= 5 {
		a.securityAttempts[accountKey] = fresh
		return false
	}
	// At most nine configured identities can authenticate, but retain a hard cap
	// if a future account-loading path changes that invariant.
	if len(a.securityAttempts) >= 32 && len(fresh) == 0 {
		return false
	}
	a.securityAttempts[accountKey] = append(fresh, now)
	return true
}

func (a *authStore) backupCredentials(accountPath, username string, expected Credentials, now time.Time) error {
	onDisk, err := LoadCredentials(accountPath)
	if err != nil {
		return err
	}
	if credentialFingerprint(onDisk) != credentialFingerprint(expected) || onDisk.LastStep != expected.LastStep {
		return errors.New("credentials changed outside the gateway")
	}
	raw, err := readPrivate(accountPath, 4096)
	if err != nil {
		return err
	}
	backupDir := a.path + ".backups"
	if err := os.Mkdir(backupDir, 0700); err != nil && !errors.Is(err, os.ErrExist) {
		return err
	}
	info, err := os.Lstat(backupDir)
	if err != nil {
		return err
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Getuid() || !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return errors.New("credential backup directory must be private and owner controlled")
	}
	entries, err := os.ReadDir(backupDir)
	if err != nil {
		return err
	}
	prefix := fmt.Sprintf("%x-", sha256.Sum256([]byte(username)))
	type backupEntry struct {
		name string
		when time.Time
	}
	backups := make([]backupEntry, 0, len(entries))
	accountBackups := make([]backupEntry, 0, maxCredentialBackupsPerAccount)
	for _, entry := range entries {
		when, ok := credentialBackupTime(entry.Name())
		if !ok || entry.IsDir() {
			return errors.New("invalid credential backup entry")
		}
		path := filepath.Join(backupDir, entry.Name())
		if _, err := readPrivate(path, 4096); err != nil {
			return err
		}
		item := backupEntry{name: entry.Name(), when: when}
		backups = append(backups, item)
		if strings.HasPrefix(entry.Name(), prefix) {
			accountBackups = append(accountBackups, item)
		}
	}
	sort.Slice(backups, func(i, j int) bool { return backups[i].when.Before(backups[j].when) })
	sort.Slice(accountBackups, func(i, j int) bool { return accountBackups[i].when.Before(accountBackups[j].when) })
	if len(accountBackups) >= maxCredentialBackupsPerAccount {
		if err := os.Remove(filepath.Join(backupDir, accountBackups[0].name)); err != nil {
			return err
		}
		for i := range backups {
			if backups[i].name == accountBackups[0].name {
				backups = append(backups[:i], backups[i+1:]...)
				break
			}
		}
	}
	if len(backups) >= maxCredentialBackups {
		if err := os.Remove(filepath.Join(backupDir, backups[0].name)); err != nil {
			return err
		}
	}
	name := prefix + now.Format("20060102T150405.000000000Z") + ".json"
	path := filepath.Join(backupDir, name)
	if _, err := os.Lstat(path); err == nil {
		return errors.New("credential backup timestamp collision")
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return config.AtomicWrite(path, raw, 0600)
}

func credentialBackupTime(name string) (time.Time, bool) {
	if len(name) < 66 || name[64] != '-' || !strings.HasSuffix(name, ".json") {
		return time.Time{}, false
	}
	for _, r := range name[:64] {
		if (r < '0' || r > '9') && (r < 'a' || r > 'f') {
			return time.Time{}, false
		}
	}
	when, err := time.Parse("20060102T150405.000000000Z", strings.TrimSuffix(name[65:], ".json"))
	return when, err == nil
}

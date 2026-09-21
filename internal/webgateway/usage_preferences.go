package webgateway

import (
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"sync"

	"github.com/codemoo/hmux/internal/config"
)

type usageProviderPreference struct {
	Enabled bool   `json:"enabled"`
	Source  string `json:"source"`
}
type usagePreferences struct {
	Version  int                     `json:"version"`
	Revision int64                   `json:"revision"`
	Claude   usageProviderPreference `json:"claude"`
	Codex    usageProviderPreference `json:"codex"`
}

func defaultUsagePreferences() usagePreferences {
	return usagePreferences{Version: 1, Claude: usageProviderPreference{true, "cswap"}, Codex: usageProviderPreference{true, "codex-lb"}}
}
func (p usagePreferences) valid() bool {
	return p.Version == 1 && p.Revision >= 0 && p.Revision < 1<<53 && (p.Claude.Source == "cli" || p.Claude.Source == "cswap") && (p.Codex.Source == "cli" || p.Codex.Source == "codex-lb")
}

type usagePreferenceStore struct {
	mu     sync.Mutex
	dir    string
	values map[string]usagePreferences
}

func newUsagePreferenceStore(dir string) *usagePreferenceStore {
	return &usagePreferenceStore{dir: dir, values: map[string]usagePreferences{}}
}
func usagePreferenceKey(login loginSession) string {
	return fmt.Sprintf("%x", sha256.Sum256([]byte(login.username+"\x00"+login.profile)))
}
func (s *usagePreferenceStore) loadLocked(key string) (usagePreferences, error) {
	if p, ok := s.values[key]; ok {
		return p, nil
	}
	p := defaultUsagePreferences()
	raw, err := readPrivate(filepath.Join(s.dir, key+".json"), 4096)
	if err == nil {
		var stored usagePreferences
		if strictPayload(raw, &stored) != nil || !stored.valid() {
			return usagePreferences{}, errors.New("invalid usage preferences")
		}
		p = stored
	} else if !errors.Is(err, os.ErrNotExist) {
		return usagePreferences{}, err
	}
	s.values[key] = p
	return p, nil
}
func (s *usagePreferenceStore) get(login loginSession) (usagePreferences, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.loadLocked(usagePreferenceKey(login))
}

var errUsagePreferenceConflict = errors.New("usage preferences changed")

func (s *usagePreferenceStore) set(login loginSession, next usagePreferences) (usagePreferences, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !next.valid() {
		return usagePreferences{}, errors.New("invalid usage preferences")
	}
	key := usagePreferenceKey(login)
	old, err := s.loadLocked(key)
	if err != nil {
		return usagePreferences{}, err
	}
	if old.Revision != next.Revision {
		return usagePreferences{}, errUsagePreferenceConflict
	}
	next.Revision++
	if !next.valid() {
		return usagePreferences{}, errors.New("usage preference revision exhausted")
	}
	raw, err := json.Marshal(next)
	if err == nil {
		err = config.AtomicWrite(filepath.Join(s.dir, key+".json"), raw, 0600)
	}
	if err != nil {
		return usagePreferences{}, err
	}
	s.values[key] = next
	return next, nil
}
func (s *Server) usagePreferencesAPI(w http.ResponseWriter, r *http.Request, token string) {
	login, ok := s.auth.pushLogin(token)
	if !ok {
		http.Error(w, "Session expired", 401)
		return
	}
	var p usagePreferences
	var err error
	switch r.Method {
	case http.MethodGet:
		p, err = s.usagePreferences.get(login)
	case http.MethodPost:
		if decodeRequest(w, r, &p) != nil || !p.valid() {
			http.Error(w, "Invalid usage preferences", 400)
			return
		}
		p, err = s.usagePreferences.set(login, p)
	default:
		http.Error(w, "Method not allowed", 405)
		return
	}
	if errors.Is(err, errUsagePreferenceConflict) {
		http.Error(w, "다른 기기에서 설정이 변경됐습니다. 다시 불러와 주세요.", 409)
		return
	}
	if err != nil {
		http.Error(w, "Usage preferences unavailable", 503)
		return
	}
	if _, ok := s.auth.pushLogin(token); !ok {
		http.Error(w, "Session expired", 401)
		return
	}
	writeJSON(w, p)
}

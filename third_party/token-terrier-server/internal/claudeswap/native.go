package claudeswap

import (
	"bytes"
	"encoding/json"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/safefile"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// NativeReader reads cswap's existing metadata and usage cache. It never invokes
// cswap list (which can migrate state or refresh credentials), nor writes files.
type NativeReader struct {
	home      string
	fallback  *Reader
	mu        sync.Mutex
	lastCheck time.Time
	cached    []wire.AccountUsage
	updated   *string
}

func NewNativeReader(home, exportPath string, logger *slog.Logger) *NativeReader {
	return &NativeReader{home: home, fallback: NewReader(exportPath, logger)}
}
func (r *NativeReader) SetActivityProvider(p ActivityProvider) { r.fallback.SetActivityProvider(p) }
func (r *NativeReader) ActiveAccountNumber() int {
	accounts, _ := r.Accounts()
	active := 0
	for _, a := range accounts {
		if a.Active {
			if active != 0 {
				return 0
			}
			active = a.Number
		}
	}
	return active
}

type nativeIdentity struct {
	Email string `json:"email"`
	Org   string `json:"organizationUuid"`
}
type nativeRoster struct {
	Accounts map[string]nativeIdentity `json:"accounts"`
	Sequence []int                     `json:"sequence"`
}
type nativeConfig struct {
	Account struct {
		Email string `json:"emailAddress"`
		Org   string `json:"organizationUuid"`
	} `json:"oauthAccount"`
}
type nativeWindow struct {
	Pct   float64 `json:"pct"`
	Reset *string `json:"resets_at"`
}
type nativeUsage struct {
	FiveHour *nativeWindow `json:"five_hour"`
	SevenDay *nativeWindow `json:"seven_day"`
}
type nativeRow struct {
	nativeIdentity
	FetchedAt float64      `json:"fetchedAt"`
	LastGood  *nativeUsage `json:"lastGood"`
	LastError string       `json:"lastError"`
}
type nativeCache struct {
	Schema   int                  `json:"schemaVersion"`
	Accounts map[string]nativeRow `json:"accounts"`
}

func (r *NativeReader) Accounts() ([]wire.AccountUsage, *string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.lastCheck.IsZero() || time.Since(r.lastCheck) >= 2*time.Second {
		r.cached, r.updated = r.readAccounts()
		r.lastCheck = time.Now()
	}
	return append([]wire.AccountUsage(nil), r.cached...), r.updated
}

func (r *NativeReader) readAccounts() ([]wire.AccountUsage, *string) {
	root := filepath.Join(r.home, ".claude-swap-backup")
	rosterPath := filepath.Join(root, "sequence.json")
	rosterFile, err := safefile.Read(rosterPath, maximumSourceBytes)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return r.fallback.Accounts()
		}
		return nil, nil
	}
	var roster nativeRoster
	if json.Unmarshal(rosterFile.Data, &roster) != nil || len(roster.Accounts) > maximumAccounts || len(roster.Sequence) > maximumAccounts {
		return nil, nil
	}
	configPath := nativeConfigPath(r.home, os.Getenv("CLAUDE_CONFIG_DIR"))
	configFile, configErr := safefile.Read(configPath, maximumSourceBytes)
	var config nativeConfig
	if configErr == nil {
		if json.Unmarshal(configFile.Data, &config) != nil {
			config = nativeConfig{}
		}
	}
	cacheFile, cacheErr := safefile.Read(filepath.Join(root, "cache", "usage.json"), maximumSourceBytes)
	var cache nativeCache
	if cacheErr == nil {
		if json.Unmarshal(cacheFile.Data, &cache) != nil || cache.Schema != 2 || len(cache.Accounts) > maximumAccounts {
			cache = nativeCache{}
		}
	}
	now := time.Now()
	rows := make([]wire.AccountUsage, 0, len(roster.Accounts))
	seen := map[int]bool{}
	activeCount := 0
	for _, number := range roster.Sequence {
		key := strconv.Itoa(number)
		identity, ok := roster.Accounts[key]
		if !ok || number <= 0 || seen[number] {
			return nil, nil
		}
		seen[number] = true
		account := wire.AccountUsage{Number: number, Email: accountLabel(identity.Email, number), Status: "unavailable"}
		account.Active = config.Account.Email != "" && config.Account.Email == identity.Email && config.Account.Org == identity.Org
		if account.Active {
			activeCount++
		}
		row, ok := cache.Accounts[key]
		if ok && row.nativeIdentity == identity && row.LastGood != nil && row.FetchedAt > 0 {
			fetched := time.UnixMilli(int64(row.FetchedAt * 1000))
			age := now.Sub(fetched)
			if age >= -maxSourceFutureSkew && age <= defaultMaxLastGoodAge {
				account.FiveHour = nativeAccountWindow(row.LastGood.FiveHour, now)
				account.SevenDay = nativeAccountWindow(row.LastGood.SevenDay, now)
				account.Status = "ok"
				if age > 5*time.Minute || row.LastError != "" {
					account.Status = "stale"
				}
				updated := fetched.UTC().Format(time.RFC3339Nano)
				account.LastRefreshAt = &updated
			}
		}
		rows = append(rows, account)
	}
	if activeCount > 1 {
		for i := range rows {
			rows[i].Active = false
		}
	}
	// A switch or roster rewrite while reading must not mix identities.
	again, err := safefile.Read(rosterPath, maximumSourceBytes)
	if err != nil || !bytes.Equal(again.Data, rosterFile.Data) {
		return nil, nil
	}
	if configErr == nil {
		again, err = safefile.Read(configPath, maximumSourceBytes)
		if err != nil || !bytes.Equal(again.Data, configFile.Data) {
			for i := range rows {
				rows[i].Active = false
			}
		}
	}
	sort.Slice(rows, func(i, j int) bool { return rows[i].Number < rows[j].Number })
	var updated *string
	if cacheErr == nil {
		value := cacheFile.Info.ModTime().UTC().Format(time.RFC3339Nano)
		updated = &value
	}
	return rows, updated
}
func nativeAccountWindow(w *nativeWindow, now time.Time) *wire.AccountWindow {
	if w == nil {
		return nil
	}
	result, err := toWindow(&cswapWindow{Pct: w.Pct, ResetsAt: w.Reset})
	if err != nil {
		return nil
	}
	if result.ResetsAt != nil {
		if reset, err := time.Parse(time.RFC3339Nano, *result.ResetsAt); err == nil && !reset.After(now) {
			return nil
		}
	}
	return result
}

// Mirror cswap paths.get_global_config_path without following unsafe files.
func nativeConfigPath(home, configured string) string {
	configHome := filepath.Join(home, ".claude")
	base := home
	if configured != "" {
		if !filepath.IsAbs(configured) {
			return ""
		}
		configHome = configured
		base = configured
	}
	legacy := filepath.Join(configHome, ".config.json")
	if _, err := os.Lstat(legacy); !errors.Is(err, os.ErrNotExist) {
		return legacy
	}
	return filepath.Join(base, ".claude.json")
}

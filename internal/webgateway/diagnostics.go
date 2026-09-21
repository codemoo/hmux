package webgateway

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

const diagnosticLimit = 2048
const diagnosticAccountLimit = 256
const diagnosticTTL = 7 * 24 * time.Hour

var diagnosticClientID = regexp.MustCompile(`^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$`)
var diagnosticBuild = regexp.MustCompile(`^(app-[A-Za-z0-9_-]{1,64}\.js|development|unknown)$`)
var diagnosticBrowser = regexp.MustCompile(`^(Unknown browser|Edge|Chrome|Firefox|Safari)( on (Android|iOS|Windows|macOS|ChromeOS|Linux))?$`)
var diagnosticProfile = regexp.MustCompile(`^[a-f0-9]{64}$`)

// Deliberately no free-form message, stack, URL, request body or terminal text.
// Client claims are diagnostic evidence, never an authorization source.
type diagnosticEvent struct {
	Sequence   int64  `json:"sequence"`
	At         int64  `json:"at"`
	Kind       string `json:"kind"`
	Reason     string `json:"reason,omitempty"`
	Route      string `json:"route,omitempty"`
	Code       int    `json:"code,omitempty"`
	Attempt    int    `json:"attempt,omitempty"`
	RetryMS    int    `json:"retry_ms,omitempty"`
	DurationMS int    `json:"duration_ms,omitempty"`
	Line       int    `json:"line,omitempty"`
	Column     int    `json:"column,omitempty"`
	Online     bool   `json:"online"`
	Visible    bool   `json:"visible"`
	Standalone bool   `json:"standalone"`
}

type diagnosticBatch struct {
	Version int               `json:"version"`
	Client  string            `json:"client"`
	Build   string            `json:"build"`
	Events  []diagnosticEvent `json:"events"`
}

type diagnosticRecord struct {
	Account  string    `json:"account,omitempty"`
	Profile  string    `json:"profile,omitempty"`
	Client   string    `json:"client"`
	Build    string    `json:"build"`
	Browser  string    `json:"browser"`
	Received time.Time `json:"received_at"`
	diagnosticEvent
}

type diagnosticRate struct {
	start   time.Time
	batches int
}
type diagnosticStore struct {
	closing         bool
	mu              sync.Mutex
	path            string
	records         []diagnosticRecord
	rates           map[string]diagnosticRate
	revision, saved uint64
	storageError    bool
	disabled        bool
	stop            chan struct{}
	done            chan struct{}
	once            sync.Once
}

func newDiagnosticStore(path string) *diagnosticStore {
	d := &diagnosticStore{path: path, rates: make(map[string]diagnosticRate), stop: make(chan struct{}), done: make(chan struct{})}
	raw, err := readPrivate(path, 2<<20)
	if err == nil {
		var disk struct {
			Version int                `json:"version"`
			Records []diagnosticRecord `json:"records"`
		}
		decoder := json.NewDecoder(bytes.NewReader(raw))
		decoder.DisallowUnknownFields()
		// readPrivate already enforces the 2 MiB disk limit; wire payloads
		// have a separate 16 KiB limit that must not truncate retained history.
		if decoder.Decode(&disk) != nil || decoder.Decode(&struct{}{}) != io.EOF || disk.Version != 1 || !validDiagnosticRecords(disk.Records, time.Now()) {
			d.disabled = true
		} else {
			d.records = disk.Records
			d.pruneLocked(time.Now())
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		d.disabled = true
	}
	d.storageError = d.disabled
	go d.run()
	return d
}

func validDiagnosticOwner(account, profile string) bool {
	return len(account) > 0 && len(account) <= 80 && strings.TrimSpace(account) == account && (profile == "" || diagnosticProfile.MatchString(profile))
}

func validDiagnosticRecords(records []diagnosticRecord, now time.Time) bool {
	if len(records) > diagnosticLimit {
		return false
	}
	counts := make(map[[2]string]int)
	for _, row := range records {
		if !validDiagnosticOwner(row.Account, row.Profile) || !diagnosticClientID.MatchString(row.Client) || !diagnosticBuild.MatchString(row.Build) || !diagnosticBrowser.MatchString(row.Browser) || row.Received.IsZero() || row.Received.After(now.Add(5*time.Minute)) || !validDiagnosticEvent(row.diagnosticEvent, row.Received) {
			return false
		}
		key := [2]string{row.Account, row.Profile}
		counts[key]++
		if counts[key] > diagnosticAccountLimit {
			return false
		}
	}
	return true
}

func (d *diagnosticStore) pruneLocked(now time.Time) {
	kept := d.records[:0]
	for _, row := range d.records {
		if row.Received.After(now.Add(-diagnosticTTL)) {
			kept = append(kept, row)
		}
	}
	if len(kept) != len(d.records) {
		d.revision++
	}
	d.records = kept
	for key, rate := range d.rates {
		if now.Sub(rate.start) >= time.Minute {
			delete(d.rates, key)
		}
	}
}

func validDiagnosticEvent(e diagnosticEvent, now time.Time) bool {
	if e.Sequence < 1 || e.Sequence > 2147483647 || e.At < now.Add(-diagnosticTTL).UnixMilli() || e.At > now.Add(5*time.Minute).UnixMilli() {
		return false
	}
	switch e.Kind {
	case "terminal-failed", "terminal-recovered", "api-failed", "offline", "resume", "runtime-error", "unhandled-rejection":
	default:
		return false
	}
	switch e.Reason {
	case "", "network", "timeout", "limit", "unavailable", "output-overflow", "protocol", "http", "TypeError", "RangeError", "ReferenceError", "SyntaxError", "Error", "unknown":
	default:
		return false
	}
	switch e.Route {
	case "", "session", "state", "action", "sessions", "account", "push", "upload", "other":
	default:
		return false
	}
	return e.Code >= 0 && e.Code <= 4999 && e.Attempt >= 0 && e.Attempt <= 1000000 && e.RetryMS >= 0 && e.RetryMS <= 60000 && e.DurationMS >= 0 && e.DurationMS <= 86400000 && e.Line >= 0 && e.Line <= 10000000 && e.Column >= 0 && e.Column <= 10000000
}

func (d *diagnosticStore) append(login loginSession, browser string, batch diagnosticBatch, now time.Time) int {
	if !validDiagnosticOwner(login.username, login.profile) || !diagnosticBrowser.MatchString(browser) || batch.Version != 1 || !diagnosticClientID.MatchString(batch.Client) || !diagnosticBuild.MatchString(batch.Build) || len(batch.Events) < 1 || len(batch.Events) > 20 {
		return http.StatusBadRequest
	}
	for _, e := range batch.Events {
		if !validDiagnosticEvent(e, now) {
			return http.StatusBadRequest
		}
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.disabled || d.closing {
		return http.StatusServiceUnavailable
	}
	d.pruneLocked(now)
	rate := d.rates[login.id]
	if rate.start.IsZero() {
		rate.start = now
	}
	if rate.batches >= 6 || (len(d.rates) >= 1024 && rate.batches == 0) {
		return http.StatusTooManyRequests
	}
	rate.batches++
	d.rates[login.id] = rate
	for _, e := range batch.Events {
		duplicate := false
		for _, row := range d.records {
			if row.Account == login.username && row.Profile == login.profile && row.Client == batch.Client && row.Sequence == e.Sequence {
				duplicate = true
				break
			}
		}
		if duplicate {
			continue
		}
		d.records = append(d.records, diagnosticRecord{Account: login.username, Profile: login.profile, Client: batch.Client, Build: batch.Build, Browser: browser, Received: now.UTC(), diagnosticEvent: e})
	}
	// Each account has a budget; one noisy browser cannot fill the whole ring.
	count := 0
	for i := len(d.records) - 1; i >= 0; i-- {
		row := d.records[i]
		if row.Account == login.username && row.Profile == login.profile {
			count++
			if count > diagnosticAccountLimit {
				d.records = append(d.records[:i], d.records[i+1:]...)
			}
		}
	}
	if len(d.records) > diagnosticLimit {
		d.records = append([]diagnosticRecord(nil), d.records[len(d.records)-diagnosticLimit:]...)
	}
	d.revision++
	return http.StatusAccepted
}

func (d *diagnosticStore) report(login loginSession) map[string]any {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.pruneLocked(time.Now())
	rows := make([]diagnosticRecord, 0)
	counts := make(map[string]int)
	for _, row := range d.records {
		if row.Account != login.username || row.Profile != login.profile {
			continue
		}
		row.Account = ""
		row.Profile = "" // Export has no other account or private deployment identifiers.
		rows = append(rows, row)
		if row.Kind == "terminal-failed" || row.Kind == "api-failed" || row.Kind == "runtime-error" || row.Kind == "unhandled-rejection" {
			counts[row.Kind+":"+row.Reason]++
		}
	}
	return map[string]any{"version": 1, "generated_at": time.Now().UTC(), "retention_days": 7, "events": rows, "counts": counts, "storage_ok": !d.storageError, "pending_save": d.saved != d.revision}
}

func (d *diagnosticStore) persist() {
	d.mu.Lock()
	d.pruneLocked(time.Now())
	if d.disabled || d.saved == d.revision {
		d.mu.Unlock()
		return
	}
	revision := d.revision
	rows := append([]diagnosticRecord(nil), d.records...)
	d.mu.Unlock()
	raw, err := json.Marshal(struct {
		Version int                `json:"version"`
		Records []diagnosticRecord `json:"records"`
	}{1, rows})
	if err == nil {
		err = config.AtomicWrite(d.path, raw, 0600)
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	d.storageError = err != nil
	if err == nil {
		d.saved = revision
	}
}
func (d *diagnosticStore) run() {
	defer close(d.done)
	tick := time.NewTicker(10 * time.Second)
	defer tick.Stop()
	for {
		select {
		case <-d.stop:
			d.persist()
			return
		case <-tick.C:
			d.persist()
		}
	}
}
func (d *diagnosticStore) close() {
	d.once.Do(func() { d.mu.Lock(); d.closing = true; d.mu.Unlock(); close(d.stop) })
	<-d.done
}

func (s *Server) diagnosticsAPI(w http.ResponseWriter, r *http.Request, token string) {
	login, ok := s.auth.pushLogin(token)
	if !ok {
		http.Error(w, "Session expired", 401)
		return
	}
	if s.diagnostics == nil {
		http.Error(w, "Diagnostics unavailable", 503)
		return
	}
	switch r.Method {
	case http.MethodGet:
		report := s.diagnostics.report(login)
		if _, ok := s.auth.pushLogin(token); !ok {
			http.Error(w, "Session expired", 401)
			return
		}
		writeJSON(w, report)
	case http.MethodPost:
		var batch diagnosticBatch
		if decodeRequest(w, r, &batch) != nil {
			http.Error(w, "Invalid diagnostics", 400)
			return
		}
		if _, ok := s.auth.pushLogin(token); !ok {
			http.Error(w, "Session expired", 401)
			return
		}
		status := s.diagnostics.append(login, browserLabel(r.UserAgent()), batch, time.Now())
		if status == 429 {
			w.Header().Set("Retry-After", "60")
		}
		w.WriteHeader(status)
	default:
		http.Error(w, "Method not allowed", 405)
	}
}

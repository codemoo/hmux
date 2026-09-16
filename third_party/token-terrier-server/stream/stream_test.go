package stream

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/burn"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/state"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

type streamRoundTripFunc func(*http.Request) (*http.Response, error)

func (f streamRoundTripFunc) RoundTrip(request *http.Request) (*http.Response, error) {
	return f(request)
}

func TestFrameRoundTrip(t *testing.T) {
	snapshot := testSnapshot(t, wire.ProviderClaude, 7)
	var output bytes.Buffer
	encoder, err := NewEncoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	want := Frame{
		ProtocolVersion: ProtocolVersion,
		Sequence:        1,
		Type:            "snapshot",
		Provider:        "claude",
		Snapshot:        snapshot,
	}
	if err := encoder.Encode(want); err != nil {
		t.Fatal(err)
	}
	decoder, err := NewDecoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	got, err := decoder.Decode()
	if err != nil {
		t.Fatal(err)
	}
	if got.ProtocolVersion != want.ProtocolVersion || got.Sequence != want.Sequence ||
		got.Type != want.Type || got.Provider != want.Provider || !bytes.Equal(got.Snapshot, want.Snapshot) {
		t.Fatalf("round trip mismatch: %#v", got)
	}
}

func TestReadOnlyCredentialSourceRejectsWriteAndTracksExternalChange(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "claude.json")
	original := []byte(`{"claudeAiOauth":{"accessToken":"old"}}`)
	if err := os.WriteFile(path, original, 0o600); err != nil {
		t.Fatal(err)
	}
	source := newReadOnlyCredentialSource(&auth.LocalSource{ClaudePath: path})
	ctx := context.Background()
	got, err := source.Read(ctx, wire.ProviderClaude)
	if err != nil || !bytes.Equal(got, original) {
		t.Fatal(err)
	}
	if err := source.Write(ctx, wire.ProviderClaude, []byte(`{"secret":"new"}`)); err == nil {
		t.Fatal("read-only credential source accepted a write")
	}
	disk, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(disk, original) {
		t.Fatalf("disk credential changed: %q, err=%v", disk, err)
	}
	replacement := []byte(`{"claudeAiOauth":{"accessToken":"external"}}`)
	if err := os.WriteFile(path, replacement, 0o600); err != nil {
		t.Fatal(err)
	}
	got, err = source.Read(ctx, wire.ProviderClaude)
	if err != nil || !bytes.Equal(got, replacement) {
		t.Fatalf("external credential was not authoritative: %q, err=%v", got, err)
	}
}

func TestDecoderRejectsUnknownAndOversizedFrames(t *testing.T) {
	unknown := `{"protocol_version":1,"sequence":1,"type":"heartbeat","extra":true}` + "\n"
	decoder, _ := NewDecoder(strings.NewReader(unknown))
	if _, err := decoder.Decode(); err == nil {
		t.Fatal("unknown frame field accepted")
	}
	oversized := strings.Repeat("x", MaximumFrameBytes) + "\n"
	decoder, _ = NewDecoder(strings.NewReader(oversized))
	if _, err := decoder.Decode(); err == nil {
		t.Fatal("oversized frame accepted")
	}
}

func TestFrameRejectsProviderMismatchAndHeartbeatData(t *testing.T) {
	for _, frame := range []Frame{
		{
			ProtocolVersion: ProtocolVersion, Sequence: 1, Type: "snapshot", Provider: "codex",
			Snapshot: testSnapshot(t, wire.ProviderClaude, 1),
		},
		{
			ProtocolVersion: ProtocolVersion, Sequence: 1, Type: "heartbeat", Provider: "claude",
		},
		{
			ProtocolVersion: ProtocolVersion, Sequence: 1, Type: "status", Code: "unknown",
		},
	} {
		if err := frame.Validate(); err == nil {
			t.Fatalf("invalid frame accepted: %#v", frame)
		}
	}
}

func TestFrameRejectsNonAllowlistedProviderPayload(t *testing.T) {
	snapshot := testSnapshot(t, wire.ProviderClaude, 1)
	var fields map[string]any
	if err := json.Unmarshal(snapshot, &fields); err != nil {
		t.Fatal(err)
	}
	fields["extras"] = map[string]any{
		"extra_rate_windows": []any{map[string]any{"unexpected": "private"}},
	}
	unsafe, err := json.Marshal(fields)
	if err != nil {
		t.Fatal(err)
	}
	frame := Frame{
		ProtocolVersion: ProtocolVersion,
		Sequence:        1,
		Type:            "snapshot",
		Provider:        "claude",
		Snapshot:        unsafe,
	}
	if err := frame.Validate(); err == nil {
		t.Fatal("non-allowlisted provider payload was accepted")
	}
}

func TestFrameAcceptsBoundedHomeAgentUpdateStatus(t *testing.T) {
	var output bytes.Buffer
	encoder, err := NewEncoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	want := Frame{
		ProtocolVersion: ProtocolVersion,
		Sequence:        1,
		Type:            "status",
		Code:            "home_agent_update_required",
	}
	if err := encoder.Encode(want); err != nil {
		t.Fatal(err)
	}
	decoder, err := NewDecoder(&output)
	if err != nil {
		t.Fatal(err)
	}
	got, err := decoder.Decode()
	if err != nil || got.ProtocolVersion != want.ProtocolVersion || got.Sequence != want.Sequence ||
		got.Type != want.Type || got.Code != want.Code || got.Provider != "" || len(got.Snapshot) != 0 {
		t.Fatalf("status frame=%#v err=%v", got, err)
	}
}

func TestTransportPreservesObservedWindowFlags(t *testing.T) {
	snapshot := newTransportSnapshot(wire.UsageSnapshot{
		Rolling5hObserved: true,
		WeeklyObserved:    true,
	})
	if !snapshot.Rolling5hObserved || !snapshot.WeeklyObserved {
		t.Fatalf("observed flags were lost: %+v", snapshot)
	}
}

func TestTransportSerializesMissingWindowFlags(t *testing.T) {
	raw, err := json.Marshal(newTransportSnapshot(wire.UsageSnapshot{}))
	if err != nil {
		t.Fatal(err)
	}
	encoded := string(raw)
	if !strings.Contains(encoded, `"rolling_5h_observed":false`) || !strings.Contains(encoded, `"weekly_observed":false`) {
		t.Fatalf("missing window flags were omitted: %s", encoded)
	}
}

func TestTransportPreservesAndValidatesRetryAt(t *testing.T) {
	retryAt := "2026-08-24T00:38:27.000Z"
	transport := newTransportSnapshot(wire.UsageSnapshot{Status: wire.SnapshotStatus{RetryAt: &retryAt}})
	if transport.Status.RetryAt == nil || *transport.Status.RetryAt != retryAt {
		t.Fatalf("retry_at was lost: %+v", transport.Status)
	}

	valid := testSnapshot(t, wire.ProviderClaude, 1)
	valid = snapshotWithStatusField(t, valid, "retry_at", retryAt)
	valid = snapshotWithStatusField(t, valid, "state", string(wire.StateRateLimited))
	if err := snapshotFrame(valid).Validate(); err != nil {
		t.Fatalf("valid retry_at rejected: %v", err)
	}

	withoutRetry := testSnapshot(t, wire.ProviderClaude, 1)
	if bytes.Contains(withoutRetry, []byte(`"retry_at"`)) {
		t.Fatalf("optional retry_at serialized while absent: %s", withoutRetry)
	}
	if err := snapshotFrame(withoutRetry).Validate(); err != nil {
		t.Fatalf("backward-compatible snapshot without retry_at rejected: %v", err)
	}

	for name, value := range map[string]string{
		"malformed": "soon",
		"expired":   "2026-08-23T23:59:59.000Z",
		"equal":     "2026-08-24T00:00:00.000Z",
		"too_far":   "2026-08-25T00:00:00.001Z",
		"oversized": strings.Repeat("2", 129),
	} {
		t.Run(name, func(t *testing.T) {
			raw := snapshotWithStatusField(t, valid, "retry_at", value)
			if err := snapshotFrame(raw).Validate(); err == nil {
				t.Fatalf("invalid retry_at %q accepted", value)
			}
		})
	}

	nonRateLimited := snapshotWithStatusField(t, valid, "state", string(wire.StateNetworkError))
	if err := snapshotFrame(nonRateLimited).Validate(); err == nil {
		t.Fatal("retry_at on a non-suspended status was accepted")
	}

	staleOK := snapshotWithStatusField(t, valid, "state", string(wire.StateOK))
	staleOK = snapshotWithStatusField(t, staleOK, "stale", true)
	if err := snapshotFrame(staleOK).Validate(); err != nil {
		t.Fatalf("stale last-good retry_at rejected: %v", err)
	}
}

func TestPostDeadlineIngestSnapshotRemainsTransportValid(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	directory := t.TempDir()
	credentialPath := filepath.Join(directory, "claude.json")
	if err := os.WriteFile(credentialPath, []byte(`{"claudeAiOauth":{"accessToken":"access"}}`), 0o600); err != nil {
		t.Fatal(err)
	}
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: streamRoundTripFunc(func(*http.Request) (*http.Response, error) {
		header := make(http.Header)
		header.Set("Retry-After", "1")
		return &http.Response{
			StatusCode: http.StatusTooManyRequests,
			Header:     header,
			Body:       io.NopCloser(strings.NewReader(`{}`)),
		}, nil
	})}
	usageState := state.New(
		wire.ProviderClaude,
		auth.NewCredentialStore(newReadOnlyCredentialSource(&auth.LocalSource{ClaudePath: credentialPath})),
		client,
		nil,
		burn.New(time.UTC, fixed),
		wire.ProducerInfo{},
		slog.New(slog.NewTextHandler(io.Discard, nil)),
	)
	limited := usageState.Refresh(context.Background(), fixed)
	if limited.Snapshot.Status.RetryAt == nil {
		t.Fatalf("active suspension lacked retry_at: %+v", limited.Snapshot.Status)
	}
	eventTime := fixed.Add(2 * time.Second)
	ingested := usageState.IngestEvent(jsonl.TokenEvent{
		Provider: wire.ProviderClaude, Tokens: 1, Timestamp: eventTime,
	}, eventTime)
	if ingested.Status.State != wire.StateRateLimited || ingested.Status.RetryAt != nil {
		t.Fatalf("post-deadline event status = %+v", ingested.Status)
	}
	raw, err := json.Marshal(newTransportSnapshot(ingested))
	if err != nil {
		t.Fatal(err)
	}
	if err := snapshotFrame(raw).Validate(); err != nil {
		t.Fatalf("post-deadline burn snapshot rejected by transport: %v", err)
	}
}

func snapshotWithStatusField(t *testing.T, raw json.RawMessage, key string, value any) json.RawMessage {
	t.Helper()
	var snapshot map[string]any
	if err := json.Unmarshal(raw, &snapshot); err != nil {
		t.Fatal(err)
	}
	status, ok := snapshot["status"].(map[string]any)
	if !ok {
		t.Fatal("test snapshot status missing")
	}
	status[key] = value
	updated, err := json.Marshal(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	return updated
}

func snapshotFrame(snapshot json.RawMessage) Frame {
	return Frame{
		ProtocolVersion: ProtocolVersion,
		Sequence:        1,
		Type:            "snapshot",
		Provider:        string(wire.ProviderClaude),
		Snapshot:        snapshot,
	}
}

func TestTransportPreservesOnlyBoundedAccountAlias(t *testing.T) {
	for _, alias := range []string{"Research", "연구 계정", ""} {
		snapshot := newTransportSnapshot(sanitizeSnapshot(wire.UsageSnapshot{
			Accounts: []wire.AccountUsage{{Number: 1, Email: "private@example.test", DisplayName: alias}},
		}))
		if snapshot.Accounts[0].Email != "" || snapshot.Accounts[0].DisplayName != alias {
			t.Fatal("alias was lost or raw email entered the stream")
		}
	}
	for _, alias := range []string{strings.Repeat("x", 257), "bad\nname", "bad\u202ename", "bad\u2066name", string([]byte{0xff})} {
		if got := accountDisplayName(alias); got != "" {
			t.Fatal("unsafe or oversized account label entered the stream")
		}
	}
}

func TestSanitizeSnapshotRemovesRemoteIdentityFields(t *testing.T) {
	email := "secret@example.test"
	snapshot := wire.UsageSnapshot{
		ProducerID: "private-host",
		Extras:     wire.SnapshotExtras{AccountEmail: &email},
		Accounts:   []wire.AccountUsage{{Number: 1, Email: email}},
	}
	got := sanitizeSnapshot(snapshot)
	if got.ProducerID != "hmux-home" || got.Extras.AccountEmail != nil || got.Accounts[0].Email != "" {
		t.Fatalf("snapshot identity was not redacted: %#v", got)
	}
}

func TestRunNeedsNoBearerAndCreatesNoCredentialFiles(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("TOKEN_USAGE_DISABLE_JSONL", "1")
	t.Setenv("TOKEN_USAGE_DISABLE_CLAUDE_SWAP", "1")
	t.Setenv("TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS", "1")
	t.Setenv("TOKEN_USAGE_DISABLE_CODEX_LB", "1")
	ctx, cancel := context.WithCancel(context.Background())
	writer := &cancelAfterWrite{cancel: cancel}
	if err := Run(ctx, writer); err != nil {
		t.Fatal(err)
	}
	decoder, err := NewDecoder(bytes.NewReader(writer.data.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	frame, err := decoder.Decode()
	if err != nil || frame.Type != "snapshot" {
		t.Fatalf("first frame=%#v err=%v", frame, err)
	}
	entries, err := os.ReadDir(home)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("usage stream created Home files: %v", entries)
	}
}

func TestCollectorRuntimeCoalescesLatestSnapshotPerProvider(t *testing.T) {
	runtime := &collectorRuntime{
		latest: map[wire.Provider]wire.UsageSnapshot{},
		dirty:  map[wire.Provider]bool{},
	}
	for sequence := 1; sequence <= 100; sequence++ {
		runtime.publish(wire.UsageSnapshot{Provider: wire.ProviderCodex, Seq: sequence})
	}
	runtime.publish(wire.UsageSnapshot{Provider: wire.ProviderClaude, Seq: 7})
	got := runtime.takeSnapshots()
	if len(got) != 2 || got[0].Provider != wire.ProviderClaude || got[0].Seq != 7 ||
		got[1].Provider != wire.ProviderCodex || got[1].Seq != 100 {
		t.Fatalf("coalesced snapshots=%#v", got)
	}
	if second := runtime.takeSnapshots(); len(second) != 0 {
		t.Fatalf("clean broker replayed snapshots=%#v", second)
	}
}

type cancelAfterWrite struct {
	data   bytes.Buffer
	cancel context.CancelFunc
}

func testSnapshot(t *testing.T, provider wire.Provider, sequence int) json.RawMessage {
	t.Helper()
	snapshot := newTransportSnapshot(wire.UsageSnapshot{
		Schema:            1,
		Seq:               sequence,
		GeneratedAtUTC:    "2026-08-24T00:00:00.000Z",
		Provider:          provider,
		BurnRatePerMinute: 0,
		BurnState:         "idle",
		Rolling5h:         wire.EmptyRollingWindow(),
		Weekly:            wire.EmptyRollingWindow(),
		Status: wire.SnapshotStatus{
			State:       wire.StateNetworkError,
			DataSource:  wire.DataSourceAPIOnly,
			QuotaSource: wire.QuotaSourceNone,
			Stale:       true,
		},
	})
	raw, err := json.Marshal(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func (w *cancelAfterWrite) Write(data []byte) (int, error) {
	count, err := w.data.Write(data)
	if w.cancel != nil {
		w.cancel()
		w.cancel = nil
	}
	return count, err
}

func TestClaudeSwapEmailTransport(t *testing.T) {
	for _, provider := range []wire.Provider{wire.ProviderClaude, wire.ProviderCodex} {
		got := newTransportSnapshot(sanitizeSnapshot(wire.UsageSnapshot{
			Provider: provider, Accounts: []wire.AccountUsage{{Number: 1, Email: "owner@example.test"}},
		}))
		want := ""
		if provider == wire.ProviderClaude {
			want = "owner@example.test"
		}
		if got.Accounts[0].Email != want {
			t.Fatal("account email policy mismatch")
		}
	}
	for _, value := range []string{"bad\nname", "bad\u202ename", strings.Repeat("x", 257)} {
		if accountEmail(wire.ProviderClaude, value) != "" {
			t.Fatal("invalid email label allowed")
		}
	}
}

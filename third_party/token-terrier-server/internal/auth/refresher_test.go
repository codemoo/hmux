package auth

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestRefreshPersistFailureIsReturnedAndNotCached(t *testing.T) {
	source := &failingWriteSource{body: []byte(`{"claudeAiOauth":{"accessToken":"old","refreshToken":"refresh","expiresAt":0}}`)}
	store := NewCredentialStore(source)
	refresher := NewRefresher(store)
	refresher.HTTP = &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
		return jsonResponse(http.StatusOK, `{"access_token":"new","refresh_token":"rotated","expires_in":3600}`, nil), nil
	})}
	credential, err := store.Reload(context.Background(), wire.ProviderClaude)
	if err != nil {
		t.Fatal(err)
	}
	got, err := refresher.Refresh(context.Background(), credential)
	var refreshErr *RefreshError
	if !errors.As(err, &refreshErr) || refreshErr.Kind != RefreshKindPersistence {
		t.Fatalf("error = %v, want persistence error", err)
	}
	if got.AccessToken != "old" {
		t.Fatalf("returned access token = %q, want old", got.AccessToken)
	}
	if cached, loadErr := store.Load(context.Background(), wire.ProviderClaude); loadErr != nil || cached.AccessToken != "old" {
		t.Fatalf("cache = %+v, err=%v", cached, loadErr)
	}
}

func TestRefreshTransientIsBoundedRedactedAndCarriesRetryAfter(t *testing.T) {
	secret := "do-not-log-this-sensitive-body"
	refresher := &Refresher{HTTP: &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
		header := http.Header{"Retry-After": []string{"120"}}
		return jsonResponse(http.StatusTooManyRequests, `{"error":{"code":"rate_limit","message":"`+secret+`"}}`, header), nil
	})}}
	_, err := refresher.refreshClaude(context.Background(), OAuthCredential{
		Provider: wire.ProviderClaude, AccessToken: "old", RefreshToken: "refresh",
	})
	var refreshErr *RefreshError
	if !errors.As(err, &refreshErr) || refreshErr.Kind != RefreshKindTransient || refreshErr.Status != 429 {
		t.Fatalf("error = %#v, want transient 429", err)
	}
	if refreshErr.RetryAfter != 120*time.Second {
		t.Fatalf("retry after = %s, want 2m", refreshErr.RetryAfter)
	}
	if refreshErr.Message != "rate_limit" || strings.Contains(err.Error(), secret) {
		t.Fatalf("unredacted error: %#v / %v", refreshErr, err)
	}
}

func TestRefreshRejectsOversizedResponse(t *testing.T) {
	refresher := &Refresher{HTTP: &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
		return jsonResponse(http.StatusOK, strings.Repeat("x", maxOAuthResponseBytes+1), nil), nil
	})}}
	_, err := refresher.refreshCodex(context.Background(), OAuthCredential{
		Provider: wire.ProviderCodex, AccessToken: "old", RefreshToken: "refresh",
	})
	var refreshErr *RefreshError
	if !errors.As(err, &refreshErr) || refreshErr.Kind != RefreshKindInvalidResponse || !strings.Contains(refreshErr.Message, "exceeds") {
		t.Fatalf("error = %v, want bounded invalid response", err)
	}
}

func TestIndependentRefreshersAdoptRotatedTokenUnderFileLock(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, ".credentials.json")
	initial := []byte(`{"claudeAiOauth":{"accessToken":"old","refreshToken":"refresh","expiresAt":0}}`)
	if err := os.WriteFile(path, initial, 0o600); err != nil {
		t.Fatal(err)
	}
	var requests atomic.Int32
	client := &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
		requests.Add(1)
		time.Sleep(20 * time.Millisecond)
		return jsonResponse(http.StatusOK, `{"access_token":"new","refresh_token":"rotated","expires_in":3600}`, nil), nil
	})}

	makeRefresher := func() (*Refresher, OAuthCredential) {
		store := NewCredentialStore(&LocalSource{ClaudePath: path})
		credential, err := store.Reload(context.Background(), wire.ProviderClaude)
		if err != nil {
			t.Fatal(err)
		}
		refresher := NewRefresher(store)
		refresher.HTTP = client
		return refresher, credential
	}
	first, firstCredential := makeRefresher()
	second, secondCredential := makeRefresher()
	results := make(chan OAuthCredential, 2)
	errs := make(chan error, 2)
	var wg sync.WaitGroup
	for _, pair := range []struct {
		refresher  *Refresher
		credential OAuthCredential
	}{{first, firstCredential}, {second, secondCredential}} {
		wg.Add(1)
		go func(pair struct {
			refresher  *Refresher
			credential OAuthCredential
		}) {
			defer wg.Done()
			result, err := pair.refresher.Refresh(context.Background(), pair.credential)
			if err != nil {
				errs <- err
				return
			}
			results <- result
		}(pair)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatal(err)
	}
	close(results)
	for result := range results {
		if result.AccessToken != "new" || result.RefreshToken != "rotated" {
			t.Fatalf("result = %+v", result)
		}
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("refresh requests = %d, want 1", got)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var payload struct {
		OAuth struct {
			AccessToken  string `json:"accessToken"`
			RefreshToken string `json:"refreshToken"`
		} `json:"claudeAiOauth"`
	}
	if err := json.Unmarshal(data, &payload); err != nil {
		t.Fatal(err)
	}
	if payload.OAuth.AccessToken != "new" || payload.OAuth.RefreshToken != "rotated" {
		t.Fatalf("persisted payload = %+v", payload)
	}
}

type failingWriteSource struct {
	body []byte
}

func (s *failingWriteSource) Read(context.Context, wire.Provider) ([]byte, error) {
	return append([]byte(nil), s.body...), nil
}
func (*failingWriteSource) Write(context.Context, wire.Provider, []byte) error {
	return errors.New("disk full")
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

func jsonResponse(status int, body string, header http.Header) *http.Response {
	if header == nil {
		header = http.Header{}
	}
	return &http.Response{
		StatusCode: status,
		Header:     header,
		Body:       io.NopCloser(strings.NewReader(body)),
	}
}

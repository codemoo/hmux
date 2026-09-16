package webgateway

import (
	"context"
	"io"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type locationTransport func(*http.Request) (*http.Response, error)

func (f locationTransport) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestSessionLocationFixedEndpointAndCache(t *testing.T) {
	l := newSessionLocator()
	var calls atomic.Int32
	l.client.Transport = locationTransport(func(r *http.Request) (*http.Response, error) {
		calls.Add(1)
		if r.URL.Scheme != "https" || r.URL.Host != "ipwho.is" || r.URL.Path != "/1.1.1.1" || r.Header.Get("Cookie") != "" || r.Header.Get("Authorization") != "" {
			t.Error("unexpected lookup request")
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"success":true,"country":"Country","region":"City","city":"City"}`)), Header: make(http.Header)}, nil
	})
	for i := 0; i < 2; i++ {
		if got := l.lookup(context.Background(), "1.1.1.1"); got != "City, Country" {
			t.Fatal(got)
		}
	}
	if calls.Load() != 1 {
		t.Fatal("lookup not cached")
	}
	for _, ip := range []string{"127.0.0.1", "10.0.0.1", "::1", "fc00::1", "100.64.0.1", "169.254.1.1", "invalid/path"} {
		l.lookup(context.Background(), ip)
	}
	if calls.Load() != 1 {
		t.Fatal("private or invalid address sent externally")
	}
}

func TestSessionLocationFailureAndBoundedCache(t *testing.T) {
	for _, response := range []string{`{"success":false}`, `not-json`, strings.Repeat("x", 8193)} {
		l := newSessionLocator()
		var calls int
		l.client.Transport = locationTransport(func(*http.Request) (*http.Response, error) {
			calls++
			return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(response)), Header: make(http.Header)}, nil
		})
		for i := 0; i < 2; i++ {
			if got := l.lookup(context.Background(), "1.1.1.1"); got != "" {
				t.Fatal(got)
			}
		}
		if calls != 1 {
			t.Fatal("failure retry storm")
		}
	}
	l := newSessionLocator()
	for i := 0; i < 256; i++ {
		l.cache[string(rune(i))] = locationEntry{expires: time.Now().Add(time.Hour)}
	}
	l.client.Transport = locationTransport(func(*http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: 429, Body: io.NopCloser(strings.NewReader("")), Header: make(http.Header)}, nil
	})
	l.lookup(context.Background(), "1.1.1.1")
	if len(l.cache) > 256 {
		t.Fatal("unbounded location cache")
	}
}

func TestSessionLocationConcurrentSameIPAndCancelledRetry(t *testing.T) {
	l := newSessionLocator()
	var calls atomic.Int32
	started := make(chan struct{})
	release := make(chan struct{})
	l.client.Transport = locationTransport(func(r *http.Request) (*http.Response, error) {
		if calls.Add(1) == 1 {
			close(started)
		}
		select {
		case <-release:
		case <-r.Context().Done():
			return nil, r.Context().Err()
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"success":true,"city":"Example","country":"Country"}`)), Header: make(http.Header)}, nil
	})
	result := make(chan string, 2)
	go func() { result <- l.lookup(context.Background(), "1.1.1.1") }()
	<-started
	go func() { result <- l.lookup(context.Background(), "1.1.1.1") }()
	close(release)
	for i := 0; i < 2; i++ {
		if value := <-result; value != "Example, Country" {
			t.Fatal(value)
		}
	}
	if calls.Load() != 1 {
		t.Fatal("same-IP lookup not coalesced")
	}
	l = newSessionLocator()
	ctx, cancel := context.WithCancel(context.Background())
	started = make(chan struct{})
	l.client.Transport = locationTransport(func(r *http.Request) (*http.Response, error) {
		close(started)
		<-r.Context().Done()
		return nil, r.Context().Err()
	})
	go func() { result <- l.lookup(ctx, "1.1.1.1") }()
	<-started
	cancel()
	<-result
	l.client.Transport = locationTransport(func(*http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"success":true,"city":"Recovered"}`)), Header: make(http.Header)}, nil
	})
	if got := l.lookup(context.Background(), "1.1.1.1"); got != "Recovered" {
		t.Fatal("cancellation poisoned cache", got)
	}
	for _, ip := range []string{"192.0.2.1", "198.51.100.1", "203.0.113.1", "198.18.1.1", "2001:db8::1"} {
		if publicLocationIP(ip) {
			t.Fatal("special address treated as public", ip)
		}
	}
}

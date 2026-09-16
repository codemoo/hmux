package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"sync"
	"time"
)

type locationEntry struct {
	label   string
	expires time.Time
}

type sessionLocator struct {
	mu      sync.Mutex
	cache   map[string]locationEntry
	pending map[string]chan struct{}
	slots   chan struct{}
	client  *http.Client
}

func newSessionLocator() *sessionLocator {
	return &sessionLocator{
		cache: make(map[string]locationEntry), pending: make(map[string]chan struct{}), slots: make(chan struct{}, 2),
		client: &http.Client{Timeout: 2 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }},
	}
}

// This optional display lookup never participates in authentication. Only a
// parsed public login IP is sent to a fixed HTTPS service, without credentials.
func (l *sessionLocator) lookup(ctx context.Context, address string) string {
	ip := net.ParseIP(address)
	if ip == nil {
		return ""
	}
	if !publicLocationIP(ip.String()) {
		return "내부·예약 네트워크"
	}
	if ctx.Err() != nil {
		return ""
	}
	address = ip.String()
	now := time.Now()
	l.mu.Lock()
	if entry, ok := l.cache[address]; ok && now.Before(entry.expires) {
		l.mu.Unlock()
		return entry.label
	}
	if done := l.pending[address]; done != nil {
		l.mu.Unlock()
		select {
		case <-done:
			return l.lookup(ctx, address)
		case <-ctx.Done():
			return ""
		}
	}
	select {
	case l.slots <- struct{}{}:
	default:
		l.mu.Unlock()
		return ""
	}
	done := make(chan struct{})
	l.pending[address] = done
	l.mu.Unlock()
	label := ""
	cacheable := true
	defer func() {
		l.mu.Lock()
		if cacheable && ctx.Err() == nil {
			if len(l.cache) >= 256 {
				var oldest string
				var expiry time.Time
				for key, entry := range l.cache {
					if oldest == "" || entry.expires.Before(expiry) {
						oldest, expiry = key, entry.expires
					}
				}
				delete(l.cache, oldest)
			}
			ttl := time.Hour
			if label != "" {
				ttl = 24 * time.Hour
			}
			l.cache[address] = locationEntry{label: label, expires: time.Now().Add(ttl)}
		}
		delete(l.pending, address)
		<-l.slots
		close(done)
		l.mu.Unlock()
	}()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://ipwho.is/"+address+"?fields=success,country,region,city", nil)
	if err != nil {
		return ""
	}
	response, err := l.client.Do(request)
	if err != nil {
		cacheable = !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded)
		return ""
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return ""
	}
	raw, err := io.ReadAll(io.LimitReader(response.Body, 8193))
	if err != nil || len(raw) > 8192 {
		return ""
	}
	var result struct {
		Success bool   `json:"success"`
		Country string `json:"country"`
		Region  string `json:"region"`
		City    string `json:"city"`
	}
	if json.Unmarshal(raw, &result) != nil || !result.Success {
		return ""
	}
	parts := []string{}
	for _, part := range []string{result.City, result.Region, result.Country} {
		part = strings.TrimSpace(part)
		if part == "" || len(part) > 160 || strings.ContainsAny(part, "\r\n\x00") {
			continue
		}
		if len(parts) == 0 || parts[len(parts)-1] != part {
			parts = append(parts, part)
		}
	}
	label = strings.Join(parts, ", ")
	return label
}

// Exclude special-use ranges as well as RFC1918/ULA. No local/test address is
// sent to the geolocation provider.
func publicLocationIP(value string) bool {
	address, err := netip.ParseAddr(value)
	if err != nil {
		return false
	}
	address = address.Unmap()
	if !address.IsGlobalUnicast() || address.IsPrivate() {
		return false
	}
	for _, prefix := range []string{
		"0.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
		"192.0.0.0/24", "192.0.2.0/24", "192.88.99.0/24", "198.18.0.0/15",
		"198.51.100.0/24", "203.0.113.0/24", "240.0.0.0/4",
		"::/96", "64:ff9b::/96", "64:ff9b:1::/48", "100::/64", "2001::/23",
		"2001:db8::/32", "2002::/16", "3fff::/20", "5f00::/16",
	} {
		if netip.MustParsePrefix(prefix).Contains(address) {
			return false
		}
	}
	return true
}

func (l *sessionLocator) enrich(ctx context.Context, sessions []sessionInfo) {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	jobs := make(chan int, len(sessions))
	for i := range sessions {
		jobs <- i
	}
	close(jobs)
	var workers sync.WaitGroup
	for n := 0; n < 2; n++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for i := range jobs {
				sessions[i].Location = l.lookup(ctx, sessions[i].IP)
			}
		}()
	}
	workers.Wait()
}

package webgateway

import (
	"context"
	"crypto/ecdh"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	webpush "github.com/SherClockHolmes/webpush-go"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

const maxPushSubscriptions = 256

type pushState struct {
	Version       int                             `json:"version"`
	PublicKey     string                          `json:"public_key"`
	PrivateKey    string                          `json:"private_key"`
	Subscriptions map[string]webpush.Subscription `json:"subscriptions"`
}
type pushPresence struct {
	Session model.SessionIdentity
	Until   time.Time
}
type completionEvent struct {
	Session     model.SessionIdentity `json:"session"`
	EventID     string                `json:"event_id"`
	CompletedAt time.Time             `json:"completed_at"`
}
type pushStore struct {
	lock      *os.File
	closeOnce sync.Once
	mu        sync.Mutex
	path      string
	state     pushState
	presence  map[string]map[string]pushPresence
	lastTest  map[string]time.Time
	seen      map[string]time.Time
	queue     chan completionEvent
	ctx       context.Context
	cancel    context.CancelFunc
	workers   sync.WaitGroup
	client    webpush.HTTPClient
}

func newPushStore(path string) (*pushStore, error) {
	p := &pushStore{path: path, presence: map[string]map[string]pushPresence{}, lastTest: map[string]time.Time{}, seen: map[string]time.Time{}, queue: make(chan completionEvent, 64), client: pushHTTPClient()}
	p.ctx, p.cancel = context.WithCancel(context.Background())
	lock, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0600)
	if err != nil {
		p.cancel()
		return nil, errors.New("private push lock unavailable")
	}
	info, err := lock.Stat()
	if err != nil {
		lock.Close()
		p.cancel()
		return nil, errors.New("private push lock unavailable")
	}
	stat, owner := info.Sys().(*syscall.Stat_t)
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || !owner || int(stat.Uid) != os.Getuid() {
		lock.Close()
		p.cancel()
		return nil, errors.New("private push lock unavailable")
	}
	if err = filelock.Acquire(p.ctx, lock, 100*time.Millisecond); err != nil {
		lock.Close()
		p.cancel()
		return nil, errors.New("push storage already in use")
	}
	p.lock = lock
	raw, err := readPrivate(path, 2<<20)
	if errors.Is(err, os.ErrNotExist) {
		private, public, e := webpush.GenerateVAPIDKeys()
		if e != nil {
			p.close()
			return nil, e
		}
		p.state = pushState{Version: 1, PublicKey: public, PrivateKey: private, Subscriptions: map[string]webpush.Subscription{}}
		err = p.saveLocked(p.state)
	} else if err == nil {
		err = json.Unmarshal(raw, &p.state)
		if err == nil && (p.state.Version != 1 || len(p.state.Subscriptions) > maxPushSubscriptions) {
			err = errors.New("invalid push state")
		}
		secret, e := base64.RawURLEncoding.DecodeString(p.state.PrivateKey)
		key, e2 := ecdh.P256().NewPrivateKey(secret)
		if e != nil || e2 != nil {
			err = errors.New("invalid push keys")
		} else if base64.RawURLEncoding.EncodeToString(key.PublicKey().Bytes()) != p.state.PublicKey {
			err = errors.New("invalid push keys")
		}
		for id, sub := range p.state.Subscriptions {
			if !validPushID(id) || validatePushSubscription(sub) != nil {
				err = errors.New("invalid push subscription state")
				break
			}
		}
		if p.state.Subscriptions == nil {
			p.state.Subscriptions = map[string]webpush.Subscription{}
		}
	}
	if err != nil {
		p.close()
		return nil, errors.New("private push storage unavailable")
	}
	return p, nil
}
func (p *pushStore) saveLocked(next pushState) error {
	if err := p.ctx.Err(); err != nil {
		return err
	}
	raw, err := json.Marshal(next)
	if err != nil {
		return err
	}
	return config.AtomicWrite(p.path, raw, 0600)
}
func (p *pushStore) close() {
	p.closeOnce.Do(func() {
		p.cancel()
		p.workers.Wait()
		p.mu.Lock()
		defer p.mu.Unlock()
		if client, ok := p.client.(*http.Client); ok {
			client.CloseIdleConnections()
		}
		if p.lock != nil {
			filelock.Unlock(p.lock)
			p.lock.Close()
		}
	})
}
func validPushID(id string) bool {
	raw, e := base64.RawURLEncoding.DecodeString(id)
	return e == nil && len(raw) == 32
}
func validatePushSubscription(sub webpush.Subscription) error {
	u, err := url.Parse(sub.Endpoint)
	if err != nil || len(sub.Endpoint) > 2048 || u.Scheme != "https" || u.User != nil || u.Fragment != "" || u.Port() != "" || u.Opaque != "" {
		return errors.New("invalid push endpoint")
	}
	host := strings.ToLower(u.Hostname())
	allowed := host == "fcm.googleapis.com" || host == "web.push.apple.com" || host == "updates.push.services.mozilla.com" || strings.HasSuffix(host, ".push.services.mozilla.com") || strings.HasSuffix(host, ".notify.windows.com")
	if !allowed {
		return errors.New("unsupported push service")
	}
	auth, e := base64.RawURLEncoding.DecodeString(sub.Keys.Auth)
	pub, e2 := base64.RawURLEncoding.DecodeString(sub.Keys.P256dh)
	_, e3 := ecdh.P256().NewPublicKey(pub)
	if e != nil || len(auth) != 16 || e2 != nil || e3 != nil {
		return errors.New("invalid push keys")
	}
	return nil
}

// Only known push services are accepted, redirects/proxies are disabled, and
// resolved addresses are pinned for dialing to prevent private-network requests.
func pushHTTPClient() *http.Client {
	tr := &http.Transport{TLSHandshakeTimeout: 5 * time.Second, ResponseHeaderTimeout: 8 * time.Second, MaxIdleConns: 4, MaxIdleConnsPerHost: 2, IdleConnTimeout: 30 * time.Second}
	tr.DialContext = func(ctx context.Context, network, address string) (net.Conn, error) {
		host, port, err := net.SplitHostPort(address)
		if err != nil {
			return nil, err
		}
		ips, err := net.DefaultResolver.LookupIPAddr(ctx, host)
		if err != nil {
			return nil, err
		}
		for _, addr := range ips {
			if !publicPushAddress(addr.IP) {
				return nil, errors.New("non-public push service address")
			}
		}
		for _, addr := range ips {
			conn, e := (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, net.JoinHostPort(addr.IP.String(), port))
			if e == nil {
				return conn, nil
			}
			err = e
		}
		if err == nil {
			err = errors.New("push service address unavailable")
		}
		return nil, err
	}
	return &http.Client{Transport: tr, Timeout: 10 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}

// Special-purpose ranges are not outbound push destinations, even where Go's
// IsGlobalUnicast classifies them as unicast. IPv6 is limited to global space.
var pushDeniedNetworks = func() []netip.Prefix {
	var prefixes []netip.Prefix
	for _, cidr := range []string{"0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16", "172.16.0.0/12", "192.0.0.0/24", "192.0.2.0/24", "192.88.99.0/24", "192.168.0.0/16", "198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "224.0.0.0/4", "240.0.0.0/4", "2001::/23", "2001:db8::/32", "2002::/16", "3fff::/20"} {
		prefixes = append(prefixes, netip.MustParsePrefix(cidr))
	}
	return prefixes
}()

func publicPushAddress(ip net.IP) bool {
	addr, ok := netip.AddrFromSlice(ip)
	if !ok {
		return false
	}
	addr = addr.Unmap()
	if !addr.IsGlobalUnicast() || addr.IsPrivate() || (addr.Is6() && !netip.MustParsePrefix("2000::/3").Contains(addr)) {
		return false
	}
	for _, prefix := range pushDeniedNetworks {
		if prefix.Contains(addr) {
			return false
		}
	}
	return true
}

// Public login IDs are only lookup keys. The authenticated cookie remains the
// authority for registration, presence and revocation; no push API takes a user.
func (a *authStore) pushLogin(token string) (loginSession, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.pushLoginLocked(a.sessions[sessionKey(token)])
}
func (a *authStore) pushLoginByID(id string) (loginSession, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	for _, login := range a.sessions {
		if login.id == id {
			return a.pushLoginLocked(login)
		}
	}
	return loginSession{}, false
}
func (a *authStore) pushLoginLocked(login *loginSession) (loginSession, bool) {
	if a.storageErr != nil || login == nil || !time.Now().Before(login.expires) {
		return loginSession{}, false
	}
	select {
	case <-login.done:
		return loginSession{}, false
	default:
	}
	return *login, true
}
func (s *Server) pushAPI(w http.ResponseWriter, r *http.Request, token string) {
	login, ok := s.auth.pushLogin(token)
	if !ok {
		http.Error(w, "Session expired", 401)
		return
	}
	p := s.push
	if p == nil {
		http.Error(w, "Notifications unavailable", 503)
		return
	}
	if r.URL.Path == "/api/push" && r.Method == http.MethodGet {
		p.mu.Lock()
		sub, enabled := p.state.Subscriptions[login.id]
		public := p.state.PublicKey
		p.mu.Unlock()
		writeJSON(w, map[string]any{"public_key": public, "login_id": login.id, "enabled": enabled, "endpoint": sub.Endpoint})
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", 405)
		return
	}
	switch r.URL.Path {
	case "/api/push/subscribe":
		var req struct {
			Endpoint       string       `json:"endpoint"`
			Keys           webpush.Keys `json:"keys"`
			ExpirationTime *float64     `json:"expirationTime"`
		}
		if decodeRequest(w, r, &req) != nil {
			http.Error(w, "Invalid subscription", 400)
			return
		}
		sub := webpush.Subscription{Endpoint: req.Endpoint, Keys: req.Keys}
		if validatePushSubscription(sub) != nil {
			http.Error(w, "지원하지 않는 알림 구독입니다.", 400)
			return
		}
		p.mu.Lock()
		// Revalidate after potentially slow body decoding. Mutations serialize with
		// delivery snapshots and never implicitly opt a new login into an old device.
		_, valid := s.auth.pushLogin(token)
		err := error(nil)
		if valid {
			next := p.state
			next.Subscriptions = make(map[string]webpush.Subscription)
			for id, old := range p.state.Subscriptions {
				if _, live := s.auth.pushLoginByID(id); live && old.Endpoint != sub.Endpoint {
					next.Subscriptions[id] = old
				}
			}
			next.Subscriptions[login.id] = sub
			if len(next.Subscriptions) > maxPushSubscriptions {
				err = errors.New("subscription limit")
			} else {
				err = p.saveLocked(next)
				if err == nil {
					p.state = next
					for id := range p.lastTest {
						if _, ok := next.Subscriptions[id]; !ok {
							delete(p.lastTest, id)
						}
					}
					for id := range p.presence {
						if _, ok := next.Subscriptions[id]; !ok {
							delete(p.presence, id)
						}
					}
				}
			}
		}
		p.mu.Unlock()
		if !valid {
			http.Error(w, "Session expired", 401)
			return
		}
		if err != nil {
			http.Error(w, "알림 설정을 저장하지 못했습니다.", 503)
			return
		}
	case "/api/push/unsubscribe":
		var req struct{}
		if decodeRequest(w, r, &req) != nil {
			http.Error(w, "Invalid request", 400)
			return
		}
		if err := p.remove(login.id, ""); err != nil {
			http.Error(w, "알림 설정을 저장하지 못했습니다.", 503)
			return
		}
	case "/api/push/presence":
		var req struct {
			ClientID string                 `json:"client_id"`
			Session  *model.SessionIdentity `json:"session"`
		}
		if decodeRequest(w, r, &req) != nil || len(req.ClientID) < 1 || len(req.ClientID) > 64 || !validSessionText(req.ClientID, 64) || (req.Session != nil && (model.ValidateSessionID(req.Session.ID) != nil || req.Session.CreatedAt < 1)) {
			http.Error(w, "Invalid presence", 400)
			return
		}
		p.mu.Lock()
		now := time.Now()
		for id, clients := range p.presence {
			for client, v := range clients {
				if !now.Before(v.Until) {
					delete(clients, client)
				}
			}
			if len(clients) == 0 {
				delete(p.presence, id)
			}
		}
		if req.Session == nil {
			delete(p.presence[login.id], req.ClientID)
		} else if len(p.presence) < maxPushSubscriptions || p.presence[login.id] != nil {
			clients := p.presence[login.id]
			if clients == nil {
				clients = map[string]pushPresence{}
				p.presence[login.id] = clients
			}
			if len(clients) < 16 || !clients[req.ClientID].Until.IsZero() {
				clients[req.ClientID] = pushPresence{*req.Session, now.Add(45 * time.Second)}
			}
		}
		p.mu.Unlock()
	case "/api/push/test":
		var req struct{}
		if decodeRequest(w, r, &req) != nil {
			http.Error(w, "Invalid request", 400)
			return
		}
		p.mu.Lock()
		sub, enabled := p.state.Subscriptions[login.id]
		last := p.lastTest[login.id]
		if enabled && time.Since(last) >= 30*time.Second {
			p.lastTest[login.id] = time.Now()
		}
		p.mu.Unlock()
		if !enabled {
			http.Error(w, "먼저 이 기기의 알림을 켜주세요.", 409)
			return
		}
		if time.Since(last) < 30*time.Second {
			http.Error(w, "잠시 후 다시 시도하세요.", 429)
			return
		}
		if !s.sendPush(r.Context(), login, sub, map[string]any{"type": "test", "login_id": login.id, "event_id": RandomToken()}) {
			http.Error(w, "테스트 알림을 보내지 못했습니다. 알림을 다시 켜주세요.", 502)
			return
		}
	default:
		http.NotFound(w, r)
		return
	}
	writeJSON(w, map[string]bool{"ok": true})
}
func (p *pushStore) remove(login, endpoint string) error {
	p.mu.Lock()
	defer p.mu.Unlock()
	old, ok := p.state.Subscriptions[login]
	if !ok || (endpoint != "" && old.Endpoint != endpoint) {
		return nil
	}
	next := p.state
	next.Subscriptions = make(map[string]webpush.Subscription, len(p.state.Subscriptions))
	for id, sub := range p.state.Subscriptions {
		if id != login {
			next.Subscriptions[id] = sub
		}
	}
	if err := p.saveLocked(next); err != nil {
		return err
	}
	p.state = next
	delete(p.presence, login)
	delete(p.lastTest, login)
	return nil
}
func (p *pushStore) enqueue(e completionEvent) {
	now := time.Now()
	if model.ValidateSessionID(e.Session.ID) != nil || e.Session.CreatedAt < 1 || len(e.EventID) != 64 || e.CompletedAt.After(now.Add(time.Minute)) || now.Sub(e.CompletedAt) > 2*time.Minute {
		return
	}
	if _, err := hex.DecodeString(e.EventID); err != nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	for id, at := range p.seen {
		if now.Sub(at) > 5*time.Minute {
			delete(p.seen, id)
		}
	}
	if _, seen := p.seen[e.EventID]; seen || len(p.seen) >= 4096 {
		return
	}
	select {
	case p.queue <- e:
		p.seen[e.EventID] = now
	default:
	}
}
func (s *Server) runPush() {
	p := s.push
	p.workers.Add(1)
	go func() {
		defer p.workers.Done()
		for {
			select {
			case <-p.ctx.Done():
				return
			case e := <-p.queue:
				s.deliverCompletion(e)
			}
		}
	}()
}
func (s *Server) pushWorkspace(ctx context.Context, profile string) (sharedworkspace.Snapshot, error) {
	if profile == "" {
		raw, err := s.hub.request(ctx, Message{Type: "request", Operation: "workspace", Payload: json.RawMessage(`{}`)})
		var out sharedworkspace.Snapshot
		if err == nil {
			err = json.Unmarshal(raw, &out)
		}
		return out, err
	}
	return (sharedworkspace.Store{StateDir: filepath.Join(s.workspaceDir, profile)}).Sync(ctx, nil, func(context.Context) (model.Catalog, error) {
		snap := s.hub.snapshot()
		raw, _ := snap["catalog"].(json.RawMessage)
		var c model.Catalog
		if snap["online"] != true {
			return c, errors.New("Home offline")
		}
		err := json.Unmarshal(raw, &c)
		return c, err
	})
}
func (s *Server) deliverCompletion(e completionEvent) {
	p := s.push
	var catalog model.Catalog
	snapshot := s.hub.snapshot()
	raw, _ := snapshot["catalog"].(json.RawMessage)
	if snapshot["online"] != true || json.Unmarshal(raw, &catalog) != nil {
		return
	}
	tabName := ""
	for _, session := range catalog.Sessions {
		if session.ID == e.Session.ID && session.CreatedAt == e.Session.CreatedAt {
			tabName = session.Alias
			if tabName == "" {
				tabName = session.Name
			}
			break
		}
	}
	if tabName == "" {
		return
	}
	tabName = model.SafeText(tabName, 120)
	p.mu.Lock()
	subs := make(map[string]webpush.Subscription, len(p.state.Subscriptions))
	for id, sub := range p.state.Subscriptions {
		subs[id] = sub
	}
	p.mu.Unlock()
	ctx, cancel := context.WithTimeout(p.ctx, 30*time.Second)
	defer cancel()
	workspaces := map[string][]model.SessionIdentity{}
	for id, sub := range subs {
		login, ok := s.auth.pushLoginByID(id)
		if !ok {
			_ = p.remove(id, sub.Endpoint)
			continue
		}
		list, loaded := workspaces[login.profile]
		if !loaded {
			workspace, err := s.pushWorkspace(ctx, login.profile)
			if err == nil {
				list = workspace.Tabs
			}
			workspaces[login.profile] = list
		}
		matching := false
		for _, tab := range list {
			if tab == e.Session {
				matching = true
				break
			}
		}
		if !matching {
			continue
		}
		p.mu.Lock()
		watching := false
		for _, v := range p.presence[id] {
			if time.Now().Before(v.Until) && v.Session == e.Session {
				watching = true
				break
			}
		}
		p.mu.Unlock()
		if watching {
			continue
		}
		s.sendPush(ctx, login, sub, map[string]any{"type": "codex-complete", "tab_name": tabName, "session": e.Session, "login_id": id, "event_id": e.EventID})
	}
}
func (s *Server) sendPush(parent context.Context, login loginSession, sub webpush.Subscription, payload any) bool {
	p := s.push
	if _, ok := s.auth.pushLoginByID(login.id); !ok {
		return false
	}
	p.mu.Lock()
	current, ok := p.state.Subscriptions[login.id]
	public, private := p.state.PublicKey, p.state.PrivateKey
	p.mu.Unlock()
	if !ok || current != sub || validatePushSubscription(sub) != nil {
		return false
	}
	deadline := time.Now().Add(10 * time.Second)
	if login.expires.Before(deadline) {
		deadline = login.expires
	}
	ctx, cancel := context.WithDeadline(parent, deadline)
	defer cancel()
	go func() {
		select {
		case <-login.done:
			cancel()
		case <-p.ctx.Done():
			cancel()
		case <-ctx.Done():
		}
	}()
	select {
	case <-login.done:
		return false
	case <-p.ctx.Done():
		return false
	case <-ctx.Done():
		return false
	default:
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		return false
	}
	hash := sha256.Sum256([]byte(login.id + string(raw)))
	response, err := webpush.SendNotificationWithContext(ctx, raw, &sub, &webpush.Options{HTTPClient: p.client, Subscriber: s.origin, TTL: 120, Urgency: webpush.UrgencyNormal, Topic: base64.RawURLEncoding.EncodeToString(hash[:24]), VAPIDPublicKey: public, VAPIDPrivateKey: private})
	if err != nil {
		return false
	}
	defer response.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 4096))
	if response.StatusCode == 404 || response.StatusCode == 410 {
		_ = p.remove(login.id, sub.Endpoint)
	}
	return response.StatusCode >= 200 && response.StatusCode < 300
}

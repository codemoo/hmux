package webgateway

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdh"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	webpush "github.com/SherClockHolmes/webpush-go"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
	"golang.org/x/crypto/hkdf"
)

type pushClientFunc func(*http.Request) (*http.Response, error)

func (f pushClientFunc) Do(r *http.Request) (*http.Response, error) { return f(r) }
func pushSubscription(t *testing.T, suffix string) (webpush.Subscription, *ecdh.PrivateKey) {
	t.Helper()
	key, err := ecdh.P256().GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return webpush.Subscription{Endpoint: "https://web.push.apple.com/" + suffix, Keys: webpush.Keys{Auth: base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{7}, 16)), P256dh: base64.RawURLEncoding.EncodeToString(key.PublicKey().Bytes())}}, key
}
func pushSubscribe(t *testing.T, s *Server, token string, sub webpush.Subscription) {
	t.Helper()
	csrf, _, _ := s.auth.get(token, false)
	if res := request(s, "POST", "/api/push/subscribe", sub, token, csrf, s.origin); res.Code != 200 {
		t.Fatal(res.Code, res.Body.String())
	}
}
func decryptPush(t *testing.T, r *http.Request, key *ecdh.PrivateKey, sub webpush.Subscription) map[string]any {
	t.Helper()
	if r.Header.Get("Content-Encoding") != "aes128gcm" || !strings.HasPrefix(r.Header.Get("Authorization"), "vapid ") {
		t.Fatal("push not encrypted/authenticated")
	}
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		t.Fatal(err)
	}
	if len(raw) < 86 || binary.BigEndian.Uint32(raw[16:20]) != 4096 {
		t.Fatal("invalid record")
	}
	keyLen := int(raw[20])
	sender, err := ecdh.P256().NewPublicKey(raw[21 : 21+keyLen])
	if err != nil {
		t.Fatal(err)
	}
	secret, err := key.ECDH(sender)
	if err != nil {
		t.Fatal(err)
	}
	auth, _ := base64.RawURLEncoding.DecodeString(sub.Keys.Auth)
	derive := func(secret, salt, info []byte, n int) []byte {
		out := make([]byte, n)
		if _, err := io.ReadFull(hkdf.New(sha256.New, secret, salt, info), out); err != nil {
			t.Fatal(err)
		}
		return out
	}
	info := append([]byte("WebPush: info\x00"), key.PublicKey().Bytes()...)
	info = append(info, sender.Bytes()...)
	ikm := derive(secret, auth, info, 32)
	cek := derive(ikm, raw[:16], []byte("Content-Encoding: aes128gcm\x00"), 16)
	nonce := derive(ikm, raw[:16], []byte("Content-Encoding: nonce\x00"), 12)
	block, _ := aes.NewCipher(cek)
	gcm, _ := cipher.NewGCM(block)
	plain, err := gcm.Open(nil, nonce, raw[21+keyLen:], nil)
	if err != nil {
		t.Fatal(err)
	}
	plain = bytes.TrimRight(plain, "\x00")
	if plain[len(plain)-1] != 2 {
		t.Fatal("invalid delimiter")
	}
	plain = plain[:len(plain)-1]
	var value map[string]any
	if err = json.Unmarshal(plain, &value); err != nil {
		t.Fatal(err)
	}
	return value
}
func TestPushRegistrationSecurityPersistenceAndDeviceTransfer(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	sub, _ := pushSubscription(t, "fixture")
	if w := request(s, "GET", "/api/push", nil, "", "", ""); w.Code != 401 {
		t.Fatal(w.Code)
	}
	if w := request(s, "POST", "/api/push/subscribe", sub, token, "", s.origin); w.Code != 403 {
		t.Fatal(w.Code)
	}
	if w := request(s, "POST", "/api/push/subscribe", sub, token, csrf, "https://evil.example"); w.Code != 403 {
		t.Fatal(w.Code)
	}
	bad := sub
	bad.Endpoint = "https://127.0.0.1/private"
	if w := request(s, "POST", "/api/push/subscribe", bad, token, csrf, s.origin); w.Code != 400 {
		t.Fatal(w.Code)
	}
	pushSubscribe(t, s, token, sub)
	login, _ := s.auth.pushLogin(token)
	if concurrent, e := newPushStore(s.push.path); e == nil {
		concurrent.close()
		t.Fatal("concurrent push store accepted")
	}
	path := s.push.path
	public := s.push.state.PublicKey
	s.push.close()
	loaded, err := newPushStore(path)
	if err != nil {
		t.Fatal(err)
	}
	s.push = loaded
	s.hub.onCompletion = loaded.enqueue
	s.runPush()
	if loaded.state.PublicKey != public || loaded.state.Subscriptions[login.id] != sub {
		t.Fatal("push state lost at restart")
	}
	info, _ := os.Stat(s.push.path)
	if info.Mode().Perm() != 0600 {
		t.Fatal("state permissions")
	}
	raw, _ := os.ReadFile(s.push.path)
	if bytes.Contains(raw, []byte(token)) {
		t.Fatal("stored authentication token")
	}
	// A new authenticated login explicitly takes ownership of this browser endpoint.
	at := time.Now().Add(31 * time.Second)
	next, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, at), at)
	if !ok {
		t.Fatal("second login")
	}
	s.push.lastTest[login.id] = time.Now()
	pushSubscribe(t, s, next, sub)
	if len(s.push.lastTest) != 0 {
		t.Fatal("test rate history retained dropped login")
	}
	if len(s.push.state.Subscriptions) != 1 {
		t.Fatal("endpoint has multiple owners")
	}
	if _, exists := s.push.state.Subscriptions[login.id]; exists {
		t.Fatal("old login retained endpoint")
	}
	if w := request(s, "POST", "/api/push/unsubscribe", struct{}{}, token, csrf, s.origin); w.Code != 200 {
		t.Fatal(w.Code)
	}
	if len(s.push.state.Subscriptions) != 1 {
		t.Fatal("old login disabled new subscription")
	}
}
func TestPushEndpointValidation(t *testing.T) {
	sub, _ := pushSubscription(t, "valid")
	for _, endpoint := range []string{"http://web.push.apple.com/x", "https://web.push.apple.com.evil.example/x", "https://web.push.apple.com:443/x", "https://user@web.push.apple.com/x", "https://web.push.apple.com/x#f", "https://169.254.169.254/x", "https://evil.example/x"} {
		sub.Endpoint = endpoint
		if validatePushSubscription(sub) == nil {
			t.Fatal("accepted", endpoint)
		}
	}
	for _, endpoint := range []string{"https://web.push.apple.com/x", "https://fcm.googleapis.com/fcm/send/x", "https://updates.push.services.mozilla.com/wpush/v2/x", "https://wns2-example.notify.windows.com/x"} {
		sub.Endpoint = endpoint
		if validatePushSubscription(sub) != nil {
			t.Fatal("rejected", endpoint)
		}
	}
	sub.Keys.P256dh = "bad"
	if validatePushSubscription(sub) == nil {
		t.Fatal("invalid public key accepted")
	}
}
func TestPushDeliveryExactWorkspacePresenceRevocationAndExpiry(t *testing.T) {
	s := testServer(t)
	first := addTestAccount(t, s, "notify-a")
	second := addTestAccount(t, s, "notify-b")
	auth, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	s.auth = auth
	a, ok := auth.login(first.Username, testPassword, codeAt(first, time.Now()), time.Now())
	if !ok {
		t.Fatal("login a")
	}
	b, ok := auth.login(second.Username, testPassword, codeAt(second, time.Now()), time.Now())
	if !ok {
		t.Fatal("login b")
	}
	subA, keyA := pushSubscription(t, "a")
	subB, _ := pushSubscription(t, "b")
	pushSubscribe(t, s, a, subA)
	pushSubscribe(t, s, b, subB)
	identity := model.SessionIdentity{ID: "$1", CreatedAt: 42}
	c := model.Catalog{Sessions: []model.Session{{ID: "$1", CreatedAt: 42, Name: "private-tab-name", Alias: "배포 작업"}}}
	raw, _ := json.Marshal(c)
	s.hub.mu.Lock()
	s.hub.home = &peer{}
	s.hub.catalog = raw
	s.hub.updated = time.Now()
	s.hub.mu.Unlock()
	defer func() { s.hub.mu.Lock(); s.hub.home = nil; s.hub.mu.Unlock() }()
	loginA, _ := auth.pushLogin(a)
	loginB, _ := auth.pushLogin(b)
	for _, entry := range []struct {
		login loginSession
		ids   []model.SessionIdentity
	}{{loginA, []model.SessionIdentity{identity}}, {loginB, nil}} {
		store := sharedworkspace.Store{StateDir: filepath.Join(s.workspaceDir, entry.login.profile)}
		_, err := store.Sync(context.Background(), &sharedworkspace.Change{OperationID: "push-fixture-0001", Tabs: entry.ids}, func(context.Context) (model.Catalog, error) { return c, nil })
		if err != nil {
			t.Fatal(err)
		}
	}
	count := 0
	s.push.client = pushClientFunc(func(r *http.Request) (*http.Response, error) {
		count++
		if r.URL.String() != subA.Endpoint {
			t.Fatal("sent to wrong account")
		}
		data := decryptPush(t, r, keyA, subA)
		if data["tab_name"] != "배포 작업" || data["login_id"] != loginA.id || data["type"] != "codex-complete" {
			t.Fatal("wrong payload", data)
		}
		if len(data) != 5 {
			t.Fatal("unexpected private payload fields", data)
		}
		return &http.Response{StatusCode: 201, Body: io.NopCloser(strings.NewReader(""))}, nil
	})
	event := completionEvent{Session: identity, EventID: strings.Repeat("a", 64), CompletedAt: time.Now()}
	s.deliverCompletion(event)
	if count != 1 {
		t.Fatal("missing completion")
	}
	// Only this browser's live presence suppresses; expiry resumes delivery.
	s.push.presence[loginA.id] = map[string]pushPresence{"client": {Session: identity, Until: time.Now().Add(time.Minute)}}
	s.deliverCompletion(event)
	if count != 1 {
		t.Fatal("not suppressed")
	}
	s.push.presence[loginA.id]["client"] = pushPresence{Session: identity, Until: time.Now().Add(-time.Second)}
	s.deliverCompletion(event)
	if count != 2 {
		t.Fatal("stale presence suppressed")
	}
	event.Session.CreatedAt = 43
	s.deliverCompletion(event)
	if count != 2 {
		t.Fatal("reused tmux identity delivered")
	}
	event.Session = identity
	if err := auth.logout(a); err != nil {
		t.Fatal(err)
	}
	s.deliverCompletion(event)
	if count != 2 {
		t.Fatal("revoked login notified")
	}
	if _, ok := s.push.state.Subscriptions[loginA.id]; ok {
		t.Fatal("revoked subscription retained")
	}
}
func TestPushGoneDisablesSubscriptionAndDedupeBounds(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	sub, _ := pushSubscription(t, "gone")
	pushSubscribe(t, s, token, sub)
	login, _ := s.auth.pushLogin(token)
	s.push.client = pushClientFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: 410, Body: io.NopCloser(strings.NewReader(""))}, nil
	})
	if s.sendPush(context.Background(), login, sub, map[string]string{"type": "test"}) {
		t.Fatal("gone reported success")
	}
	if _, ok := s.push.state.Subscriptions[login.id]; ok {
		t.Fatal("gone retained")
	}
	// Isolated queue prevents the worker consuming test events.
	p := &pushStore{queue: make(chan completionEvent, 1), seen: map[string]time.Time{}}
	e := completionEvent{Session: model.SessionIdentity{ID: "$1", CreatedAt: 42}, EventID: strings.Repeat("b", 64), CompletedAt: time.Now()}
	p.enqueue(e)
	p.enqueue(e)
	if len(p.queue) != 1 {
		t.Fatal("duplicate")
	}
	<-p.queue
	p.enqueue(e)
	if len(p.queue) != 0 {
		t.Fatal("dedupe lost after dequeue")
	}
	e.EventID = strings.Repeat("c", 64)
	e.CompletedAt = time.Now().Add(-3 * time.Minute)
	p.enqueue(e)
	if len(p.queue) != 0 {
		t.Fatal("historical completion accepted")
	}
	e.CompletedAt = time.Time{}
	p.enqueue(e)
	if len(p.queue) != 0 {
		t.Fatal("missing timestamp accepted")
	}
}

func TestPushSendDeadlineTracksLoginExpiry(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	sub, _ := pushSubscription(t, "expiry")
	pushSubscribe(t, s, token, sub)
	expiry := time.Now().Add(120 * time.Millisecond)
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].expires = expiry
	s.auth.mu.Unlock()
	login, _ := s.auth.pushLogin(token)
	called := false
	s.push.client = pushClientFunc(func(r *http.Request) (*http.Response, error) {
		called = true
		deadline, ok := r.Context().Deadline()
		if !ok || deadline.After(expiry) {
			t.Fatal("send outlives login")
		}
		<-r.Context().Done()
		return nil, r.Context().Err()
	})
	if s.sendPush(context.Background(), login, sub, map[string]string{"type": "test"}) || !called {
		t.Fatal("expiry cancellation failed")
	}
	// Expired or already revoked logins must not invoke an outbound client at all.
	called = false
	s.sendPush(context.Background(), login, sub, map[string]string{"type": "test"})
	if called {
		t.Fatal("expired login sent")
	}
}
func TestPushAddressesMustBePubliclyRoutable(t *testing.T) {
	for _, value := range []string{"127.0.0.1", "10.0.0.1", "100.64.0.1", "198.18.0.1", "192.0.2.1", "203.0.113.1", "169.254.169.254", "0.1.2.3", "240.0.0.1", "::1", "::ffff:100.64.0.1", "64:ff9b::a00:1", "fc00::1", "2001:db8::1", "2002::1", "3fff::1"} {
		if publicPushAddress(net.ParseIP(value)) {
			t.Fatal("non-public accepted", value)
		}
	}
	for _, value := range []string{"1.1.1.1", "8.8.8.8", "2606:4700:4700::1111", "2001:4860:4860::8888"} {
		if !publicPushAddress(net.ParseIP(value)) {
			t.Fatal("public rejected", value)
		}
	}
}

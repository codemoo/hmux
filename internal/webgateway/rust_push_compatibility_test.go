package webgateway

import (
	"bytes"
	"crypto/ecdh"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	webpush "github.com/SherClockHolmes/webpush-go"
)

type rustPushCase struct {
	Name      string `json:"name"`
	Input     string `json:"input"`
	GoValid   bool   `json:"go_valid"`
	RustValid bool   `json:"rust_valid"`
	Delta     string `json:"delta,omitempty"`
}

// All keys are deterministic synthetic P-256 fixtures. No push service is contacted.
func TestRustPushStateOracle(t *testing.T) {
	encode := base64.RawURLEncoding.EncodeToString
	private := bytes.Repeat([]byte{7}, 32)
	key, err := ecdh.P256().NewPrivateKey(private)
	if err != nil {
		t.Fatal(err)
	}
	public := key.PublicKey().Bytes()
	login := encode(bytes.Repeat([]byte{1}, 32))
	sub := webpush.Subscription{Endpoint: "https://web.push.apple.com/synthetic", Keys: webpush.Keys{Auth: encode(bytes.Repeat([]byte{7}, 16)), P256dh: encode(public)}}
	state := pushState{Version: 1, PrivateKey: encode(private), PublicKey: encode(public), Subscriptions: map[string]webpush.Subscription{login: sub}}
	marshal := func(v any) string {
		raw, e := json.Marshal(v)
		if e != nil {
			t.Fatal(e)
		}
		return string(raw)
	}
	base := marshal(state)
	noncanonical := func(s string) string {
		const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
		i := strings.IndexByte(alphabet, s[len(s)-1])
		return s[:len(s)-1] + string(alphabet[i|1])
	}
	var endpoints, keys, ids, states []rustPushCase
	add := func(out *[]rustPushCase, name, input string, valid bool, delta string) {
		*out = append(*out, rustPushCase{name, input, valid, valid && delta == "", delta})
	}
	for _, c := range []struct{ name, value, delta string }{
		{"apple", sub.Endpoint, ""},
		{"google", "https://fcm.googleapis.com/fcm/send/fixture", ""},
		{"mozilla", "https://updates.push.services.mozilla.com/wpush/fixture", ""},
		{"mozilla-subdomain", "https://a.push.services.mozilla.com/fixture", ""},
		{"windows", "https://wns2-fixture.notify.windows.com/fixture", ""},
		{"uppercase-host", "https://WEB.PUSH.APPLE.COM/fixture", ""},
		{"uppercase-scheme", "HTTPS://web.push.apple.com/fixture", "strict lowercase scheme"},
		{"encoded-path", "https://web.push.apple.com/a%2Fb?channel=x%20y", ""},
		{"http", "http://web.push.apple.com/fixture", ""},
		{"opaque", "https:web.push.apple.com/fixture", ""},
		{"userinfo", "https://user@web.push.apple.com/fixture", ""},
		{"port", "https://web.push.apple.com:443/fixture", ""},
		{"empty-port", "https://web.push.apple.com:/fixture", "strict authority without port delimiter"},
		{"fragment", "https://web.push.apple.com/fixture#x", ""},
		{"empty-fragment", "https://web.push.apple.com/fixture#", "strict fragment delimiter rejection"},
		{"other-host", "https://web.push.apple.com.evil.example/fixture", ""},
		{"private-ip", "https://127.0.0.1/fixture", ""},
		{"bare-mozilla", "https://push.services.mozilla.com/fixture", ""},
		{"trailing-dot", "https://web.push.apple.com./fixture", ""},
		{"unicode-path", "https://web.push.apple.com/한글", "strict ASCII URL"},
		{"space-path", "https://web.push.apple.com/a b", "strict whitespace rejection"},
		{"bad-path-escape", "https://web.push.apple.com/%ZZ", ""},
		{"bad-query-escape", "https://web.push.apple.com/?x=%ZZ", "strict percent escape rejection"},
		{"empty-host", "https:///fixture", ""},
		{"exact-length", "https://web.push.apple.com/" + strings.Repeat("a", 2048-len("https://web.push.apple.com/")), ""},
		{"too-long", "https://web.push.apple.com/" + strings.Repeat("a", 2049-len("https://web.push.apple.com/")), ""},
	} {
		v := sub
		v.Endpoint = c.value
		add(&endpoints, c.name, c.value, validatePushSubscription(v) == nil, c.delta)
	}
	compressed := append([]byte{2 + (public[64] & 1)}, public[1:33]...)
	for _, c := range []struct{ name, auth, pub, delta string }{
		{"valid", sub.Keys.Auth, sub.Keys.P256dh, ""},
		{"short-auth", encode(bytes.Repeat([]byte{7}, 15)), sub.Keys.P256dh, ""},
		{"long-auth", encode(bytes.Repeat([]byte{7}, 17)), sub.Keys.P256dh, ""},
		{"padded-auth", sub.Keys.Auth + "==", sub.Keys.P256dh, ""},
		{"newline-auth", sub.Keys.Auth + "\n", sub.Keys.P256dh, "canonical base64"},
		{"trailing-bits-auth", noncanonical(sub.Keys.Auth), sub.Keys.P256dh, "canonical base64"},
		{"invalid-pub", sub.Keys.Auth, encode(bytes.Repeat([]byte{4}, 65)), ""},
		{"compressed-pub", sub.Keys.Auth, encode(compressed), ""},
		{"short-pub", sub.Keys.Auth, encode(public[:64]), ""},
		{"long-pub", sub.Keys.Auth, encode(append(append([]byte{}, public...), 0)), ""},
		{"newline-pub", sub.Keys.Auth, sub.Keys.P256dh + "\n", "canonical base64"},
		{"trailing-bits-pub", sub.Keys.Auth, noncanonical(sub.Keys.P256dh), "canonical base64"},
		{"empty", "", "", ""},
	} {
		v := sub
		v.Keys = webpush.Keys{Auth: c.auth, P256dh: c.pub}
		add(&keys, c.name, marshal(v), validatePushSubscription(v) == nil, c.delta)
	}
	for _, c := range []struct{ name, value, delta string }{
		{"valid", login, ""}, {"short", encode(bytes.Repeat([]byte{1}, 31)), ""}, {"long", encode(bytes.Repeat([]byte{1}, 33)), ""},
		{"padded", login + "=", ""}, {"invalid", strings.Repeat("*", 43), ""}, {"empty", "", ""},
		{"newline", login + "\r\n", "canonical base64"}, {"trailing-bits", noncanonical(login), "canonical base64"},
	} {
		add(&ids, c.name, c.value, validPushID(c.value), c.delta)
	}
	path := filepath.Join(t.TempDir(), "push.json")
	check := func(name, raw, delta string) {
		if e := os.WriteFile(path, []byte(raw), 0600); e != nil {
			t.Fatal(e)
		}
		owner, e := newPushStore(path)
		if e == nil {
			owner.close()
		}
		after, e2 := os.ReadFile(path)
		if e2 != nil || string(after) != raw {
			t.Fatal("decoder overwrote original")
		}
		add(&states, name, raw, e == nil, delta)
	}
	check("valid", base, "")
	for _, c := range []struct{ name, old, next, delta string }{
		{"version-zero", "\"version\":1", "\"version\":0", ""},
		{"version-null", "\"version\":1", "\"version\":null", ""},
		{"unknown", "\"version\":1", "\"version\":1,\"extra\":true", "strict unknown fields"},
		{"duplicate", "\"version\":1", "\"version\":0,\"version\":1", "strict duplicate fields"},
		{"unknown-sub", "\"endpoint\":", "\"extra\":true,\"endpoint\":", "strict unknown fields"},
		{"unknown-key", "\"auth\":", "\"extra\":true,\"auth\":", "strict unknown fields"},
		{"duplicate-endpoint", "\"endpoint\":", "\"endpoint\":\"https://invalid.example/x\",\"endpoint\":", "strict duplicate fields"},
		{"zero-private", marshal(state.PrivateKey), marshal(encode(make([]byte, 32))), ""},
		{"large-private", marshal(state.PrivateKey), marshal(encode(bytes.Repeat([]byte{255}, 32))), ""},
		{"short-private", marshal(state.PrivateKey), marshal(encode(private[:31])), ""},
		{"newline-private", marshal(state.PrivateKey), marshal(state.PrivateKey + "\n"), "canonical base64"},
		{"trailing-bits-private", marshal(state.PrivateKey), marshal(noncanonical(state.PrivateKey)), "canonical base64"},
		{"wrong-public", marshal(state.PublicKey), marshal(encode(bytes.Repeat([]byte{4}, 65))), ""},
		{"bad-login", marshal(login), marshal("bad"), ""},
	} {
		check(c.name, strings.Replace(base, c.old, c.next, 1), c.delta)
	}
	empty := state
	empty.Subscriptions = nil
	check("null-subscriptions", marshal(empty), "")
	empty.Subscriptions = map[string]webpush.Subscription{}
	check("empty-subscriptions", marshal(empty), "")
	check("missing-subscriptions", marshal(map[string]any{"version": 1, "public_key": state.PublicKey, "private_key": state.PrivateKey}), "")
	check("array", "[]", "")
	check("null", "null", "")
	check("trailing", base+"{}", "")
	check("positional-state", "[1,"+marshal(state.PublicKey)+","+marshal(state.PrivateKey)+","+marshal(state.Subscriptions)+"]", "")
	check("positional-sub", strings.Replace(base, marshal(sub), marshal([]any{sub.Endpoint, sub.Keys}), 1), "")
	check("positional-keys", strings.Replace(base, marshal(sub.Keys), marshal([]any{sub.Keys.Auth, sub.Keys.P256dh}), 1), "")
	check("escaped-login", strings.Replace(base, marshal(login), "\"\\u0041"+login[1:]+"\"", 1), "")
	check("duplicate-login", strings.Replace(base, marshal(login)+":"+marshal(sub), marshal(login)+":"+marshal(sub)+","+marshal(login)+":"+marshal(sub), 1), "strict duplicate map keys")
	out := struct {
		State        string               `json:"state"`
		Subscription webpush.Subscription `json:"subscription"`
		LoginID      string               `json:"login_id"`
		Endpoints    []rustPushCase       `json:"endpoints"`
		Keys         []rustPushCase       `json:"keys"`
		IDs          []rustPushCase       `json:"ids"`
		States       []rustPushCase       `json:"states"`
	}{base, sub, login, endpoints, keys, ids, states}
	raw, e := json.MarshalIndent(out, "", "  ")
	if e != nil {
		t.Fatal(e)
	}
	raw = append(raw, '\n')
	target := filepath.Join("..", "..", "tests", "fixtures", "push-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_PUSH_FIXTURE") == "1" {
		if e := os.MkdirAll(filepath.Dir(target), 0755); e != nil {
			t.Fatal(e)
		}
		if e := os.WriteFile(target, raw, 0644); e != nil {
			t.Fatal(e)
		}
	}
	want, e := os.ReadFile(target)
	if e != nil {
		t.Fatal(e)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go push state oracle changed")
	}
}

// Current-state handoff uses Rust's generated VAPID key. Go validates it and
// persists removal, then Rust must read that current state without restoring it.
func TestRustPushStateHandoff(t *testing.T) {
	dir := os.Getenv("HMUX_RUST_PUSH_HANDOFF")
	if dir == "" {
		t.Skip("isolated Rust/Go handoff")
	}
	if !filepath.IsAbs(dir) {
		t.Fatal("absolute isolated path required")
	}
	path := filepath.Join(dir, "synthetic-credentials.json.push.json")
	if os.Getenv("HMUX_RUST_PUSH_EXPECT_LOCKED") == "1" {
		before, e := os.ReadFile(path)
		if e != nil {
			t.Fatal(e)
		}
		owner, e := newPushStore(path)
		if e == nil {
			owner.close()
			t.Fatal("Go acquired Rust lifetime lock")
		}
		after, e := os.ReadFile(path)
		if e != nil || !bytes.Equal(before, after) {
			t.Fatal("lock conflict changed current state")
		}
		fmt.Fprintln(os.Stdout, "Go respected Rust lifetime lock")
		return
	}
	p, e := newPushStore(path)
	if e != nil {
		t.Fatal(e)
	}
	defer p.close()
	public, e := os.ReadFile(filepath.Join(dir, "expected-public.txt"))
	if e != nil || string(public) != p.state.PublicKey {
		t.Fatal("VAPID key changed")
	}
	a := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{1}, 32))
	b := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{2}, 32))
	if len(p.state.Subscriptions) != 2 || p.state.Subscriptions[a].Endpoint == "" || p.state.Subscriptions[b].Endpoint == "" {
		t.Fatal("Rust subscriptions unreadable")
	}
	if e := p.remove(a, ""); e != nil {
		t.Fatal(e)
	}
	if len(p.state.Subscriptions) != 1 || p.state.Subscriptions[b].Endpoint == "" {
		t.Fatal("wrong removal")
	}
	if p.state.PublicKey != string(public) {
		t.Fatal("VAPID replaced on removal")
	}
	fmt.Fprintln(os.Stdout, "Rust push key/state accepted; Go removal persisted")
}

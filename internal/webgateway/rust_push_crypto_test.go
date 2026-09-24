package webgateway

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdh"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	webpush "github.com/SherClockHolmes/webpush-go"
	"golang.org/x/crypto/hkdf"
)

// Every private key and endpoint in these isolated handoffs is synthetic.
// The existing Go sender is intercepted before any network operation.
type rustPushCryptoRequest struct {
	Endpoint        string               `json:"endpoint"`
	Headers         map[string]string    `json:"headers"`
	Body            string               `json:"body"`
	Plaintext       string               `json:"plaintext"`
	LoginID         string               `json:"login_id"`
	Origin          string               `json:"origin"`
	Now             int64                `json:"now"`
	Subscription    webpush.Subscription `json:"subscription"`
	ReceiverPrivate string               `json:"receiver_private"`
	VAPIDPublic     string               `json:"vapid_public"`
	VAPIDPrivate    string               `json:"vapid_private"`
}

func rustPushDecode(s string) ([]byte, error) { return base64.RawURLEncoding.DecodeString(s) }

func rustPushDecrypt(body, auth, private []byte) ([]byte, error) {
	if len(body) < 103 || len(body) > 4096 || binary.BigEndian.Uint32(body[16:20]) != 4096 || body[20] != 65 {
		return nil, errors.New("invalid encrypted record")
	}
	receiver, e := ecdh.P256().NewPrivateKey(private)
	if e != nil {
		return nil, e
	}
	sender, e := ecdh.P256().NewPublicKey(body[21:86])
	if e != nil {
		return nil, e
	}
	secret, e := receiver.ECDH(sender)
	if e != nil {
		return nil, e
	}
	info := append([]byte("WebPush: info\x00"), receiver.PublicKey().Bytes()...)
	info = append(info, sender.Bytes()...)
	ikm := make([]byte, 32)
	if _, e = io.ReadFull(hkdf.New(sha256.New, secret, auth, info), ikm); e != nil {
		return nil, e
	}
	cek := make([]byte, 16)
	nonce := make([]byte, 12)
	if _, e = io.ReadFull(hkdf.New(sha256.New, ikm, body[:16], []byte("Content-Encoding: aes128gcm\x00")), cek); e != nil {
		return nil, e
	}
	if _, e = io.ReadFull(hkdf.New(sha256.New, ikm, body[:16], []byte("Content-Encoding: nonce\x00")), nonce); e != nil {
		return nil, e
	}
	block, e := aes.NewCipher(cek)
	if e != nil {
		return nil, e
	}
	aead, e := cipher.NewGCM(block)
	if e != nil {
		return nil, e
	}
	padded, e := aead.Open(nil, nonce, body[86:], nil)
	if e != nil {
		return nil, e
	}
	i := len(padded) - 1
	for i >= 0 && padded[i] == 0 {
		i--
	}
	if i < 0 || padded[i] != 2 {
		return nil, errors.New("invalid padding delimiter")
	}
	return padded[:i], nil
}

func rustPushCheckRequest(t *testing.T, r rustPushCryptoRequest, minimumExpiry, maximumExpiry int64) {
	t.Helper()
	body, e := rustPushDecode(r.Body)
	if e != nil || len(body) != 4096 {
		t.Fatal("body bound")
	}
	if r.Endpoint != r.Subscription.Endpoint || validatePushSubscription(r.Subscription) != nil || !validPushID(r.LoginID) {
		t.Fatal("request identity")
	}
	plain, e := rustPushDecode(r.Plaintext)
	if e != nil {
		t.Fatal(e)
	}
	auth, e := rustPushDecode(r.Subscription.Keys.Auth)
	if e != nil {
		t.Fatal(e)
	}
	private, e := rustPushDecode(r.ReceiverPrivate)
	if e != nil {
		t.Fatal(e)
	}
	decoded, e := rustPushDecrypt(body, auth, private)
	if e != nil || !bytes.Equal(decoded, plain) {
		t.Fatal("independent Go decryption mismatch")
	}
	// Authentication is checked independently of any Rust decrypt implementation.
	altered := append([]byte{}, body...)
	altered[len(altered)-1] ^= 1
	if _, e = rustPushDecrypt(altered, auth, private); e == nil {
		t.Fatal("modified ciphertext accepted")
	}
	headers := http.Header{}
	for key, value := range r.Headers {
		headers.Set(key, value)
	}
	for key, value := range map[string]string{"Content-Encoding": "aes128gcm", "Content-Type": "application/octet-stream", "TTL": "120", "Urgency": "normal"} {
		if headers.Get(key) != value {
			t.Fatalf("wrong %s", key)
		}
	}
	hash := sha256.Sum256(append([]byte(r.LoginID), plain...))
	if headers.Get("Topic") != base64.RawURLEncoding.EncodeToString(hash[:24]) {
		t.Fatal("topic hash differs")
	}
	token, public, ok := strings.Cut(strings.TrimPrefix(headers.Get("Authorization"), "vapid t="), ", k=")
	if !ok || !strings.HasPrefix(headers.Get("Authorization"), "vapid t=") || public != r.VAPIDPublic {
		t.Fatal("VAPID authorization format")
	}
	pieces := strings.Split(token, ".")
	if len(pieces) != 3 {
		t.Fatal("JWS format")
	}
	headerRaw, e := rustPushDecode(pieces[0])
	if e != nil {
		t.Fatal(e)
	}
	var head struct {
		Alg string `json:"alg"`
		Typ string `json:"typ"`
	}
	if e = json.Unmarshal(headerRaw, &head); e != nil || head.Alg != "ES256" || head.Typ != "JWT" {
		t.Fatal("JWS algorithm")
	}
	claimsRaw, e := rustPushDecode(pieces[1])
	if e != nil {
		t.Fatal(e)
	}
	var claims struct {
		Audience string `json:"aud"`
		Subject  string `json:"sub"`
		Expires  int64  `json:"exp"`
	}
	if e = json.Unmarshal(claimsRaw, &claims); e != nil {
		t.Fatal(e)
	}
	authority := strings.SplitN(strings.TrimPrefix(r.Endpoint, "https://"), "/", 2)[0]
	authority = strings.SplitN(authority, "?", 2)[0]
	if claims.Audience != "https://"+strings.ToLower(authority) || claims.Subject != r.Origin || claims.Expires < minimumExpiry || claims.Expires > maximumExpiry {
		t.Fatal("VAPID claims")
	}
	publicRaw, e := rustPushDecode(public)
	if e != nil {
		t.Fatal(e)
	}
	x, y := elliptic.Unmarshal(elliptic.P256(), publicRaw)
	if x == nil {
		t.Fatal("invalid VAPID public point")
	}
	sig, e := rustPushDecode(pieces[2])
	if e != nil || len(sig) != 64 {
		t.Fatal("JWS needs fixed-width r||s")
	}
	digest := sha256.Sum256([]byte(pieces[0] + "." + pieces[1]))
	key := ecdsa.PublicKey{Curve: elliptic.P256(), X: x, Y: y}
	if !ecdsa.Verify(&key, digest[:], new(big.Int).SetBytes(sig[:32]), new(big.Int).SetBytes(sig[32:])) {
		t.Fatal("Go rejected ES256 signature")
	}
	digest[0] ^= 1
	if ecdsa.Verify(&key, digest[:], new(big.Int).SetBytes(sig[:32]), new(big.Int).SetBytes(sig[32:])) {
		t.Fatal("signature accepted modified claims")
	}
}

type rustPushCapture struct {
	request *http.Request
	body    []byte
}

func (c *rustPushCapture) Do(r *http.Request) (*http.Response, error) {
	if c.request != nil {
		return nil, errors.New("unexpected second request")
	}
	body, e := io.ReadAll(io.LimitReader(r.Body, 4097))
	if e != nil {
		return nil, e
	}
	c.request = r
	c.body = body
	return &http.Response{StatusCode: 201, Body: io.NopCloser(strings.NewReader("")), Header: make(http.Header)}, nil
}

func rustPushActualGoRequest(t *testing.T, input rustPushCryptoRequest) rustPushCryptoRequest {
	t.Helper()
	plain, e := rustPushDecode(input.Plaintext)
	if e != nil {
		t.Fatal(e)
	}
	hash := sha256.Sum256(append([]byte(input.LoginID), plain...))
	capture := &rustPushCapture{}
	before := time.Now().Unix()
	response, e := webpush.SendNotificationWithContext(context.Background(), plain, &input.Subscription, &webpush.Options{
		HTTPClient: capture, Subscriber: input.Origin, TTL: 120, Urgency: webpush.UrgencyNormal,
		Topic: base64.RawURLEncoding.EncodeToString(hash[:24]), VAPIDPublicKey: input.VAPIDPublic, VAPIDPrivateKey: input.VAPIDPrivate,
	})
	if e != nil {
		t.Fatal(e)
	}
	response.Body.Close()
	after := time.Now().Unix()
	if capture.request.Method != "POST" || capture.request.URL.String() != input.Endpoint {
		t.Fatal("Go request target")
	}
	output := input
	output.Now = after
	output.Headers = map[string]string{}
	for k := range capture.request.Header {
		output.Headers[k] = capture.request.Header.Get(k)
	}
	output.Body = base64.RawURLEncoding.EncodeToString(capture.body)
	rustPushCheckRequest(t, output, before+12*3600, after+12*3600)
	return output
}

// Verify the bytes captured after real Rust HTTP/TLS delivery, independently of
// the Rust test's receiver. This fixture never contacts a provider.
func TestRustPushDeliveredRequest(t *testing.T) {
	path := os.Getenv("HMUX_PUSH_DELIVERY_CAPTURE")
	if path == "" {
		t.Skip("isolated Rust HTTPS capture")
	}
	if !filepath.IsAbs(path) {
		t.Fatal("absolute synthetic path required")
	}
	raw, err := os.ReadFile(path)
	if err != nil || len(raw) > 32<<10 {
		t.Fatal("invalid capture input")
	}
	var request rustPushCryptoRequest
	if err = json.Unmarshal(raw, &request); err != nil {
		t.Fatal(err)
	}
	rustPushCheckRequest(t, request, request.Now+12*3600-5, request.Now+12*3600)
	plain, err := rustPushDecode(request.Plaintext)
	if err != nil {
		t.Fatal(err)
	}
	var payload struct {
		Type    string `json:"type"`
		Tab     string `json:"tab_name"`
		Session struct {
			ID        string `json:"id"`
			CreatedAt int64  `json:"created_at"`
		} `json:"session"`
		LoginID string `json:"login_id"`
		EventID string `json:"event_id"`
	}
	if err = json.Unmarshal(plain, &payload); err != nil || payload.Type != "codex-complete" || payload.Tab != "배포 작업" || payload.Session.ID != "$1" || payload.Session.CreatedAt != 42 || payload.LoginID != request.LoginID || len(payload.EventID) != 64 {
		t.Fatal("browser deep-link payload mismatch")
	}
}

func TestRustPushCryptoHandoff(t *testing.T) {
	dir := os.Getenv("HMUX_RUST_PUSH_CRYPTO")
	if dir == "" {
		t.Skip("isolated Rust/Go crypto handoff")
	}
	if !filepath.IsAbs(dir) {
		t.Fatal("absolute synthetic path required")
	}
	raw, e := os.ReadFile(filepath.Join(dir, "rust-requests.json"))
	if e != nil || len(raw) > 128<<10 {
		t.Fatal("invalid handoff input")
	}
	var requests []rustPushCryptoRequest
	if e = json.Unmarshal(raw, &requests); e != nil || len(requests) != 3 {
		t.Fatal("invalid request fixtures")
	}
	var outputs []rustPushCryptoRequest
	for _, request := range requests {
		rustPushCheckRequest(t, request, request.Now+12*3600, request.Now+12*3600)
		outputs = append(outputs, rustPushActualGoRequest(t, request))
	}
	raw, e = json.Marshal(outputs)
	if e != nil {
		t.Fatal(e)
	}
	if e = os.WriteFile(filepath.Join(dir, "go-requests.json"), raw, 0600); e != nil {
		t.Fatal(e)
	}
	fmt.Fprintln(os.Stdout, "Go decrypted Rust Web Push and verified ES256; actual Go sender captured without network")
}

func TestRustPushCryptoRFCAndGoSender(t *testing.T) {
	// RFC 8291 section 5; validates the independent Go decryptor itself.
	bodyText := "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml" +
		"mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT" +
		"pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
	body, _ := rustPushDecode(bodyText)
	auth, _ := rustPushDecode("BTBZMqHH6r4Tts7J_aSIgg")
	private, _ := rustPushDecode("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94")
	plain, e := rustPushDecrypt(body, auth, private)
	if e != nil || string(plain) != "When I grow up, I want to be a watermelon" {
		t.Fatal("RFC8291 vector mismatch")
	}
	vapidPrivate := bytes.Repeat([]byte{9}, 32)
	vapid, e := ecdh.P256().NewPrivateKey(vapidPrivate)
	if e != nil {
		t.Fatal(e)
	}
	receiver, e := ecdh.P256().NewPrivateKey(private)
	if e != nil {
		t.Fatal(e)
	}
	encode := base64.RawURLEncoding.EncodeToString
	sub := webpush.Subscription{Endpoint: "https://web.push.apple.com/synthetic", Keys: webpush.Keys{Auth: encode(auth), P256dh: encode(receiver.PublicKey().Bytes())}}
	for _, payload := range [][]byte{nil, []byte("synthetic 한글 \x00\xff"), bytes.Repeat([]byte{0x66}, 3993)} {
		fixture := rustPushCryptoRequest{Endpoint: sub.Endpoint, Plaintext: encode(payload), LoginID: encode(bytes.Repeat([]byte{1}, 32)),
			Origin: "https://hmux.example", Subscription: sub, ReceiverPrivate: encode(private), VAPIDPublic: encode(vapid.PublicKey().Bytes()), VAPIDPrivate: encode(vapidPrivate)}
		rustPushActualGoRequest(t, fixture)
	}
}

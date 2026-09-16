package catalogstream

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
)

func TestBrokerIsLoopbackPinnedAuthenticatedAndOneUse(t *testing.T) {
	now := time.Now().UTC()
	broker, err := NewBroker(now)
	if err != nil {
		t.Fatal(err)
	}
	bootstrap := broker.Bootstrap()
	if err := ValidateBootstrap(bootstrap, now); err != nil {
		t.Fatal(err)
	}
	certificate, err := x509.ParseCertificate(broker.tlsConfig.Certificates[0].Certificate[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(certificate.IPAddresses) != 1 || certificate.IPAddresses[0].String() != "127.0.0.1" ||
		len(certificate.DNSNames) != 0 || certificate.NotAfter.Sub(now) > bootstrapLifetime+time.Second {
		t.Fatalf("certificate SAN/expiry=%#v %v", certificate.IPAddresses, certificate.NotAfter)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	updates := make(chan model.Catalog, 1)
	sourceErrors := make(chan error, 1)
	serveDone := make(chan error, 1)
	go func() { serveDone <- broker.Serve(ctx, updates, sourceErrors) }()

	wrongPinClient := pinnedHTTPClient(t, "00"+bootstrap.CertificateSHA256[2:])
	if _, _, err := websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{HTTPClient: wrongPinClient}); err == nil {
		t.Fatal("mismatched certificate pin was accepted")
	}

	wrongTokenHeaders := http.Header{"Authorization": []string{"Bearer wrong"}}
	_, response, err := websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{
		HTTPClient: pinnedHTTPClient(t, bootstrap.CertificateSHA256), HTTPHeader: wrongTokenHeaders,
	})
	if err == nil || response == nil || response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("wrong token err=%v response=%v", err, response)
	}

	originHeaders := http.Header{
		"Authorization": []string{"Bearer " + bootstrap.Token},
		"Origin":        []string{"https://example.invalid"},
	}
	_, response, err = websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{
		HTTPClient: pinnedHTTPClient(t, bootstrap.CertificateSHA256), HTTPHeader: originHeaders,
	})
	if err == nil || response == nil || response.StatusCode != http.StatusForbidden {
		t.Fatalf("browser origin err=%v response=%v", err, response)
	}

	headers := http.Header{"Authorization": []string{"Bearer " + bootstrap.Token}}
	connection, _, err := websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{
		HTTPClient: pinnedHTTPClient(t, bootstrap.CertificateSHA256), HTTPHeader: headers,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer connection.CloseNow()
	connection.SetReadLimit(MaximumFrameSize)

	_, response, err = websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{
		HTTPClient: pinnedHTTPClient(t, bootstrap.CertificateSHA256), HTTPHeader: headers,
	})
	if err == nil || response == nil || response.StatusCode != http.StatusConflict {
		t.Fatalf("replayed token err=%v response=%v", err, response)
	}

	updates <- model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		GeneratedAt:     now,
		Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: "streamed"}},
	}
	readCtx, readCancel := context.WithTimeout(ctx, 3*time.Second)
	defer readCancel()
	messageType, data, err := connection.Read(readCtx)
	if err != nil || messageType != websocket.MessageText {
		t.Fatalf("read type=%v err=%v", messageType, err)
	}
	var message WebSocketMessage
	if err := json.Unmarshal(data, &message); err != nil {
		t.Fatal(err)
	}
	if message.Sequence != 1 || message.StreamID == "" || message.Data.Sessions[0].Name != "streamed" {
		t.Fatalf("message=%#v", message)
	}
	cancel()
	select {
	case <-serveDone:
	case <-time.After(3 * time.Second):
		t.Fatal("broker did not stop")
	}
}

func TestValidateBootstrapRejectsAuthorityConfusion(t *testing.T) {
	value := UnsupportedBootstrap()
	value.Supported = true
	value.URL = "wss://127.0.0.1:443@evil.invalid/catalog"
	value.Token = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO"
	value.CertificateSHA256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
	value.ExpiresAt = time.Now().Add(time.Minute)
	if err := ValidateBootstrap(value, time.Now()); err == nil {
		t.Fatal("confused URL authority was accepted")
	}
}

func TestBrokerConnectionSurvivesInitialConnectionDeadline(t *testing.T) {
	broker, err := NewBroker(time.Now())
	if err != nil {
		t.Fatal(err)
	}
	broker.connectTimeout = time.Second
	bootstrap := broker.Bootstrap()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	updates := make(chan model.Catalog, 1)
	sourceErrors := make(chan error, 1)
	serveDone := make(chan error, 1)
	go func() { serveDone <- broker.Serve(ctx, updates, sourceErrors) }()

	headers := http.Header{"Authorization": []string{"Bearer " + bootstrap.Token}}
	connection, _, err := websocket.Dial(ctx, bootstrap.URL, &websocket.DialOptions{
		HTTPClient: pinnedHTTPClient(t, bootstrap.CertificateSHA256), HTTPHeader: headers,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer connection.CloseNow()
	time.Sleep(1250 * time.Millisecond)
	updates <- model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: "still-live"}},
	}
	readCtx, readCancel := context.WithTimeout(ctx, 3*time.Second)
	defer readCancel()
	_, data, err := connection.Read(readCtx)
	if err != nil {
		t.Fatalf("stream ended at the initial connection deadline: %v", err)
	}
	var message WebSocketMessage
	if err := json.Unmarshal(data, &message); err != nil || message.Data.Sessions[0].Name != "still-live" {
		t.Fatalf("message=%#v decode=%v", message, err)
	}
	select {
	case err := <-serveDone:
		t.Fatalf("broker stopped after accepted connection: %v", err)
	default:
	}
}

func pinnedHTTPClient(t *testing.T, expectedHex string) *http.Client {
	t.Helper()
	expected, err := hex.DecodeString(expectedHex)
	if err != nil {
		t.Fatal(err)
	}
	transport := &http.Transport{TLSClientConfig: &tls.Config{
		MinVersion:         tls.VersionTLS13,
		InsecureSkipVerify: true, // Exact ephemeral leaf pin is verified below.
		VerifyConnection: func(state tls.ConnectionState) error {
			if len(state.PeerCertificates) != 1 {
				return errors.New("unexpected certificate chain")
			}
			actual := sha256.Sum256(state.PeerCertificates[0].Raw)
			if !equalBytes(actual[:], expected) {
				return errors.New("certificate pin mismatch")
			}
			return nil
		},
	}}
	return &http.Client{Transport: transport, Timeout: 3 * time.Second}
}

func equalBytes(left, right []byte) bool {
	if len(left) != len(right) {
		return false
	}
	var different byte
	for index := range left {
		different |= left[index] ^ right[index]
	}
	return different == 0
}

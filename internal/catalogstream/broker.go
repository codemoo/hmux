package catalogstream

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
)

const (
	AppProtocolVersion = 1
	bootstrapLifetime  = 5 * time.Minute
	connectTimeout     = 15 * time.Second
	pingInterval       = 10 * time.Second
	pingTimeout        = 5 * time.Second
)

type Bootstrap struct {
	AppProtocolVersion    int       `json:"app_protocol_version"`
	StreamProtocolVersion int       `json:"stream_protocol_version"`
	Supported             bool      `json:"supported"`
	URL                   string    `json:"url,omitempty"`
	Token                 string    `json:"token,omitempty"`
	CertificateSHA256     string    `json:"certificate_sha256,omitempty"`
	ExpiresAt             time.Time `json:"expires_at,omitempty"`
	MaximumFrameBytes     int       `json:"maximum_frame_bytes"`
	WorkspaceSourceKey    string    `json:"workspace_source_key,omitempty"`
}

type WebSocketMessage struct {
	AppProtocolVersion    int           `json:"app_protocol_version"`
	StreamProtocolVersion int           `json:"stream_protocol_version"`
	StreamID              string        `json:"stream_id"`
	Sequence              uint64        `json:"sequence"`
	Type                  string        `json:"type"`
	Data                  model.Catalog `json:"data"`
}

type Broker struct {
	listener       net.Listener
	tlsConfig      *tls.Config
	token          string
	streamID       string
	bootstrap      Bootstrap
	connectTimeout time.Duration
	consumed       atomic.Bool
	closeOnce      sync.Once
}

func UnsupportedBootstrap() Bootstrap {
	return Bootstrap{
		AppProtocolVersion:    AppProtocolVersion,
		StreamProtocolVersion: ProtocolVersion,
		Supported:             false,
		MaximumFrameBytes:     MaximumFrameSize,
	}
}

func NewBroker(now time.Time) (*Broker, error) {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("listen on loopback: %w", err)
	}
	fail := func(err error) (*Broker, error) {
		_ = listener.Close()
		return nil, err
	}
	tcpAddress, ok := listener.Addr().(*net.TCPAddr)
	if !ok || tcpAddress.IP == nil || !tcpAddress.IP.Equal(net.IPv4(127, 0, 0, 1)) || tcpAddress.Port < 1 {
		return fail(errors.New("catalog stream listener is not loopback-only"))
	}
	certificate, der, expiresAt, err := ephemeralCertificate(now)
	if err != nil {
		return fail(err)
	}
	token, err := randomText(32)
	if err != nil {
		return fail(err)
	}
	streamID, err := randomText(18)
	if err != nil {
		return fail(err)
	}
	pin := sha256.Sum256(der)
	return &Broker{
		listener: listener,
		tlsConfig: &tls.Config{
			Certificates: []tls.Certificate{certificate},
			MinVersion:   tls.VersionTLS13,
		},
		token:          token,
		streamID:       streamID,
		connectTimeout: connectTimeout,
		bootstrap: Bootstrap{
			AppProtocolVersion:    AppProtocolVersion,
			StreamProtocolVersion: ProtocolVersion,
			Supported:             true,
			URL:                   fmt.Sprintf("wss://127.0.0.1:%d/catalog", tcpAddress.Port),
			Token:                 token,
			CertificateSHA256:     hex.EncodeToString(pin[:]),
			ExpiresAt:             expiresAt,
			MaximumFrameBytes:     MaximumFrameSize,
		},
	}, nil
}

func (b *Broker) Bootstrap() Bootstrap { return b.bootstrap }

func (b *Broker) Close() {
	b.closeOnce.Do(func() { _ = b.listener.Close() })
}

// Serve accepts exactly one authenticated loopback WebSocket and forwards the
// latest catalog. The updates channel should have capacity one so producers can
// replace stale pending snapshots instead of building an unbounded queue.
func (b *Broker) Serve(ctx context.Context, updates <-chan model.Catalog, sourceErrors <-chan error) error {
	if b == nil || b.listener == nil || b.tlsConfig == nil {
		return errors.New("invalid catalog stream broker")
	}
	defer b.Close()
	handlerDone := make(chan error, 1)
	connected := make(chan struct{}, 1)
	server := &http.Server{
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       30 * time.Second,
		MaxHeaderBytes:    16 * 1024,
	}
	server.Handler = http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		accepted, err := b.handle(ctx, writer, request, updates, sourceErrors, connected)
		if accepted {
			handlerDone <- err
		}
	})
	serverErrors := make(chan error, 1)
	go func() {
		err := server.Serve(tls.NewListener(b.listener, b.tlsConfig))
		if !errors.Is(err, http.ErrServerClosed) {
			serverErrors <- err
		}
	}()

	connectionDeadline := b.connectTimeout
	if connectionDeadline <= 0 {
		connectionDeadline = connectTimeout
	}
	timer := time.NewTimer(connectionDeadline)
	defer timer.Stop()
	timeout := timer.C
	var result error
	finished := false
	for !finished {
		select {
		case <-ctx.Done():
			result = ctx.Err()
			finished = true
		case err := <-sourceErrors:
			result = err
			finished = true
		case err := <-handlerDone:
			result = err
			finished = true
		case err := <-serverErrors:
			result = err
			finished = true
		case <-connected:
			if timeout != nil {
				if !timer.Stop() {
					select {
					case <-timer.C:
					default:
					}
				}
				timeout = nil
			}
		case <-timeout:
			result = errors.New("catalog stream client connection timed out")
			finished = true
		}
	}
	shutdownCtx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_ = server.Shutdown(shutdownCtx)
	return result
}

func (b *Broker) handle(
	ctx context.Context,
	writer http.ResponseWriter,
	request *http.Request,
	updates <-chan model.Catalog,
	sourceErrors <-chan error,
	connected chan<- struct{},
) (bool, error) {
	if request.Method != http.MethodGet || request.URL.Path != "/catalog" || request.URL.RawQuery != "" {
		http.Error(writer, "not found", http.StatusNotFound)
		return false, errors.New("invalid catalog stream endpoint")
	}
	host, _, err := net.SplitHostPort(request.RemoteAddr)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		http.Error(writer, "forbidden", http.StatusForbidden)
		return false, errors.New("catalog stream client is not loopback")
	}
	if request.Header.Get("Origin") != "" {
		http.Error(writer, "forbidden", http.StatusForbidden)
		return false, errors.New("catalog stream browser origin is forbidden")
	}
	wantAuthorization := []byte("Bearer " + b.token)
	gotAuthorization := []byte(request.Header.Get("Authorization"))
	if len(gotAuthorization) != len(wantAuthorization) || subtle.ConstantTimeCompare(gotAuthorization, wantAuthorization) != 1 {
		http.Error(writer, "unauthorized", http.StatusUnauthorized)
		return false, errors.New("catalog stream authentication failed")
	}
	if !b.consumed.CompareAndSwap(false, true) {
		http.Error(writer, "conflict", http.StatusConflict)
		return false, errors.New("catalog stream token was already consumed")
	}
	writer.Header().Set("Cache-Control", "no-store")
	connection, err := websocket.Accept(writer, request, &websocket.AcceptOptions{
		CompressionMode: websocket.CompressionDisabled,
	})
	if err != nil {
		return true, fmt.Errorf("accept catalog WebSocket: %w", err)
	}
	defer connection.CloseNow()
	select {
	case connected <- struct{}{}:
	default:
	}
	connection.SetReadLimit(1024)
	readContext := connection.CloseRead(ctx)

	var sequence uint64
	ping := time.NewTicker(pingInterval)
	defer ping.Stop()
	for {
		select {
		case <-ctx.Done():
			_ = connection.Close(websocket.StatusNormalClosure, "stream stopped")
			return true, ctx.Err()
		case <-readContext.Done():
			return true, errors.New("catalog WebSocket closed")
		case err := <-sourceErrors:
			_ = connection.Close(websocket.StatusInternalError, "source unavailable")
			return true, err
		case value, ok := <-updates:
			if !ok {
				_ = connection.Close(websocket.StatusNormalClosure, "source stopped")
				return true, nil
			}
			sequence++
			message := WebSocketMessage{
				AppProtocolVersion:    AppProtocolVersion,
				StreamProtocolVersion: ProtocolVersion,
				StreamID:              b.streamID,
				Sequence:              sequence,
				Type:                  "snapshot",
				Data:                  value,
			}
			data, err := json.Marshal(message)
			if err != nil || len(data) < 1 || len(data) > MaximumFrameSize {
				_ = connection.Close(websocket.StatusMessageTooBig, "snapshot rejected")
				return true, errors.New("catalog WebSocket snapshot exceeds size limit")
			}
			writeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
			err = connection.Write(writeCtx, websocket.MessageText, data)
			cancel()
			if err != nil {
				return true, fmt.Errorf("write catalog WebSocket: %w", err)
			}
		case <-ping.C:
			pingCtx, cancel := context.WithTimeout(ctx, pingTimeout)
			err := connection.Ping(pingCtx)
			cancel()
			if err != nil {
				return true, fmt.Errorf("catalog WebSocket ping: %w", err)
			}
		}
	}
}

func ephemeralCertificate(now time.Time) (tls.Certificate, []byte, time.Time, error) {
	privateKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return tls.Certificate{}, nil, time.Time{}, err
	}
	serialBytes := make([]byte, 16)
	if _, err := rand.Read(serialBytes); err != nil {
		return tls.Certificate{}, nil, time.Time{}, err
	}
	serial := new(big.Int).SetBytes(serialBytes)
	if serial.Sign() == 0 {
		serial.SetInt64(1)
	}
	expiresAt := now.Add(bootstrapLifetime).UTC()
	template := &x509.Certificate{
		SerialNumber: serial,
		Subject:      pkix.Name{CommonName: "HMux Ephemeral Catalog Stream"},
		NotBefore:    now.Add(-time.Minute).UTC(),
		NotAfter:     expiresAt,
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1)},
	}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &privateKey.PublicKey, privateKey)
	if err != nil {
		return tls.Certificate{}, nil, time.Time{}, err
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: privateKey}, der, expiresAt, nil
}

func randomText(size int) (string, error) {
	if size < 16 || size > 64 {
		return "", errors.New("invalid random value size")
	}
	data := make([]byte, size)
	if _, err := rand.Read(data); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(data), nil
}

func ValidateBootstrap(value Bootstrap, now time.Time) error {
	if value.WorkspaceSourceKey != "" {
		if len(value.WorkspaceSourceKey) != 64 {
			return errors.New("workspace source key is invalid")
		}
		for _, ch := range value.WorkspaceSourceKey {
			if (ch < '0' || ch > '9') && (ch < 'a' || ch > 'f') {
				return errors.New("workspace source key is invalid")
			}
		}
	}
	if value.AppProtocolVersion != AppProtocolVersion || value.StreamProtocolVersion != ProtocolVersion {
		return errors.New("catalog stream bootstrap protocol mismatch")
	}
	if !value.Supported {
		return nil
	}
	parsedURL, err := url.Parse(value.URL)
	if err != nil || parsedURL.Scheme != "wss" || parsedURL.Hostname() != "127.0.0.1" ||
		parsedURL.Path != "/catalog" || parsedURL.RawQuery != "" || parsedURL.User != nil {
		return errors.New("catalog stream bootstrap URL is invalid")
	}
	port, err := strconv.Atoi(parsedURL.Port())
	if err != nil || port < 1 || port > 65535 {
		return errors.New("catalog stream bootstrap port is invalid")
	}
	if len(value.Token) < 40 || len(value.Token) > 128 {
		return errors.New("catalog stream bootstrap token is invalid")
	}
	if len(value.CertificateSHA256) != sha256.Size*2 {
		return errors.New("catalog stream certificate pin is invalid")
	}
	if _, err := hex.DecodeString(value.CertificateSHA256); err != nil {
		return errors.New("catalog stream certificate pin is invalid")
	}
	if value.ExpiresAt.Before(now) || value.ExpiresAt.After(now.Add(bootstrapLifetime+time.Minute)) {
		return errors.New("catalog stream bootstrap expiration is invalid")
	}
	if value.MaximumFrameBytes != MaximumFrameSize {
		return errors.New("catalog stream frame limit is incompatible")
	}
	return nil
}

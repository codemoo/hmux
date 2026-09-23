package webgateway

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"syscall"

	"github.com/coder/websocket"
)

// Retain the first failure before socket shutdown/cancellation obscures it.
// Only fixed categories and numeric protocol codes may leave this recorder.
type homeConnectionFailure struct {
	stage      string
	cause      error
	httpStatus int
}

func (e *homeConnectionFailure) Error() string {
	text := "stage=" + e.stage + " reason=" + connectionErrorCategory(e.cause)
	if e.httpStatus >= 100 && e.httpStatus <= 599 {
		text += fmt.Sprintf(" http_status=%d", e.httpStatus)
	}
	if code := websocket.CloseStatus(e.cause); code >= 1000 && code <= 4999 {
		text += fmt.Sprintf(" close_code=%d", code)
	}
	return text
}
func (e *homeConnectionFailure) Unwrap() error { return e.cause }

type homeFailureRecorder struct {
	mu    sync.Mutex
	first *homeConnectionFailure
}

func (r *homeFailureRecorder) record(stage string, err error) {
	if err == nil {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.first == nil {
		r.first = &homeConnectionFailure{stage: stage, cause: err}
	}
}
func (r *homeFailureRecorder) result(fallback error) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.first != nil {
		return r.first
	}
	return fallback
}
func connectionErrorCategory(err error) string {
	if err == nil {
		return "unknown"
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "timeout"
	}
	if errors.Is(err, context.Canceled) {
		return "canceled"
	}
	if websocket.CloseStatus(err) != -1 {
		return "websocket-close"
	}
	var dns *net.DNSError
	if errors.As(err, &dns) {
		return "dns"
	}
	var cert *tls.CertificateVerificationError
	var unknown x509.UnknownAuthorityError
	var invalid x509.CertificateInvalidError
	var hostname x509.HostnameError
	if errors.As(err, &cert) || errors.As(err, &unknown) || errors.As(err, &invalid) || errors.As(err, &hostname) {
		return "tls-certificate"
	}
	if errors.Is(err, syscall.ECONNRESET) {
		return "connection-reset"
	}
	if errors.Is(err, syscall.ECONNREFUSED) {
		return "connection-refused"
	}
	if errors.Is(err, syscall.EPIPE) {
		return "broken-pipe"
	}
	if errors.Is(err, net.ErrClosed) {
		return "socket-closed"
	}
	if errors.Is(err, websocket.ErrMessageTooBig) {
		return "message-too-big"
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
		return "eof"
	}
	var network net.Error
	if errors.As(err, &network) && network.Timeout() {
		return "timeout"
	}
	var op *net.OpError
	if errors.As(err, &op) {
		return "network"
	}
	return "internal-or-protocol"
}
func homeConnectionSummary(err error) string {
	var failure *homeConnectionFailure
	if errors.As(err, &failure) {
		return failure.Error()
	}
	return "stage=connection reason=" + connectionErrorCategory(err)
}

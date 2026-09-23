package webgateway

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"net"
	"net/http"
	"net/url"
	"path/filepath"
	"strings"

	"github.com/coder/websocket"
)

type Server struct {
	usagePreferences *usagePreferenceStore
	diagnostics      *diagnosticStore
	push             *pushStore
	locations        *sessionLocator
	workspaceDir     string
	origin           string
	host             string
	token            string
	auth             *authStore
	hub              *hub
	uploads          *uploadLimiter
	assets           http.Handler
}

func NewServer(origin, credentialsPath, tokenPath string, assets fs.FS) (*Server, error) {
	u, err := url.Parse(origin)
	if err != nil || u.Scheme != "https" || u.Host == "" || u.Path != "" || u.RawQuery != "" || u.Fragment != "" || u.User != nil {
		return nil, errors.New("origin must be an exact HTTPS origin")
	}
	a, err := newAuth(credentialsPath)
	if err != nil {
		return nil, err
	}
	token, err := LoadToken(tokenPath)
	if err != nil {
		return nil, err
	}
	push, err := newPushStore(credentialsPath + ".push.json")
	if err != nil {
		return nil, err
	}
	s := &Server{push: push, locations: newSessionLocator(), workspaceDir: filepath.Join(filepath.Dir(credentialsPath), "web-profiles"), origin: origin, host: u.Host, token: token, auth: a, hub: newHub(), uploads: newUploadLimiter(), assets: http.FileServer(http.FS(assets))}
	s.diagnostics = newDiagnosticStore(credentialsPath + ".diagnostics.json")
	s.usagePreferences = newUsagePreferenceStore(credentialsPath + ".usage-preferences")
	s.hub.onCompletion = push.enqueue
	s.runPush()
	return s, nil
}

// SetTransportLog configures a bounded, privacy-filtered sink before serving.
func (s *Server) SetTransportLog(report func(string)) { s.hub.report = report }

func LoopbackAddress(address string) error {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return err
	}
	ip := net.ParseIP(host)
	if ip == nil || !ip.IsLoopback() {
		return errors.New("listen must be a literal loopback address behind HTTPS reverse proxy")
	}
	return nil
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.Header().Set("Referrer-Policy", "no-referrer")
	w.Header().Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
	w.Header().Set("Content-Security-Policy", "default-src 'none'; manifest-src 'self'; worker-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
	if r.Host != s.host {
		http.Error(w, "Invalid host", http.StatusMisdirectedRequest)
		return
	}
	if r.URL.Path == "/connect" {
		s.connector(w, r)
		return
	}
	if strings.HasPrefix(r.URL.Path, "/api/") {
		s.serveAPI(w, r)
		return
	}
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		http.Error(w, "Method not allowed", 405)
		return
	}
	s.assets.ServeHTTP(w, r)
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func decodeRequest(w http.ResponseWriter, r *http.Request, v any) error {
	if !strings.HasPrefix(r.Header.Get("Content-Type"), "application/json") {
		return errors.New("JSON required")
	}
	r.Body = http.MaxBytesReader(w, r.Body, 16<<10)
	d := json.NewDecoder(r.Body)
	d.DisallowUnknownFields()
	if err := d.Decode(v); err != nil {
		return err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return errors.New("trailing data")
	}
	return nil
}

func setCookie(w http.ResponseWriter, token string, maxAge int) {
	http.SetCookie(w, &http.Cookie{Name: cookieName, Value: token, Path: "/", HttpOnly: true, Secure: true, SameSite: http.SameSiteStrictMode, MaxAge: maxAge})
}

func (s *Server) connector(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet || r.Header.Get("Origin") != "" || subtle.ConstantTimeCompare([]byte(r.Header.Get("Authorization")), []byte("Bearer "+s.token)) != 1 {
		http.Error(w, "Forbidden", 403)
		return
	}
	conn, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	defer conn.CloseNow()
	conn.SetReadLimit(maxMessage)
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	p := observedPeer(&peer{conn: conn}, s.hub.report)
	go heartbeat(ctx, p, func(err error) { p.trace("heartbeat", err) })
	if !s.hub.serve(ctx, p) {
		p.trace("duplicate-home-rejected", nil)
		_ = conn.Close(websocket.StatusPolicyViolation, "Home already connected")
	}
}

func (s *Server) Close() {
	if s.diagnostics != nil {
		s.diagnostics.close()
	}
	if s.push != nil {
		s.push.close()
	}
	s.auth.closeConnections()
	s.hub.mu.Lock()
	p := s.hub.home
	s.hub.mu.Unlock()
	if p != nil {
		_ = p.conn.CloseNow()
	}
}

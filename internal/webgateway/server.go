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
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
	"github.com/coder/websocket"
)

type Server struct {
	locations    *sessionLocator
	workspaceDir string
	origin       string
	host         string
	token        string
	auth         *authStore
	hub          *hub
	uploads      *uploadLimiter
	assets       http.Handler
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
	return &Server{locations: newSessionLocator(), workspaceDir: filepath.Join(filepath.Dir(credentialsPath), "web-profiles"), origin: origin, host: u.Host, token: token, auth: a, hub: newHub(), uploads: newUploadLimiter(), assets: http.FileServer(http.FS(assets))}, nil
}
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
		if r.Method != http.MethodGet && r.Header.Get("Origin") != s.origin {
			http.Error(w, "Invalid origin", http.StatusForbidden)
			return
		}
		if r.URL.Path == "/api/login" {
			s.login(w, r)
			return
		}
		cookie, err := r.Cookie(cookieName)
		if err != nil {
			http.Error(w, "Sign in required", 401)
			return
		}
		csrf, done, ok := s.auth.get(cookie.Value, false)
		if !ok {
			http.Error(w, "Session expired", 401)
			return
		}
		if r.Method != http.MethodGet && subtle.ConstantTimeCompare([]byte(r.Header.Get("X-CSRF-Token")), []byte(csrf)) != 1 {
			http.Error(w, "Invalid request token", 403)
			return
		}
		switch r.URL.Path {
		case "/api/session":
			if r.Method != http.MethodGet {
				http.Error(w, "Method not allowed", 405)
				return
			}
			username, profile, _ := s.auth.identity(cookie.Value)
			writeJSON(w, map[string]any{"username": username, "profile": profile, "csrf": csrf})
		case "/api/logout":
			if r.Method != http.MethodPost {
				http.Error(w, "Method not allowed", 405)
				return
			}
			if err := s.auth.logout(cookie.Value); err != nil {
				http.Error(w, "Session storage unavailable", http.StatusServiceUnavailable)
				return
			}
			setCookie(w, "", -1)
			writeJSON(w, map[string]bool{"ok": true})
		case "/api/sessions":
			if r.Method != http.MethodGet {
				http.Error(w, "Method not allowed", 405)
				return
			}
			sessions, ok := s.auth.listSessions(cookie.Value)
			if !ok {
				http.Error(w, "Session expired", http.StatusUnauthorized)
				return
			}
			if s.locations != nil {
				s.locations.enrich(r.Context(), sessions)
			}
			if _, _, valid := s.auth.get(cookie.Value, false); !valid {
				http.Error(w, "Session expired", http.StatusUnauthorized)
				return
			}
			writeJSON(w, map[string]any{"sessions": sessions})
		case "/api/sessions/revoke":
			if r.Method != http.MethodPost {
				http.Error(w, "Method not allowed", 405)
				return
			}
			var req struct {
				ID string `json:"id"`
			}
			if decodeRequest(w, r, &req) != nil {
				http.Error(w, "Invalid session request", http.StatusBadRequest)
				return
			}
			current, err := s.auth.revoke(cookie.Value, req.ID)
			if errors.Is(err, errSessionNotFound) {
				http.Error(w, "Session not found", http.StatusNotFound)
				return
			}
			if err != nil {
				http.Error(w, "Session storage unavailable", http.StatusServiceUnavailable)
				return
			}
			if current {
				setCookie(w, "", -1)
			}
			writeJSON(w, map[string]bool{"ok": true})
		case "/api/account/security":
			s.accountSecurity(w, r, cookie.Value)
		case "/api/state":
			if r.Method != http.MethodGet {
				http.Error(w, "Method not allowed", 405)
				return
			}
			writeJSON(w, s.hub.snapshot())
		case "/api/action":
			s.action(w, r, cookie.Value)
		case "/api/terminal":
			s.terminal(w, r, cookie.Value, done)
		case "/api/upload":
			s.upload(w, r, cookie.Value, csrf, done)
		default:
			http.NotFound(w, r)
		}
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
func (s *Server) login(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", 405)
		return
	}
	var req struct {
		Username string `json:"username"`
		Password string `json:"password"`
		Code     string `json:"code"`
	}
	if decodeRequest(w, r, &req) != nil {
		http.Error(w, "Invalid login request", 400)
		return
	}
	token, status := s.auth.loginWithChallenge(req.Username, req.Password, req.Code, time.Now(), loginSource(r), loginIP(r), browserLabel(r.UserAgent()))
	if status == loginTOTPRequired {
		writeJSON(w, map[string]bool{"totp_required": true})
		return
	}
	if status != loginSucceeded {
		http.Error(w, "로그인 정보를 확인하거나 잠시 후 다시 시도하세요.", 401)
		return
	}
	setCookie(w, token, int(loginLifetime/time.Second))
	writeJSON(w, map[string]bool{"ok": true})
}

func (s *Server) accountSecurity(w http.ResponseWriter, r *http.Request, token string) {
	switch r.Method {
	case http.MethodGet:
		enabled, ok := s.auth.totpEnabled(token)
		if !ok {
			http.Error(w, "Session expired", http.StatusUnauthorized)
			return
		}
		writeJSON(w, map[string]bool{"totp_enabled": enabled})
	case http.MethodPost:
		var req struct {
			TOTPEnabled *bool  `json:"totp_enabled"`
			Password    string `json:"password"`
			Code        string `json:"code"`
		}
		if decodeRequest(w, r, &req) != nil || req.TOTPEnabled == nil {
			http.Error(w, "Invalid security request", http.StatusBadRequest)
			return
		}
		status := s.auth.setTOTPEnabled(token, req.Password, req.Code, *req.TOTPEnabled, time.Now())
		switch status {
		case accountSecurityOK:
			writeJSON(w, map[string]bool{"totp_enabled": *req.TOTPEnabled})
		case accountSecurityForbidden:
			http.Error(w, "비밀번호와 아직 사용하지 않은 인증 코드를 확인하세요.", http.StatusForbidden)
		case accountSecurityRateLimited:
			http.Error(w, "시도가 많습니다. 잠시 후 다시 시도하세요.", http.StatusTooManyRequests)
		case accountSecurityStorageUnavailable:
			http.Error(w, "보안 설정을 저장하지 못했습니다. 잠시 후 다시 시도하세요.", http.StatusServiceUnavailable)
		default:
			http.Error(w, "Session expired", http.StatusUnauthorized)
		}
	default:
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
	}
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
	p := &peer{conn: conn}
	go heartbeat(ctx, p)
	if !s.hub.serve(ctx, p) {
		_ = conn.Close(websocket.StatusPolicyViolation, "Home already connected")
	}
}
func (s *Server) action(w http.ResponseWriter, r *http.Request, token string) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", 405)
		return
	}
	var m Message
	if decodeRequest(w, r, &m) != nil {
		http.Error(w, "Invalid request", 400)
		return
	}
	switch m.Operation {
	case "profiles", "create", "alias", "hidden", "conversation", "workspace":
	default:
		http.Error(w, "Unknown operation", 400)
		return
	}
	// The browser cannot address another terminal or choose a transport request ID.
	m.Type = "request"
	m.ID = ""
	m.Data = nil
	m.Cols = 0
	m.Rows = 0
	m.Error = ""
	ctx, cancel := context.WithTimeout(r.Context(), 20*time.Second)
	defer cancel()
	_, done, ok := s.auth.get(token, false)
	if !ok {
		http.Error(w, "Session expired", 401)
		return
	}
	go func() {
		select {
		case <-done:
			cancel()
		case <-ctx.Done():
		}
	}()
	touch := true
	if m.Operation == "workspace" {
		var q struct {
			Change *sharedworkspace.Change `json:"change"`
		}
		if strictPayload(m.Payload, &q) != nil {
			http.Error(w, "Invalid workspace request", 400)
			return
		}
		touch = q.Change != nil
		_, profile, ok := s.auth.identity(token)
		if !ok {
			http.Error(w, "Session expired", 401)
			return
		}
		if profile != "" {
			if _, _, valid := s.auth.get(token, touch); !valid {
				http.Error(w, "Session expired", http.StatusUnauthorized)
				return
			}
			value, err := (sharedworkspace.Store{StateDir: filepath.Join(s.workspaceDir, profile)}).Sync(ctx, q.Change, func(context.Context) (model.Catalog, error) {
				snapshot := s.hub.snapshot()
				if snapshot["online"] != true {
					return model.Catalog{}, errors.New("Home offline")
				}
				raw, ok := snapshot["catalog"].(json.RawMessage)
				if !ok {
					return model.Catalog{}, errors.New("Home catalog unavailable")
				}
				var catalog model.Catalog
				err := json.Unmarshal(raw, &catalog)
				return catalog, err
			})
			if err != nil {
				http.Error(w, "Profile workspace unavailable", 502)
				return
			}
			writeJSON(w, value)
			return
		}

	}
	if _, _, valid := s.auth.get(token, touch); !valid {
		http.Error(w, "Session expired", http.StatusUnauthorized)
		return
	}
	result, err := s.hub.request(ctx, m)
	if err != nil {
		http.Error(w, "Home 요청을 완료하지 못했습니다. 연결과 세션 상태를 확인하세요.", 502)
		return
	}
	writeJSON(w, json.RawMessage(result))
}
func validSize(m Message) bool { return m.Cols >= 2 && m.Cols <= 500 && m.Rows >= 2 && m.Rows <= 250 }
func (s *Server) terminal(w http.ResponseWriter, r *http.Request, token string, done <-chan struct{}) {
	if r.Method != http.MethodGet || r.Header.Get("Origin") != s.origin {
		http.Error(w, "Forbidden", 403)
		return
	}
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{OriginPatterns: []string{s.host}})
	if err != nil {
		return
	}
	defer conn.CloseNow()
	conn.SetReadLimit(64 << 10)
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	go func() {
		select {
		case <-done:
			cancel()
		case <-ctx.Done():
		}
	}()
	first, stop := context.WithTimeout(ctx, 10*time.Second)
	_, raw, err := conn.Read(first)
	stop()
	var m Message
	if err != nil || json.Unmarshal(raw, &m) != nil || m.Type != "open" || model.ValidateSessionID(m.Session.ID) != nil || m.Session.CreatedAt < 1 || !validSize(m) {
		_ = conn.Close(websocket.StatusPolicyViolation, "Invalid terminal request")
		return
	}
	id := RandomToken()
	output := make(chan Message, 64)
	s.hub.mu.Lock()
	if len(s.hub.terminals) >= maxTerminals {
		s.hub.mu.Unlock()
		_ = conn.Close(websocket.StatusTryAgainLater, "Terminal limit reached")
		return
	}
	s.hub.terminals[id] = output
	s.hub.mu.Unlock()
	defer func() {
		s.hub.mu.Lock()
		delete(s.hub.terminals, id)
		s.hub.mu.Unlock()
		c, stop := context.WithTimeout(context.Background(), 2*time.Second)
		defer stop()
		_ = s.hub.send(c, Message{Type: "close", ID: id})
	}()
	openCtx, stop := context.WithTimeout(ctx, 20*time.Second)
	_, err = s.hub.request(openCtx, Message{Type: "open", ID: id, Session: m.Session, Cols: m.Cols, Rows: m.Rows})
	stop()
	if err != nil {
		_ = conn.Close(websocket.StatusPolicyViolation, "Terminal unavailable")
		return
	}
	if err = conn.Write(ctx, websocket.MessageText, []byte(`{"type":"ready"}`)); err != nil {
		return
	}
	go func() {
		defer cancel()
		for {
			kind, raw, err := conn.Read(ctx)
			if err != nil {
				return
			}
			if _, _, ok := s.auth.get(token, true); !ok {
				return
			}
			var frame Message
			if kind == websocket.MessageBinary {
				if len(raw) > 32<<10 {
					return
				}
				frame = Message{Type: "input", ID: id, Data: raw}
			} else {
				if json.Unmarshal(raw, &frame) != nil {
					return
				}
				switch frame.Type {
				case "resize":
					if !validSize(frame) {
						return
					}
					frame = Message{Type: "resize", ID: id, Cols: frame.Cols, Rows: frame.Rows}
				case "refresh":
					frame = Message{Type: "refresh", ID: id}
				default:
					return
				}
			}
			if s.hub.send(ctx, frame) != nil {
				return
			}
		}
	}()
	tick := time.NewTicker(5 * time.Second)
	defer tick.Stop()
	go heartbeat(ctx, &peer{conn: conn})
	for {
		select {
		case <-ctx.Done():
			return
		case <-done:
			return
		case <-tick.C:
			if _, _, ok := s.auth.get(token, false); !ok {
				return
			}
		case frame, ok := <-output:
			if !ok || frame.Type == "exit" {
				return
			}
			c, stop := context.WithTimeout(ctx, 5*time.Second)
			var err error
			if frame.Type == "refresh-result" {
				raw, _ := json.Marshal(struct {
					Type string `json:"type"`
					OK   bool   `json:"ok"`
				}{"refresh-result", frame.Error == ""})
				err = conn.Write(c, websocket.MessageText, raw)
			} else {
				err = conn.Write(c, websocket.MessageBinary, frame.Data)
			}
			stop()
			if err != nil {
				return
			}
		}
	}
}

func (s *Server) Close() {
	s.auth.closeConnections()
	s.hub.mu.Lock()
	p := s.hub.home
	s.hub.mu.Unlock()
	if p != nil {
		_ = p.conn.CloseNow()
	}
}

// Only the local reverse proxy may assert the client address. Nginx overwrites
// X-Real-IP; untrusted X-Forwarded-For chains are never used for authorization.
func loginIP(r *http.Request) string {
	host, _, _ := net.SplitHostPort(r.RemoteAddr)
	ip := net.ParseIP(host)
	if ip != nil && ip.IsLoopback() {
		if forwarded := net.ParseIP(r.Header.Get("X-Real-IP")); forwarded != nil {
			ip = forwarded
		}
	}
	if ip == nil {
		return "unknown"
	}
	if v4 := ip.To4(); v4 != nil {
		return v4.String()
	}
	return ip.String()
}

func loginSource(r *http.Request) string {
	ip := net.ParseIP(loginIP(r))
	if ip == nil {
		return "unknown"
	}
	if v4 := ip.To4(); v4 != nil {
		return v4.String()
	}
	return ip.Mask(net.CIDRMask(64, 128)).String()
}

func browserLabel(userAgent string) string {
	if len(userAgent) > 512 {
		userAgent = userAgent[:512]
	}
	osName := ""
	switch {
	case strings.Contains(userAgent, "Android"):
		osName = "Android"
	case strings.Contains(userAgent, "iPhone"), strings.Contains(userAgent, "iPad"), strings.Contains(userAgent, "iPod"):
		osName = "iOS"
	case strings.Contains(userAgent, "Windows"):
		osName = "Windows"
	case strings.Contains(userAgent, "Macintosh"), strings.Contains(userAgent, "Mac OS X"):
		osName = "macOS"
	case strings.Contains(userAgent, "CrOS"):
		osName = "ChromeOS"
	case strings.Contains(userAgent, "Linux"):
		osName = "Linux"
	}
	browser := "Unknown browser"
	switch {
	case strings.Contains(userAgent, "Edg/"):
		browser = "Edge"
	case strings.Contains(userAgent, "CriOS"), strings.Contains(userAgent, "Chrome/"):
		browser = "Chrome"
	case strings.Contains(userAgent, "FxiOS"), strings.Contains(userAgent, "Firefox/"):
		browser = "Firefox"
	case strings.Contains(userAgent, "Safari/"):
		browser = "Safari"
	}
	if osName != "" {
		return browser + " on " + osName
	}
	return browser
}

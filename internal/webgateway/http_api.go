package webgateway

import (
	"crypto/subtle"
	"errors"
	"net/http"
)

// serveAPI owns origin, login, session and CSRF gates before route dispatch.
func (s *Server) serveAPI(w http.ResponseWriter, r *http.Request) {
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
		login, _ := s.auth.pushLogin(cookie.Value)
		writeJSON(w, map[string]any{"username": username, "profile": profile, "csrf": csrf, "login_id": login.id})
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
	case "/api/account/usage":
		s.usagePreferencesAPI(w, r, cookie.Value)
	case "/api/push", "/api/push/subscribe", "/api/push/unsubscribe", "/api/push/presence", "/api/push/test":
		s.pushAPI(w, r, cookie.Value)
	case "/api/diagnostics":
		s.diagnosticsAPI(w, r, cookie.Value)
	case "/api/state":
		if r.Method != http.MethodGet {
			http.Error(w, "Method not allowed", 405)
			return
		}
		snapshot := s.hub.snapshot()
		login, valid := s.auth.pushLogin(cookie.Value)
		if !valid {
			http.Error(w, "Session expired", 401)
			return
		}
		if preferences, err := s.usagePreferences.get(login); err == nil {
			snapshot["usage_preferences"] = preferences
		}
		if _, valid := s.auth.pushLogin(cookie.Value); !valid {
			http.Error(w, "Session expired", 401)
			return
		}
		writeJSON(w, snapshot)
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

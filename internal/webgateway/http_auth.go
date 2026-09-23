package webgateway

import (
	"net"
	"net/http"
	"strings"
	"time"
)

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

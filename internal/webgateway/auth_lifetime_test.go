package webgateway

import (
	"net/http/httptest"
	"testing"
	"time"
)

func TestLoginSurvivesInactivityUntilSevenDayBoundary(t *testing.T) {
	s := testServer(t)
	now := time.Now()
	token, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, now), now, "192.0.2.1")
	if !ok {
		t.Fatal("login failed")
	}
	session := s.auth.sessions[sessionKey(token)]
	deadline := now.Add(7 * 24 * time.Hour)
	if !session.expires.Equal(deadline) {
		t.Fatal("incorrect expiry")
	}
	s.auth.prune(deadline.Add(-time.Nanosecond))
	if s.auth.sessions[sessionKey(token)] == nil {
		t.Fatal("inactive login expired early")
	}
	s.auth.prune(deadline)
	if s.auth.sessions[sessionKey(token)] != nil {
		t.Fatal("login survived deadline")
	}
	select {
	case <-session.done:
	default:
		t.Fatal("expired connections not revoked")
	}
}

func TestLoginCookieSevenDays(t *testing.T) {
	w := httptest.NewRecorder()
	setCookie(w, "test-token", int(loginLifetime/time.Second))
	cookie := w.Result().Cookies()[0]
	if cookie.MaxAge != 604800 || !cookie.Secure || !cookie.HttpOnly {
		t.Fatal("incorrect cookie lifetime or protection")
	}
}

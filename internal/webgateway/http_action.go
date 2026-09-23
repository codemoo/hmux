package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"path/filepath"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

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
	case "profiles", "create", "alias", "hidden", "conversation", "workspace",
		"providers", "provider-key", "provider-job-start", "provider-job", "provider-job-input", "provider-job-cancel":
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

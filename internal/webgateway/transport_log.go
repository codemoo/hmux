package webgateway

import (
	"context"
	"fmt"
	"sync/atomic"
	"time"

	"github.com/codemoo/hmux/internal/timing"
	"github.com/coder/websocket"
)

var transportSequence atomic.Uint64

// Only call with local constants, safe error categories and numeric counters.
// Never include caller operations, identities, payloads or error strings.
func (p *peer) trace(stage string, err error) {
	if p.report == nil {
		return
	}
	text := fmt.Sprintf("connection=%d stage=%s elapsed_ms=%d", p.connectionID, stage, time.Since(p.started).Milliseconds())
	if err != nil {
		text += " reason=" + connectionErrorCategory(err)
		if code := websocket.CloseStatus(err); code >= 1000 && code <= 4999 {
			text += fmt.Sprintf(" close_code=%d", code)
		}
	}
	p.report(text)
}
func observedPeer(p *peer, report func(string)) *peer {
	p.connectionID = transportSequence.Add(1)
	p.started = time.Now()
	p.report = report
	return p
}

// Only allowlisted protocol verbs enter logs, never user-provided strings.
func logOperation(m Message) string {
	if m.Type == "open" {
		return "terminal-open"
	}
	switch m.Operation {
	case "workspace", "profiles", "providers", "provider-key", "provider-job-start", "provider-job", "provider-job-input", "provider-job-cancel", "create", "conversation", "alias", "hidden":
		return m.Operation
	default:
		return "unknown"
	}
}
func (p *peer) timingContext(ctx context.Context, operation string) context.Context {
	if p.report == nil {
		return ctx
	}
	return timing.WithReporter(ctx, func(stage string, elapsed time.Duration) {
		p.report(fmt.Sprintf("connection=%d operation=%s stage=%s duration_ms=%d", p.connectionID, operation, stage, elapsed.Milliseconds()))
	})
}

package webgateway

import (
	"context"
	"encoding/json"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

// Notification discovery is best effort and never blocks catalog publication or
// cancels the connector. Keep just the newest pending snapshot, with one worker
// owning the tracker so slow filesystem reads cannot spawn unbounded work.
type completionWorker struct {
	pending chan model.Catalog
	observe func(context.Context, model.Catalog) ([]catalog.TaskCompletion, error)
	send    func(context.Context, Message) error
}

func newCompletionWorker(observe func(context.Context, model.Catalog) ([]catalog.TaskCompletion, error), send func(context.Context, Message) error) *completionWorker {
	return &completionWorker{pending: make(chan model.Catalog, 1), observe: observe, send: send}
}

// Called by the single catalog producer before semantic digest suppression.
func (w *completionWorker) enqueue(ctx context.Context, value model.Catalog) error {
	if ctx.Err() != nil {
		return nil
	}
	// Only identities and pane PIDs are read by the tracker. Detach the slice
	// from the collector before another goroutine observes it.
	value.Sessions = append([]model.Session(nil), value.Sessions...)
	select {
	case w.pending <- value:
	default:
		select {
		case <-w.pending:
		default:
		}
		select {
		case w.pending <- value:
		default:
		}
	}
	return nil
}

func (w *completionWorker) run(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case value := <-w.pending:
			bounded, stop := context.WithTimeout(ctx, 3*time.Second)
			events, err := w.observe(bounded, value)
			overBudget := bounded.Err() != nil
			if overBudget {
				err = bounded.Err()
			}
			stop()
			if err != nil {
				if !overBudget {
					continue
				}
				// An over-budget scan must not immediately consume another queued
				// snapshot and keep the Home busy continuously.
				timer := time.NewTimer(5 * time.Second)
				select {
				case <-ctx.Done():
					timer.Stop()
					return
				case <-timer.C:
				}
				continue
			}
			for _, event := range events {
				if ctx.Err() != nil {
					return
				}
				payload, _ := json.Marshal(struct {
					CompletedAt time.Time `json:"completed_at"`
				}{event.CompletedAt})
				// Use the worker lifetime, not an already-finished catalog fetch's
				// deadline. peer.send bounds both queueing and writing.
				if w.send(ctx, Message{Type: "task-complete", ID: event.EventID, Session: event.Session, Payload: payload}) != nil {
					break
				}
			}
		}
	}
}

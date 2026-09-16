// Package sse broadcasts UsageSnapshot frames to subscribed HTTP clients
// over server-sent events.
package sse

import (
	"context"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// Hub is a per-provider SSE broadcaster. Frames published into it fan out
// to every subscriber's bounded buffer; slow subscribers drop oldest events
// rather than blocking the broadcaster.
type Hub struct {
	mu                sync.Mutex
	wg                sync.WaitGroup
	clients           map[int]*client
	latestSnapshot    *wire.SSEEvent
	latestSeq         int
	hasLatestSeq      bool
	encodeFailures    uint64
	heartbeatInterval time.Duration
	closed            bool
	nextID            int
}

// client owns one subscriber's send channel. Each client carries its own
// mutex so the multiple-producer / single-consumer model (broadcaster +
// heartbeat goroutine → client.out, SSE handler ← client.out) avoids the
// classic "send on closed channel" panic without serializing producers
// across clients.
type client struct {
	mu     sync.Mutex
	out    chan wire.SSEEvent
	cancel context.CancelFunc
	closed bool // protected by mu; mu also guards close(out) so `send` and
	// `close` are mutually exclusive on this client.
}

// send is best-effort: drops oldest then re-enqueues if the buffer is full
// (matches Swift's bufferingNewest(1) policy). Returns silently when the
// client is already closed.
func (c *client) send(event wire.SSEEvent) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return
	}
	select {
	case c.out <- event:
	default:
		select {
		case <-c.out:
		default:
		}
		select {
		case c.out <- event:
		default:
		}
	}
}

// sendHeartbeat never evicts a state-bearing event. A heartbeat is useful
// only while the queue is otherwise empty; snapshots always take priority.
func (c *client) sendHeartbeat(event wire.SSEEvent) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return
	}
	select {
	case c.out <- event:
	default:
	}
}

// shutdown closes the client's send channel and cancels its heartbeat ctx.
// Idempotent — safe to call from both unsubscribe and Hub.Close.
func (c *client) shutdown() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return
	}
	c.closed = true
	if c.cancel != nil {
		c.cancel()
	}
	close(c.out)
}

// NewHub builds a Hub with a 10s heartbeat (matching the Swift default).
func NewHub() *Hub {
	return &Hub{
		clients:           map[int]*client{},
		heartbeatInterval: 10 * time.Second,
	}
}

// Subscribe registers a new client and returns a channel of SSE events plus
// an unsubscribe func. The most recent snapshot (if any) is delivered first
// so freshly connected clients see state immediately. The supplied context
// cancels heartbeats and removes the client when the HTTP request goes
// away.
func (h *Hub) Subscribe(ctx context.Context) (<-chan wire.SSEEvent, func()) {
	out := make(chan wire.SSEEvent, 1)
	subCtx, cancel := context.WithCancel(ctx)

	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		cancel()
		close(out)
		return out, func() {}
	}
	id := h.nextID
	h.nextID++
	c := &client{out: out, cancel: cancel}
	h.clients[id] = c
	cached := h.latestSnapshot
	// Add while the admission lock is held. Close takes the same lock before
	// setting closed, so Wait after Close can never race a later Add.
	h.wg.Add(2)
	h.mu.Unlock()

	// Deliver the cached latest snapshot if one exists.
	if cached != nil {
		c.send(*cached)
	}

	// Per-client heartbeat keeps idle SSE connections from being culled
	// by intermediate proxies that drop quiet TCP streams.
	go func() {
		defer h.wg.Done()
		t := time.NewTicker(h.heartbeatInterval)
		defer t.Stop()
		for {
			select {
			case <-subCtx.Done():
				return
			case <-t.C:
				c.sendHeartbeat(wire.HeartbeatEvent())
			}
		}
	}()

	// Drop the client when its ctx fires (HTTP disconnect or Hub close).
	go func() {
		defer h.wg.Done()
		<-subCtx.Done()
		h.mu.Lock()
		if h.clients[id] == c {
			delete(h.clients, id)
		}
		h.mu.Unlock()
		c.shutdown()
	}()

	return out, cancel
}

// PublishSnapshot stores the snapshot as the latest and fans out to clients.
func (h *Hub) PublishSnapshot(snapshot wire.UsageSnapshot) error {
	event, err := wire.SnapshotEvent(snapshot)
	if err != nil {
		h.mu.Lock()
		h.encodeFailures++
		h.mu.Unlock()
		return err
	}
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil
	}
	// Concurrent HTTP/SSE callers may receive the same coalesced refresh
	// result. Do not publish duplicate or superseded sequence numbers: an SSE
	// stream must never move backward or repeat a state frame.
	if h.hasLatestSeq && snapshot.Seq <= h.latestSeq {
		h.mu.Unlock()
		return nil
	}
	h.latestSnapshot = &event
	h.latestSeq = snapshot.Seq
	h.hasLatestSeq = true
	clients := make([]*client, 0, len(h.clients))
	for _, c := range h.clients {
		clients = append(clients, c)
	}
	h.mu.Unlock()
	for _, c := range clients {
		c.send(event)
	}
	return nil
}

// EncodeFailures returns the number of snapshots rejected before broadcast
// because their wire JSON could not be encoded.
func (h *Hub) EncodeFailures() uint64 {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.encodeFailures
}

// ClientCount returns the live subscriber count (debug + tests).
func (h *Hub) ClientCount() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	return len(h.clients)
}

// Close terminates every client and stops accepting new ones.
func (h *Hub) Close() {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return
	}
	h.closed = true
	clients := make([]*client, 0, len(h.clients))
	for _, c := range h.clients {
		clients = append(clients, c)
	}
	h.clients = map[int]*client{}
	h.mu.Unlock()
	for _, c := range clients {
		c.shutdown()
	}
}

// Wait blocks until all admitted subscriber heartbeat and cleanup goroutines
// have exited. Call Close first so no new subscribers can be admitted.
func (h *Hub) Wait() {
	h.wg.Wait()
}

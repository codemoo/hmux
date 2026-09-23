package webgateway

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
)

type pausedWriteConn struct {
	net.Conn
	armed   atomic.Bool
	entered chan struct{}
	release chan struct{}
	once    sync.Once
}

func (c *pausedWriteConn) Write(p []byte) (int, error) {
	if c.armed.Load() {
		c.once.Do(func() { close(c.entered) })
		<-c.release
	}
	return c.Conn.Write(p)
}

type pausedWriteListener struct {
	net.Listener
	accepted chan *pausedWriteConn
}

func (l *pausedWriteListener) Accept() (net.Conn, error) {
	c, e := l.Listener.Accept()
	if e != nil {
		return nil, e
	}
	wrapped := &pausedWriteConn{Conn: c, entered: make(chan struct{}), release: make(chan struct{})}
	l.accepted <- wrapped
	return wrapped, nil
}

// A browser can cancel an HTTP request after the gateway starts forwarding its
// frame. That cancellation must not tear down the shared Home transport.
func TestSharedHomeWriteSurvivesCallerCancellation(t *testing.T) {
	for _, deadline := range []bool{false, true} {
		name := "cancel"
		if deadline {
			name = "deadline"
		}
		t.Run(name, func(t *testing.T) { testSharedHomeWriteSurvivesCancellation(t, deadline, false) })
	}
}
func TestQueuedHomeWriteGetsIndependentTransportBudget(t *testing.T) {
	testSharedHomeWriteSurvivesCancellation(t, false, true)
}
func testSharedHomeWriteSurvivesCancellation(t *testing.T, deadline bool, queued bool) {
	peers := make(chan *peer, 1)
	done := make(chan struct{})
	srv := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, e := websocket.Accept(w, r, nil)
		if e != nil {
			return
		}
		defer c.CloseNow()
		peers <- &peer{conn: c}
		<-done
	}))
	listener := &pausedWriteListener{Listener: srv.Listener, accepted: make(chan *pausedWriteConn, 1)}
	srv.Listener = listener
	srv.Start()
	defer srv.Close()
	defer close(done)
	ctx, stop := context.WithTimeout(context.Background(), 10*time.Second)
	defer stop()
	client, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer client.CloseNow()
	transport := <-listener.accepted
	var releaseOnce sync.Once
	release := func() { releaseOnce.Do(func() { close(transport.release) }) }
	defer release()
	p := <-peers
	transport.armed.Store(true)
	caller, cancel := context.WithCancel(ctx)
	if deadline {
		cancel()
		caller, cancel = context.WithTimeout(ctx, 250*time.Millisecond)
	}
	defer cancel()
	if queued {
		if err := p.acquireWriter(ctx); err != nil {
			t.Fatal(err)
		}
	}
	sent := make(chan error, 1)
	go func() { sent <- p.send(caller, Message{Type: "request", ID: "test"}) }()
	if queued {
		time.Sleep(4500 * time.Millisecond)
		<-p.writeLock
	}
	select {
	case <-transport.entered:
	case <-ctx.Done():
		t.Fatal("write never started")
	}
	if deadline {
		<-caller.Done()
	} else {
		cancel()
	}
	if queued {
		time.Sleep(750 * time.Millisecond)
	}
	// Let cancellation propagate while the transport write is in progress.
	time.Sleep(25 * time.Millisecond)
	release()
	if err := <-sent; err != nil {
		t.Fatalf("caller cancellation broke shared write: %v", err)
	}
	if _, _, err := client.Read(ctx); err != nil {
		t.Fatalf("Home lost first frame: %v", err)
	}
	if err := p.send(ctx, Message{Type: "request", ID: "next"}); err != nil {
		t.Fatalf("shared transport unusable: %v", err)
	}
	if _, _, err := client.Read(ctx); err != nil {
		t.Fatalf("Home lost subsequent frame: %v", err)
	}
}

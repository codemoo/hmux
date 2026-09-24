package webgateway

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/model"
)

// TestRustHomePeerGoGateway is an opt-in, process-level oracle for the Rust
// Home peer. It uses the actual Go connector and hub, with no browser account,
// provider, tmux, or persistent-state owner.
func TestRustHomePeerGoGateway(t *testing.T) {
	if os.Getenv("HMUX_RUST_HOME_PEER_ORACLE") != "1" {
		t.Skip("set HMUX_RUST_HOME_PEER_ORACLE=1 for the isolated Rust Home peer test")
	}
	const token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	h := newHub()
	s := &Server{token: token, hub: h}
	connectorDone := make(chan struct{}, 1)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Host != "hmux.example" || r.URL.Path != "/connect" {
			http.Error(w, "Invalid host or path", http.StatusMisdirectedRequest)
			return
		}
		if r.Header.Get("Sec-WebSocket-Protocol") != "hmux-home.pb.v2.controls1" {
			http.Error(w, "candidate must offer v2", http.StatusBadRequest)
			return
		}
		s.connector(w, r)
		select {
		case connectorDone <- struct{}{}:
		default:
		}
	})}
	serveDone := make(chan error, 1)
	go func() { serveDone <- server.Serve(listener) }()
	defer func() {
		h.mu.Lock()
		p := h.home
		h.mu.Unlock()
		if p != nil {
			_ = p.conn.CloseNow()
		}
		_ = server.Close()
		select {
		case err := <-serveDone:
			if !errors.Is(err, http.ErrServerClosed) {
				t.Errorf("Go gateway listener: %v", err)
			}
		case <-time.After(time.Second):
			t.Error("Go gateway listener failed to stop")
		}
	}()
	fmt.Fprintln(os.Stdout, "HMUX_GO_HOME_READY", listener.Addr().String())

	wait := func(label string, condition func() bool) {
		t.Helper()
		tick := time.NewTicker(5 * time.Millisecond)
		defer tick.Stop()
		for {
			if condition() {
				return
			}
			select {
			case <-tick.C:
			case <-ctx.Done():
				t.Fatalf("%s: %v", label, ctx.Err())
			}
		}
	}
	wait("Rust Home catalog did not arrive", func() bool {
		h.mu.Lock()
		defer h.mu.Unlock()
		return h.home != nil && h.updated.IsZero() == false && len(h.catalog) > 0
	})
	h.mu.Lock()
	catalogRaw := append(json.RawMessage(nil), h.catalog...)
	uploadCap, outputCap := h.uploadCap, h.outputCap
	h.mu.Unlock()
	if !uploadCap || !outputCap {
		t.Fatalf("unexpected Home capabilities: upload=%t terminal=%t", uploadCap, outputCap)
	}
	if online, _ := h.snapshot()["online"].(bool); !online {
		t.Fatal("catalog did not make Home online")
	}
	var catalog model.Catalog
	if err := json.Unmarshal(catalogRaw, &catalog); err != nil {
		t.Fatal(err)
	}
	if len(catalog.Sessions) != 1 {
		t.Fatalf("catalog session count = %d", len(catalog.Sessions))
	}
	got := catalog.Sessions[0]
	if got.ID != "$7" || got.CreatedAt != 1700000000 || got.Name != "agent" || got.CurrentPath != "/synthetic/work" || got.Width != 80 || got.Height != 24 {
		t.Fatalf("wrong synthetic catalog session: %+v", got)
	}
	request := func(m Message) (json.RawMessage, error) {
		t.Helper()
		bounded, stop := context.WithTimeout(ctx, 5*time.Second)
		defer stop()
		return h.request(bounded, m)
	}
	wantProfiles := []map[string]string{{"id": "shell", "label": "Shell"}, {"id": "codex", "label": "Codex"}}
	checkProfiles := func() {
		t.Helper()
		raw, err := request(Message{Type: "request", Operation: "profiles"})
		if err != nil {
			t.Fatalf("profiles request: %v", err)
		}
		var profiles []map[string]string
		if err := json.Unmarshal(raw, &profiles); err != nil {
			t.Fatalf("profiles response: %v", err)
		}
		if !reflect.DeepEqual(profiles, wantProfiles) {
			t.Fatalf("profiles = %s; want %v", raw, wantProfiles)
		}
	}
	checkProfiles()
	for _, m := range []Message{
		{Type: "request", Operation: "workspace", Payload: json.RawMessage(`{}`)},
	} {
		if _, err := request(m); err == nil || err.Error() != "Home operation unavailable" {
			t.Fatalf("%s error = %v; want Home operation unavailable", m.Operation+m.Type, err)
		}
	}
	// Exercise the same hub protocol as the browser upload bridge. The first
	// chunk crosses the file boundary; Go validates the response and both hashes.
	files := [][]byte{{0, 0xff, 'A'}, {0, 'B', 0x80, '\n'}}
	header, err := browserUploadHeader(browserUploadStart{
		Session: filestage.SessionIdentity{ID: "$7", CreatedAt: 1700000000},
		Files:   []browserUploadFile{{Size: int64(len(files[0])), Extension: "bin"}, {Size: int64(len(files[1])), Extension: "dat"}},
	})
	if err != nil {
		t.Fatalf("Go upload metadata: %v", err)
	}
	stageRoot := filepath.Join(os.Getenv("TMPDIR"), "hmux", "staged-files-v1")
	lock, err := os.OpenFile(filepath.Join(stageRoot, ".lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer lock.Close()
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		t.Fatal(err)
	}
	upload, err := h.openUpload(header)
	if err != nil {
		t.Fatalf("Go hub upload admission: %v", err)
	}
	defer h.closeUpload(header.RequestID, upload)
	if err := h.sendUpload(ctx, upload, Message{Type: "upload-start", ID: header.RequestID, Header: &header}); err != nil {
		t.Fatalf("Go hub upload start: %v", err)
	}
	if event, err := waitUploadEvent(ctx, upload, 100*time.Millisecond); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Rust staging bypassed Go-held flock: type=%s err=%v", event.Type, err)
	}
	checkProfiles() // An occupied spool must not stall the shared peer reader.
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_UN); err != nil {
		t.Fatal(err)
	}
	event, err := waitUploadEvent(ctx, upload, 5*time.Second)
	if err != nil || event.Type != "upload-ready" || event.ID != header.RequestID {
		t.Fatalf("Rust Home upload ready: type=%q id=%q err=%v", event.Type, event.ID, err)
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); !errors.Is(err, syscall.EWOULDBLOCK) && !errors.Is(err, syscall.EAGAIN) {
		t.Fatalf("Rust ready upload does not own shared flock: %v", err)
	}
	contents := append(append([]byte(nil), files[0]...), files[1]...)
	var received int64
	for _, chunk := range [][]byte{contents[:4], contents[4:]} {
		if err := h.sendUpload(ctx, upload, Message{Type: "upload-data", ID: header.RequestID, Data: chunk}); err != nil {
			t.Fatalf("Go hub upload data: %v", err)
		}
		received += int64(len(chunk))
		event, err = waitUploadEvent(ctx, upload, 5*time.Second)
		if err != nil || event.Type != "upload-ack" || event.ID != header.RequestID || event.Received != received {
			t.Fatalf("Rust Home upload ACK: type=%q id=%q received=%d want=%d err=%v", event.Type, event.ID, event.Received, received, err)
		}
	}
	if err := h.sendUpload(ctx, upload, Message{Type: "upload-finish", ID: header.RequestID}); err != nil {
		t.Fatalf("Go hub upload finish: %v", err)
	}
	event, err = waitUploadEvent(ctx, upload, 5*time.Second)
	if err != nil || event.Type != "upload-complete" || event.ID != header.RequestID {
		t.Fatalf("Rust Home upload complete: type=%q id=%q err=%v", event.Type, event.ID, err)
	}
	response, err := filestage.DecodeResponse(event.Payload)
	if err != nil {
		t.Fatalf("Go upload response parser: %v", err)
	}
	hashes := make([]string, len(files))
	for index, content := range files {
		sum := sha256.Sum256(content)
		hashes[index] = hex.EncodeToString(sum[:])
	}
	if err := filestage.ValidateResponseForHeader(response, header, hashes); err != nil {
		t.Fatalf("Go upload response validator: %v", err)
	}
	if response.ExpiresAtUnix < time.Now().Add(3*time.Hour-time.Minute).Unix() || response.ExpiresAtUnix > time.Now().Add(3*time.Hour+time.Minute).Unix() {
		t.Fatalf("Rust Home upload expiry = %d", response.ExpiresAtUnix)
	}
	for index, staged := range response.Files {
		if filepath.Dir(filepath.Dir(staged.Path)) != stageRoot {
			t.Fatalf("staged file %d escaped isolated root", index)
		}
		content, err := os.ReadFile(staged.Path)
		if err != nil || !bytes.Equal(content, files[index]) {
			t.Fatalf("staged file %d bytes: error=%v", index, err)
		}
		info, err := os.Stat(staged.Path)
		if err != nil || info.Mode().Perm() != 0o600 {
			t.Fatalf("staged file %d mode: error=%v", index, err)
		}
	}
	wait("Rust completed upload retained spool lock", func() bool {
		err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if errors.Is(err, syscall.EWOULDBLOCK) || errors.Is(err, syscall.EAGAIN) {
			return false
		}
		if err != nil {
			t.Fatal(err)
		}
		return true
	})
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_UN); err != nil {
		t.Fatal(err)
	}
	h.closeUpload(header.RequestID, upload)
	// Real Rust PTY owner, with synthetic tmux commands and an owned shell.
	id := "rust-terminal"
	output := &terminalOutput{frames: make(chan Message, 64)}
	h.mu.Lock()
	h.terminals[id] = output
	h.mu.Unlock()
	if raw, err := request(Message{Type: "open", ID: id, Session: model.SessionIdentity{ID: "$7", CreatedAt: 1700000000}, Cols: 80, Rows: 24, Capabilities: []string{terminalFlowCapability}}); err != nil || string(raw) != `{"ok":true}` {
		t.Fatalf("Rust terminal open: payload=%s error=%v", raw, err)
	}
	receive := func() Message {
		t.Helper()
		select {
		case value, ok := <-output.frames:
			if !ok {
				t.Fatal("terminal stream closed unexpectedly")
			}
			return value
		case <-ctx.Done():
			t.Fatal("terminal output deadline")
			return Message{}
		}
	}
	text := func(marker string) {
		t.Helper()
		var data strings.Builder
		for {
			frame := receive()
			if frame.Type != "data" || len(frame.Data) > terminalOutputChunk {
				t.Fatalf("terminal frame: %s", frame.Type)
			}
			data.Write(frame.Data)
			if err := h.send(ctx, Message{Type: "output-ack", ID: id, Received: int64(len(frame.Data))}); err != nil {
				t.Fatal(err)
			}
			if strings.Contains(data.String(), marker) {
				return
			}
			if data.Len() > 64<<10 {
				t.Fatal("synthetic output limit")
			}
		}
	}
	text("RUST_READY")
	if err := h.send(ctx, Message{Type: "input", ID: id, Data: []byte("synthetic 한글\n")}); err != nil {
		t.Fatal(err)
	}
	text("RUST_INPUT:synthetic 한글")
	if err := h.send(ctx, Message{Type: "resize", ID: id, Cols: 100, Rows: 40}); err != nil {
		t.Fatal(err)
	}
	if err := h.send(ctx, Message{Type: "input", ID: id, Data: []byte("SIZE\n")}); err != nil {
		t.Fatal(err)
	}
	text("40 100")
	if err := h.send(ctx, Message{Type: "refresh", ID: id}); err != nil {
		t.Fatal(err)
	}
	if frame := receive(); frame.Type != "refresh-result" || frame.Error != "Terminal refresh unavailable" {
		t.Fatalf("refresh result: %s %s", frame.Type, frame.Error)
	}
	if err := h.send(ctx, Message{Type: "close", ID: id}); err != nil {
		t.Fatal(err)
	}
	if frame := receive(); frame.Type != "exit" {
		t.Fatalf("terminal close: %s", frame.Type)
	}
	h.mu.Lock()
	delete(h.terminals, id)
	h.mu.Unlock()
	checkProfiles()
	fmt.Fprintln(os.Stdout, "HMUX_GO_HOME_CHECKED")
	select {
	case <-connectorDone:
	case <-ctx.Done():
		t.Fatalf("Rust Home did not disconnect: %v", ctx.Err())
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.home != nil || h.catalog != nil || !h.updated.IsZero() || len(h.pending) != 0 || len(h.terminals) != 0 || len(h.uploads) != 0 || h.uploadCap || h.outputCap {
		t.Fatalf("Go hub retained Rust Home state: home=%t catalog=%d pending=%d views=%d uploads=%d", h.home != nil, len(h.catalog), len(h.pending), len(h.terminals), len(h.uploads))
	}
}

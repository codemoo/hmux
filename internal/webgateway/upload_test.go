package webgateway

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/coder/websocket"
)

func uploadSocketServer(t *testing.T) (*Server, *httptest.Server, string, string) {
	t.Helper()
	s := testServer(t)
	ts := httptest.NewServer(s)
	t.Cleanup(ts.Close)
	s.origin = ts.URL
	s.host = strings.TrimPrefix(ts.URL, "http://")
	token := loginForTest(t, s)
	csrf, _, ok := s.auth.get(token, false)
	if !ok {
		t.Fatal("login unavailable")
	}
	return s, ts, token, csrf
}

func dialUpload(t *testing.T, ts *httptest.Server, origin, token string) *websocket.Conn {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, response, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(ts.URL, "http")+"/api/upload", &websocket.DialOptions{
		HTTPHeader: http.Header{"Cookie": {cookieName + "=" + token}, "Origin": {origin}},
	})
	if err != nil {
		if response != nil {
			t.Fatalf("dial upload: status=%d err=%v", response.StatusCode, err)
		}
		t.Fatal(err)
	}
	return conn
}

func connectFakeUploadHome(t *testing.T, s *Server, ts *httptest.Server) (*peer, *websocket.Conn) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(ts.URL, "http")+"/connect", &websocket.DialOptions{
		HTTPHeader: http.Header{"Authorization": {"Bearer " + s.token}},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.CloseNow() })
	p := &peer{conn: conn}
	if err := p.send(context.Background(), Message{Type: "hello", Capabilities: []string{"web-upload-v1"}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for {
		s.hub.mu.Lock()
		ready := s.hub.uploadCap
		s.hub.mu.Unlock()
		if ready {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("gateway did not accept upload capability")
		}
		time.Sleep(time.Millisecond)
	}
	return p, conn
}

func writeUploadText(t *testing.T, conn *websocket.Conn, value any) {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := conn.Write(ctx, websocket.MessageText, raw); err != nil {
		t.Fatal(err)
	}
}

func readUploadJSON(t *testing.T, conn *websocket.Conn, value any) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	kind, raw, err := conn.Read(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if kind != websocket.MessageText || json.Unmarshal(raw, value) != nil {
		t.Fatalf("invalid upload response %q", raw)
	}
}

func TestUploadWebSocketAuthenticationOriginCSRFAndLimits(t *testing.T) {
	s, ts, token, csrf := uploadSocketServer(t)
	endpoint := "ws" + strings.TrimPrefix(ts.URL, "http") + "/api/upload"
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for name, headers := range map[string]http.Header{
		"missing cookie": {"Origin": {s.origin}},
		"wrong origin":   {"Origin": {"https://invalid.example"}, "Cookie": {cookieName + "=" + token}},
	} {
		t.Run(name, func(t *testing.T) {
			conn, response, err := websocket.Dial(ctx, endpoint, &websocket.DialOptions{HTTPHeader: headers})
			if err == nil {
				conn.CloseNow()
				t.Fatal("unauthorized socket accepted")
			}
			if response == nil || response.StatusCode != map[string]int{"missing cookie": 401, "wrong origin": 403}[name] {
				t.Fatalf("response=%v err=%v", response, err)
			}
		})
	}

	for name, start := range map[string]any{
		"wrong csrf":    map[string]any{"type": "start", "csrf": "wrong", "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 1, "extension": "txt"}}},
		"unknown field": map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 1, "extension": "txt"}}, "name": "secret.txt"},
		"empty file":    map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 0, "extension": "txt"}}},
		"large file":    map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": filestage.MaximumFileBytes + 1, "extension": "txt"}}},
		"bad extension": map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 1, "extension": "../txt"}}},
	} {
		t.Run(name, func(t *testing.T) {
			conn := dialUpload(t, ts, s.origin, token)
			defer conn.CloseNow()
			writeUploadText(t, conn, start)
			var response map[string]any
			readUploadJSON(t, conn, &response)
			if response["type"] != "error" || response["error"] != "Invalid upload request" {
				t.Fatalf("response=%v", response)
			}
		})
	}
}

func TestUploadRoundTripPreservesChunkedBinaryAndRejectsChangedResponse(t *testing.T) {
	for _, changed := range []bool{false, true} {
		t.Run(fmt.Sprintf("changed=%t", changed), func(t *testing.T) {
			s, ts, token, csrf := uploadSocketServer(t)
			home, _ := connectFakeUploadHome(t, s, ts)
			bodySeen := make(chan []byte, 1)
			go fakeUploadReceiver(context.Background(), home, changed, bodySeen, nil)
			browser := dialUpload(t, ts, s.origin, token)
			defer browser.CloseNow()
			writeUploadText(t, browser, map[string]any{
				"type": "start", "csrf": csrf, "session": map[string]any{"id": "$7", "created_at": 42},
				"files": []any{map[string]any{"size": 3, "extension": "txt"}, map[string]any{"size": 4, "extension": "bin"}},
			})
			var message map[string]any
			readUploadJSON(t, browser, &message)
			if message["type"] != "ready" {
				t.Fatal(message)
			}
			for _, chunk := range [][]byte{{0, 1, 2, 3}, {4, 0xff, 6}} {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				if err := browser.Write(ctx, websocket.MessageBinary, chunk); err != nil {
					cancel()
					t.Fatal(err)
				}
				cancel()
				readUploadJSON(t, browser, &message)
				if message["type"] != "ack" {
					t.Fatal(message)
				}
			}
			writeUploadText(t, browser, map[string]string{"type": "finish"})
			readUploadJSON(t, browser, &message)
			if changed {
				if message["type"] != "error" || message["error"] != "Home upload unavailable" {
					t.Fatalf("changed response accepted: %v", message)
				}
			} else if message["type"] != "complete" {
				t.Fatalf("upload did not complete: %v", message)
			}
			if got := <-bodySeen; !bytes.Equal(got, []byte{0, 1, 2, 3, 4, 0xff, 6}) {
				t.Fatalf("binary changed: %x", got)
			}
		})
	}
}

func TestUploadActualHomeReceiverCommitsBytesHashesAndThreeHourExpiry(t *testing.T) {
	root := filepath.Join(t.TempDir(), "hmux", "staged-files-v1")
	originalRoot, originalVerify := homeFileStageRoot, homeFileStageVerify
	homeFileStageRoot = func() (string, error) { return root, nil }
	identity := filestage.SessionIdentity{ID: "$7", CreatedAt: 42}
	var verifyCalls atomic.Int32
	homeFileStageVerify = func(_ context.Context, got filestage.SessionIdentity) error {
		verifyCalls.Add(1)
		if got != identity {
			return fmt.Errorf("unexpected identity: %+v", got)
		}
		return nil
	}

	s, ts, token, csrf := uploadSocketServer(t)
	home, connection := connectFakeUploadHome(t, s, ts)
	homeCtx, cancelHome := context.WithCancel(context.Background())
	bridgeDone, workerDone := serveActualUploadHome(homeCtx, home)
	defer func() {
		cancelHome()
		_ = connection.CloseNow()
		select {
		case <-bridgeDone:
		case <-time.After(3 * time.Second):
			t.Error("actual Home bridge did not stop")
		}
		homeFileStageRoot, homeFileStageVerify = originalRoot, originalVerify
	}()

	first := []byte{0, 1, 2}
	second := []byte{3, 4, 0xff, 6}
	started := time.Now()
	browser := dialUpload(t, ts, s.origin, token)
	defer browser.CloseNow()
	writeUploadText(t, browser, map[string]any{
		"type": "start", "csrf": csrf, "session": identity,
		"files": []any{map[string]any{"size": len(first), "extension": "txt"}, map[string]any{"size": len(second), "extension": "bin"}},
	})
	var message struct {
		Type     string             `json:"type"`
		Received int64              `json:"received"`
		Stage    filestage.Response `json:"stage"`
	}
	readUploadJSON(t, browser, &message)
	if message.Type != "ready" {
		t.Fatalf("not ready: %+v", message)
	}
	for _, chunk := range [][]byte{{0, 1, 2, 3}, {4, 0xff, 6}} {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		if err := browser.Write(ctx, websocket.MessageBinary, chunk); err != nil {
			cancel()
			t.Fatal(err)
		}
		cancel()
		readUploadJSON(t, browser, &message)
		if message.Type != "ack" {
			t.Fatalf("chunk not acknowledged: %+v", message)
		}
	}
	writeUploadText(t, browser, map[string]string{"type": "finish"})
	readUploadJSON(t, browser, &message)
	completed := time.Now()
	if message.Type != "complete" || len(message.Stage.Files) != 2 {
		t.Fatalf("actual receiver did not complete: %+v", message)
	}
	if message.Stage.Session != identity || message.Stage.ExpiresAtUnix < started.Add(webFileStageTTL).Unix() ||
		message.Stage.ExpiresAtUnix > completed.Add(webFileStageTTL+time.Minute).Unix() {
		t.Fatalf("identity or expiry mismatch: %+v", message.Stage)
	}
	for index, want := range [][]byte{first, second} {
		got, err := os.ReadFile(message.Stage.Files[index].Path)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("file %d changed: %x", index, got)
		}
		sum := sha256.Sum256(want)
		if message.Stage.Files[index].SHA256 != hex.EncodeToString(sum[:]) {
			t.Fatalf("file %d hash mismatch", index)
		}
	}
	select {
	case <-workerDone:
	case <-time.After(3 * time.Second):
		t.Fatal("actual receiver worker did not finish")
	}
	if verifyCalls.Load() != 2 {
		t.Fatalf("session verification calls=%d", verifyCalls.Load())
	}
}

func TestUploadActualHomeReceiverCancellationJoinsPartialCleanup(t *testing.T) {
	root := filepath.Join(t.TempDir(), "hmux", "staged-files-v1")
	originalRoot, originalVerify := homeFileStageRoot, homeFileStageVerify
	homeFileStageRoot = func() (string, error) { return root, nil }
	homeFileStageVerify = func(context.Context, filestage.SessionIdentity) error { return nil }

	s, ts, token, csrf := uploadSocketServer(t)
	home, connection := connectFakeUploadHome(t, s, ts)
	homeCtx, cancelHome := context.WithCancel(context.Background())
	bridgeDone, workerDone := serveActualUploadHome(homeCtx, home)
	defer func() {
		cancelHome()
		_ = connection.CloseNow()
		select {
		case <-bridgeDone:
		case <-time.After(3 * time.Second):
			t.Error("actual Home bridge did not stop")
		}
		homeFileStageRoot, homeFileStageVerify = originalRoot, originalVerify
	}()

	browser := dialUpload(t, ts, s.origin, token)
	writeUploadText(t, browser, map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 2, "extension": "bin"}}})
	var message map[string]any
	readUploadJSON(t, browser, &message)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	if err := browser.Write(ctx, websocket.MessageBinary, []byte{1}); err != nil {
		cancel()
		t.Fatal(err)
	}
	cancel()
	readUploadJSON(t, browser, &message)
	_ = browser.CloseNow()
	select {
	case <-workerDone:
	case <-time.After(3 * time.Second):
		t.Fatal("canceled actual receiver worker did not finish")
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.Name() != ".lock" {
			t.Fatalf("partial stage survived joined cancellation: %q", entry.Name())
		}
	}
}

func TestUploadRejectsShortAndExtraStreamsAndCancelsHome(t *testing.T) {
	for name, test := range map[string]struct {
		declared int64
		body     []byte
	}{
		"short": {declared: 2, body: []byte{1}},
		"extra": {declared: 1, body: []byte{1, 2}},
	} {
		t.Run(name, func(t *testing.T) {
			s, ts, token, csrf := uploadSocketServer(t)
			home, _ := connectFakeUploadHome(t, s, ts)
			canceled := make(chan struct{}, 1)
			go fakeUploadReceiver(context.Background(), home, false, nil, canceled)
			browser := dialUpload(t, ts, s.origin, token)
			defer browser.CloseNow()
			writeUploadText(t, browser, map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": test.declared, "extension": ""}}})
			var message map[string]any
			readUploadJSON(t, browser, &message)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			if name == "short" {
				if err := browser.Write(ctx, websocket.MessageBinary, test.body); err != nil {
					t.Fatal(err)
				}
				readUploadJSON(t, browser, &message)
				writeUploadText(t, browser, map[string]string{"type": "finish"})
			} else if err := browser.Write(ctx, websocket.MessageBinary, test.body); err != nil {
				t.Fatal(err)
			}
			cancel()
			readUploadJSON(t, browser, &message)
			if message["type"] != "error" || message["error"] != "Upload data rejected" {
				t.Fatalf("stream accepted: %v", message)
			}
			select {
			case <-canceled:
			case <-time.After(3 * time.Second):
				t.Fatal("Home upload was not canceled")
			}
		})
	}
}

func TestUploadConnectorLossFailsWithoutRerouting(t *testing.T) {
	s, ts, token, csrf := uploadSocketServer(t)
	home, connection := connectFakeUploadHome(t, s, ts)
	go func() {
		message, err := home.read(context.Background())
		if err == nil && message.Type == "upload-start" {
			_ = home.send(context.Background(), Message{Type: "upload-ready", ID: message.ID})
			if message, err = home.read(context.Background()); err == nil && message.Type == "upload-data" {
				connection.CloseNow()
			}
		}
	}()
	browser := dialUpload(t, ts, s.origin, token)
	defer browser.CloseNow()
	writeUploadText(t, browser, map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 1, "extension": ""}}})
	var message map[string]any
	readUploadJSON(t, browser, &message)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := browser.Write(ctx, websocket.MessageBinary, []byte{1}); err != nil {
		t.Fatal(err)
	}
	readUploadJSON(t, browser, &message)
	if message["type"] != "error" || message["error"] != "Home upload unavailable" {
		t.Fatalf("connector loss response=%v", message)
	}
}

func TestUploadLogoutAfterReadyCancelsHome(t *testing.T) {
	s, ts, token, csrf := uploadSocketServer(t)
	home, _ := connectFakeUploadHome(t, s, ts)
	canceled := make(chan struct{}, 1)
	go fakeUploadReceiver(context.Background(), home, false, nil, canceled)
	browser := dialUpload(t, ts, s.origin, token)
	defer browser.CloseNow()
	writeUploadText(t, browser, map[string]any{"type": "start", "csrf": csrf, "session": map[string]any{"id": "$1", "created_at": 42}, "files": []any{map[string]any{"size": 1, "extension": ""}}})
	var message map[string]any
	readUploadJSON(t, browser, &message)
	if message["type"] != "ready" {
		t.Fatal(message)
	}
	if err := s.auth.logout(token); err != nil {
		t.Fatal(err)
	}
	select {
	case <-canceled:
	case <-time.After(3 * time.Second):
		t.Fatal("logout did not cancel Home upload")
	}
}

func TestHomeFileStageSweeperRunsAtStartupAndPeriodically(t *testing.T) {
	original := homeFileStageSweep
	defer func() { homeFileStageSweep = original }()
	calls := make(chan time.Time, 4)
	homeFileStageSweep = func(_ context.Context, root string, now time.Time) error {
		if root != "/tmp/hmux/staged-files-v1" {
			t.Errorf("root=%q", root)
		}
		calls <- now
		return nil
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		runHomeFileStageSweeper(ctx, "/tmp/hmux/staged-files-v1", 5*time.Millisecond, func() time.Time { return time.Unix(42, 0) })
		close(done)
	}()
	for index := 0; index < 2; index++ {
		select {
		case got := <-calls:
			if got.Unix() != 42 {
				t.Fatalf("time=%v", got)
			}
		case <-time.After(time.Second):
			t.Fatal("sweeper did not run")
		}
	}
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("sweeper did not stop")
	}
}

func fakeUploadReceiver(ctx context.Context, home *peer, changed bool, bodySeen chan<- []byte, canceled chan<- struct{}) {
	var header *filestage.Header
	var body []byte
	for {
		message, err := home.read(ctx)
		if err != nil {
			return
		}
		switch message.Type {
		case "upload-start":
			header = message.Header
			_ = home.send(ctx, Message{Type: "upload-ready", ID: message.ID})
		case "upload-data":
			body = append(body, message.Data...)
			_ = home.send(ctx, Message{Type: "upload-ack", ID: message.ID, Received: int64(len(body))})
		case "upload-cancel":
			if canceled != nil {
				canceled <- struct{}{}
			}
			return
		case "upload-finish":
			if bodySeen != nil {
				bodySeen <- append([]byte(nil), body...)
			}
			response := fakeStageResponse(*header, body)
			if changed {
				response.Session.CreatedAt++
			}
			raw, _ := json.Marshal(response)
			_ = home.send(ctx, Message{Type: "upload-complete", ID: message.ID, Payload: raw})
			return
		}
	}
}

func serveActualUploadHome(ctx context.Context, home *peer) (<-chan struct{}, <-chan struct{}) {
	bridgeDone := make(chan struct{})
	workerDone := make(chan struct{}, 1)
	go func() {
		defer close(bridgeDone)
		var workers sync.WaitGroup
		var cancelUpload context.CancelFunc
		var input chan Message
		defer func() {
			if cancelUpload != nil {
				cancelUpload()
			}
			workers.Wait()
		}()
		for {
			message, err := home.read(ctx)
			if err != nil {
				return
			}
			switch message.Type {
			case "upload-start":
				uploadCtx, cancel := context.WithCancel(ctx)
				cancelUpload = cancel
				input = make(chan Message, 1)
				workers.Add(1)
				go func(header filestage.Header) {
					defer workers.Done()
					runHomeUpload(uploadCtx, home, header, input)
					workerDone <- struct{}{}
				}(*message.Header)
			case "upload-data", "upload-finish":
				select {
				case input <- message:
				case <-ctx.Done():
					return
				}
			case "upload-cancel":
				if cancelUpload != nil {
					cancelUpload()
				}
			}
		}
	}()
	return bridgeDone, workerDone
}

func fakeStageResponse(header filestage.Header, body []byte) filestage.Response {
	const stageID = "ffeeddccbbaa99887766554433221100"
	expires := time.Now().Add(filestage.StageTTL).Unix()
	stageDir := fmt.Sprintf("%d-%s", expires, stageID)
	response := filestage.Response{ProtocolVersion: filestage.ProtocolVersion, RequestID: header.RequestID, StageID: stageID, Session: header.Session, ExpiresAtUnix: expires}
	offset := 0
	for index, file := range header.Files {
		end := offset + int(file.Size)
		sum := sha256.Sum256(body[offset:end])
		name := fmt.Sprintf("file-%04d", index+1)
		if file.Extension != "" {
			name += "." + file.Extension
		}
		response.Files = append(response.Files, filestage.StagedFile{Index: index, Path: filepath.Join("/tmp/hmux/staged-files-v1", stageDir, name), Size: file.Size, SHA256: hex.EncodeToString(sum[:])})
		offset = end
	}
	return response
}

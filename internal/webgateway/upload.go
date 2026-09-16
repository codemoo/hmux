package webgateway

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"hash"
	"io"
	"net/http"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
)

const (
	uploadOverallTimeout = 5 * time.Minute
	uploadIdleTimeout    = 30 * time.Second
	maxGlobalUploads     = 2
)

type uploadLimiter struct {
	mu      sync.Mutex
	total   int
	byLogin map[string]int
}

func newUploadLimiter() *uploadLimiter { return &uploadLimiter{byLogin: make(map[string]int)} }

func (l *uploadLimiter) acquire(login string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.total >= maxGlobalUploads || l.byLogin[login] >= 1 {
		return false
	}
	l.total++
	l.byLogin[login]++
	return true
}

func (l *uploadLimiter) release(login string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.byLogin[login] > 0 {
		l.byLogin[login]--
		l.total--
		if l.byLogin[login] == 0 {
			delete(l.byLogin, login)
		}
	}
}

type browserUploadStart struct {
	Type    string                    `json:"type"`
	CSRF    string                    `json:"csrf"`
	Session filestage.SessionIdentity `json:"session"`
	Files   []browserUploadFile       `json:"files"`
}

type browserUploadFile struct {
	Size      int64  `json:"size"`
	Extension string `json:"extension"`
}

func (s *Server) upload(w http.ResponseWriter, r *http.Request, token, csrf string, done <-chan struct{}) {
	if r.Method != http.MethodGet || r.Header.Get("Origin") != s.origin {
		http.Error(w, "Forbidden", http.StatusForbidden)
		return
	}
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{OriginPatterns: []string{s.host}, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		return
	}
	defer conn.CloseNow()
	conn.SetReadLimit(maxUploadChunk + 1)
	ctx, cancel := context.WithTimeout(r.Context(), uploadOverallTimeout)
	defer cancel()
	go func() {
		select {
		case <-done:
			cancel()
		case <-ctx.Done():
		}
	}()

	kind, raw, err := readUploadFrame(ctx, conn, 10*time.Second)
	var start browserUploadStart
	if err != nil || kind != websocket.MessageText || strictUploadJSON(raw, &start) != nil || start.Type != "start" ||
		subtle.ConstantTimeCompare([]byte(start.CSRF), []byte(csrf)) != 1 {
		writeBrowserUploadError(conn, "Invalid upload request")
		return
	}
	header, err := browserUploadHeader(start)
	if err != nil {
		writeBrowserUploadError(conn, "Invalid upload request")
		return
	}
	if _, _, ok := s.auth.get(token, true); !ok {
		return
	}
	login := sessionKey(token)
	if !s.uploads.acquire(login) {
		writeBrowserUploadError(conn, "Upload limit reached")
		return
	}
	defer s.uploads.release(login)
	upload, err := s.hub.openUpload(header)
	if err != nil {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	completed := false
	defer func() {
		if !completed {
			cancelCtx, stop := context.WithTimeout(context.Background(), 2*time.Second)
			_ = s.hub.sendUpload(cancelCtx, upload, Message{Type: "upload-cancel", ID: header.RequestID})
			stop()
		}
		s.hub.closeUpload(header.RequestID, upload)
	}()
	if s.hub.sendUpload(ctx, upload, Message{Type: "upload-start", ID: header.RequestID, Header: &header}) != nil {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	event, err := waitUploadEvent(ctx, upload, uploadIdleTimeout)
	if err != nil || event.Type != "upload-ready" {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	if writeBrowserUploadJSON(ctx, conn, map[string]string{"type": "ready"}) != nil {
		return
	}

	hashers := make([]hash.Hash, len(header.Files))
	for index := range hashers {
		hashers[index] = sha256.New()
	}
	fileIndex := 0
	fileRemaining := header.Files[0].Size
	var received int64
	for received < header.TotalBytes {
		kind, raw, err = readUploadFrame(ctx, conn, uploadIdleTimeout)
		if err != nil || kind != websocket.MessageBinary || len(raw) < 1 || len(raw) > maxUploadChunk || int64(len(raw)) > header.TotalBytes-received {
			writeBrowserUploadError(conn, "Upload data rejected")
			return
		}
		if _, _, ok := s.auth.get(token, true); !ok {
			return
		}
		consumeUploadHashes(hashers, header.Files, &fileIndex, &fileRemaining, raw)
		received += int64(len(raw))
		if s.hub.sendUpload(ctx, upload, Message{Type: "upload-data", ID: header.RequestID, Data: raw}) != nil {
			writeBrowserUploadError(conn, "Home upload unavailable")
			return
		}
		event, err = waitUploadEvent(ctx, upload, uploadIdleTimeout)
		if err != nil || event.Type != "upload-ack" || event.Received != received {
			writeBrowserUploadError(conn, "Home upload unavailable")
			return
		}
		if writeBrowserUploadJSON(ctx, conn, map[string]any{"type": "ack", "received": received}) != nil {
			return
		}
	}
	kind, raw, err = readUploadFrame(ctx, conn, uploadIdleTimeout)
	var finish struct {
		Type string `json:"type"`
	}
	if err != nil || kind != websocket.MessageText || strictUploadJSON(raw, &finish) != nil || finish.Type != "finish" {
		writeBrowserUploadError(conn, "Upload data rejected")
		return
	}
	if _, _, ok := s.auth.get(token, true); !ok {
		return
	}
	if s.hub.sendUpload(ctx, upload, Message{Type: "upload-finish", ID: header.RequestID}) != nil {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	event, err = waitUploadEvent(ctx, upload, uploadIdleTimeout)
	if err != nil || event.Type != "upload-complete" || len(event.Payload) == 0 {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	if _, _, ok := s.auth.get(token, true); !ok {
		return
	}
	response, err := filestage.DecodeResponse(event.Payload)
	if err != nil {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	hashes := make([]string, len(hashers))
	for index := range hashers {
		hashes[index] = hex.EncodeToString(hashers[index].Sum(nil))
	}
	if filestage.ValidateResponseForHeader(response, header, hashes) != nil {
		writeBrowserUploadError(conn, "Home upload unavailable")
		return
	}
	completed = true
	_ = writeBrowserUploadJSON(ctx, conn, map[string]any{"type": "complete", "stage": response})
}

func browserUploadHeader(start browserUploadStart) (filestage.Header, error) {
	if model.ValidateSessionID(start.Session.ID) != nil || start.Session.CreatedAt < 1 || len(start.Files) < 1 || len(start.Files) > filestage.MaximumFiles {
		return filestage.Header{}, errors.New("invalid upload metadata")
	}
	requestID, err := filestage.NewRequestID()
	if err != nil {
		return filestage.Header{}, err
	}
	header := filestage.Header{ProtocolVersion: filestage.ProtocolVersion, RequestID: requestID, Session: start.Session, FileCount: len(start.Files)}
	for index, file := range start.Files {
		header.Files = append(header.Files, filestage.FileHeader{Index: index, Size: file.Size, Extension: file.Extension})
		header.TotalBytes += file.Size
	}
	if err := filestage.ValidateHeader(header); err != nil {
		return filestage.Header{}, err
	}
	return header, nil
}

func strictUploadJSON(raw []byte, value any) error {
	if len(raw) < 1 || len(raw) > filestage.MaximumHeaderBytes {
		return errors.New("invalid upload frame")
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return errors.New("trailing upload frame")
	}
	return nil
}

func readUploadFrame(ctx context.Context, conn *websocket.Conn, timeout time.Duration) (websocket.MessageType, []byte, error) {
	readCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	return conn.Read(readCtx)
}

func waitUploadEvent(ctx context.Context, upload *gatewayUpload, timeout time.Duration) (Message, error) {
	waitCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	select {
	case <-waitCtx.Done():
		return Message{}, waitCtx.Err()
	case event, ok := <-upload.events:
		if !ok || event.Type == "upload-error" || event.Error != "" {
			return Message{}, errors.New("Home upload unavailable")
		}
		return event, nil
	}
}

func writeBrowserUploadJSON(ctx context.Context, conn *websocket.Conn, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	writeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	return conn.Write(writeCtx, websocket.MessageText, raw)
}

func writeBrowserUploadError(conn *websocket.Conn, message string) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = writeBrowserUploadJSON(ctx, conn, map[string]string{"type": "error", "error": message})
}

func consumeUploadHashes(hashers []hash.Hash, files []filestage.FileHeader, fileIndex *int, fileRemaining *int64, data []byte) {
	for len(data) > 0 {
		count := int64(len(data))
		if count > *fileRemaining {
			count = *fileRemaining
		}
		_, _ = hashers[*fileIndex].Write(data[:int(count)])
		data = data[int(count):]
		*fileRemaining -= count
		if *fileRemaining == 0 && *fileIndex+1 < len(files) {
			*fileIndex++
			*fileRemaining = files[*fileIndex].Size
		}
	}
}

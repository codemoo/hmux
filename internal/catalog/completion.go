package catalog

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"os"
	"strconv"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

// TaskCompletion is bounded lifecycle metadata for one exact tmux session. The
// event ID is derived from local identities and a rollout byte offset; provider
// IDs, record paths and record contents never leave Home.
type TaskCompletion struct {
	Session     model.SessionIdentity
	EventID     string
	CompletedAt time.Time
}

// CompletionTracker observes the Codex rollout bound to each live tmux session.
// A zero tracker is ready to use. Its first observation establishes a baseline,
// so reconnecting a Home connector never replays historical completions.
// CompletionTracker is not safe for concurrent use.
type CompletionTracker struct {
	inspector systemProcessInspector
	cursors   map[model.SessionIdentity]completionCursor
}

type completionCursor struct {
	root, path, recordID string
	info                 os.FileInfo
	offset               int64
	anchor               [sha256.Size]byte
	anchorLength         int64
	armed                bool
}

type completionRecord struct {
	kind      string
	timestamp time.Time
	offset    int64
}

// Observe resolves current process-to-rollout bindings from the catalog's
// in-memory pane PIDs and returns only completions appended after a safe
// baseline. Binding failures reset the affected baseline rather than guessing.
func (t *CompletionTracker) Observe(ctx context.Context, value model.Catalog) ([]TaskCompletion, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if t.cursors == nil {
		t.cursors = make(map[model.SessionIdentity]completionCursor)
	}

	sessions := make(map[int][]model.Session)
	identities := make(map[model.SessionIdentity]bool)
	panes := make([]int, 0, len(value.Sessions))
	seenPanes := make(map[int]bool)
	for _, session := range value.Sessions {
		identity := model.SessionIdentity{ID: session.ID, CreatedAt: session.CreatedAt}
		if model.ValidateSessionID(identity.ID) != nil || identity.CreatedAt < 1 || session.PanePID < 1 {
			delete(t.cursors, identity)
			continue
		}
		identities[identity] = true
		sessions[session.PanePID] = append(sessions[session.PanePID], session)
		if !seenPanes[session.PanePID] {
			seenPanes[session.PanePID] = true
			panes = append(panes, session.PanePID)
		}
	}
	for identity := range t.cursors {
		if !identities[identity] {
			delete(t.cursors, identity)
		}
	}
	if len(panes) == 0 {
		return nil, nil
	}

	nodes, err := t.inspector.processSnapshot(ctx)
	if err != nil {
		// A later successful observation must baseline again; otherwise events
		// written while process ownership was unknown could be replayed.
		clear(t.cursors)
		return nil, err
	}
	bindings := t.inspector.resolveCompletionBindings(ctx, nodes, panes)
	if err := ctx.Err(); err != nil {
		clear(t.cursors)
		return nil, err
	}

	var completions []TaskCompletion
	for pane, paneSessions := range sessions {
		binding := bindings[pane]
		for _, session := range paneSessions {
			if err := ctx.Err(); err != nil {
				clear(t.cursors)
				return nil, err
			}
			identity := model.SessionIdentity{ID: session.ID, CreatedAt: session.CreatedAt}
			if binding.provider != "codex" || binding.status != sessionBindingReady || binding.path == "" || binding.root == "" || binding.recordID == "" {
				delete(t.cursors, identity)
				continue
			}
			found, next, ok := t.observeBinding(ctx, identity, binding)
			if !ok {
				delete(t.cursors, identity)
				continue
			}
			t.cursors[identity] = next
			completions = append(completions, found...)
		}
	}
	if err := ctx.Err(); err != nil {
		clear(t.cursors)
		return nil, err
	}
	return completions, nil
}

func (t *CompletionTracker) observeBinding(ctx context.Context, identity model.SessionIdentity, binding sessionBinding) ([]TaskCompletion, completionCursor, bool) {
	file, info, err := openSessionRecord(binding.root, binding.path)
	if err != nil {
		return nil, completionCursor{}, false
	}
	defer file.Close()

	current, exists := t.cursors[identity]
	anchor, anchorLength, anchorOK := completionAnchor(ctx, file, current.offset)
	sameBinding := exists && current.root == binding.root && current.path == binding.path && current.recordID == binding.recordID &&
		current.info != nil && os.SameFile(current.info, info) && info.Size() >= current.offset && anchorOK &&
		current.anchorLength == anchorLength && current.anchor == anchor
	if !sameBinding || info.Size()-current.offset > eventTailLimit {
		records, offset, ok := readCompletionTail(ctx, file, info.Size())
		if !ok {
			return nil, completionCursor{}, false
		}
		anchor, anchorLength, ok = completionAnchor(ctx, file, offset)
		if !ok {
			return nil, completionCursor{}, false
		}
		armed := false
		for _, record := range records {
			switch record.kind {
			case "task_started":
				armed = true
			case "task_complete":
				armed = false
			}
		}
		return nil, completionCursor{
			root: binding.root, path: binding.path, recordID: binding.recordID,
			info: info, offset: offset, anchor: anchor, anchorLength: anchorLength, armed: armed,
		}, true
	}

	records, offset, ok := readCompletionRange(ctx, file, current.offset, info.Size()-current.offset)
	if !ok {
		return nil, completionCursor{}, false
	}
	current.info = info
	current.offset = offset
	current.anchor, current.anchorLength, ok = completionAnchor(ctx, file, offset)
	if !ok {
		return nil, completionCursor{}, false
	}
	var completions []TaskCompletion
	for _, record := range records {
		switch record.kind {
		case "task_started":
			current.armed = true
		case "task_complete":
			if current.armed && !record.timestamp.IsZero() {
				completions = append(completions, TaskCompletion{
					Session:     identity,
					EventID:     completionEventID(identity, binding.recordID, record.offset),
					CompletedAt: record.timestamp,
				})
			}
			current.armed = false
		}
	}
	return completions, current, true
}

func completionAnchor(ctx context.Context, file *os.File, offset int64) ([sha256.Size]byte, int64, bool) {
	const anchorLimit = 256
	length := offset
	if length > anchorLimit {
		length = anchorLimit
	}
	data, ok := readCompletionBytes(ctx, file, offset-length, length, anchorLimit)
	if !ok {
		return [sha256.Size]byte{}, 0, false
	}
	return sha256.Sum256(data), length, true
}

func readCompletionTail(ctx context.Context, file *os.File, size int64) ([]completionRecord, int64, bool) {
	start := size - eventTailLimit
	if start < 0 {
		start = 0
	}
	readStart := start
	if readStart > 0 {
		readStart--
	}
	data, ok := readCompletionBytes(ctx, file, readStart, size-readStart, eventTailLimit+1)
	if !ok {
		return nil, 0, false
	}
	base := readStart
	if start > 0 {
		if data[0] == '\n' {
			data = data[1:]
			base++
		} else if newline := bytes.IndexByte(data, '\n'); newline >= 0 {
			data = data[newline+1:]
			base += int64(newline + 1)
		} else {
			// No bounded complete record is available. Baseline at EOF and do
			// not reinterpret a suffix of the oversized record on a later poll.
			return nil, size, true
		}
	}
	return parseCompletionRecords(ctx, data, base)
}

func readCompletionRange(ctx context.Context, file *os.File, start, length int64) ([]completionRecord, int64, bool) {
	if length < 0 || length > eventTailLimit {
		return nil, 0, false
	}
	data, ok := readCompletionBytes(ctx, file, start, length, eventTailLimit)
	if !ok {
		return nil, 0, false
	}
	return parseCompletionRecords(ctx, data, start)
}

func readCompletionBytes(ctx context.Context, file io.ReadSeeker, start, length, limit int64) ([]byte, bool) {
	if start < 0 || length < 0 || length > limit {
		return nil, false
	}
	if _, err := file.Seek(start, io.SeekStart); err != nil {
		return nil, false
	}
	data := make([]byte, int(length))
	for offset := 0; offset < len(data); {
		if ctx.Err() != nil {
			return nil, false
		}
		end := min(offset+(32<<10), len(data))
		if _, err := io.ReadFull(file, data[offset:end]); err != nil {
			return nil, false
		}
		offset = end
	}
	return data, ctx.Err() == nil
}

func parseCompletionRecords(ctx context.Context, data []byte, base int64) ([]completionRecord, int64, bool) {
	lastNewline := bytes.LastIndexByte(data, '\n')
	if lastNewline < 0 {
		return nil, base, true
	}
	complete := data[:lastNewline+1]
	records := make([]completionRecord, 0)
	lineStart := 0
	for lineStart < len(complete) {
		if ctx.Err() != nil {
			return nil, 0, false
		}
		relativeEnd := bytes.IndexByte(complete[lineStart:], '\n')
		if relativeEnd < 0 {
			break
		}
		lineEnd := lineStart + relativeEnd
		if lineEnd > lineStart && lineEnd-lineStart <= eventLineLimit {
			if record, ok := decodeCompletionRecord(complete[lineStart:lineEnd], base+int64(lineStart)); ok {
				records = append(records, record)
			}
		}
		lineStart = lineEnd + 1
	}
	return records, base + int64(lastNewline+1), true
}

func decodeCompletionRecord(line []byte, offset int64) (completionRecord, bool) {
	var event struct {
		Type      string `json:"type"`
		Timestamp string `json:"timestamp"`
		Payload   struct {
			Type string `json:"type"`
		} `json:"payload"`
	}
	if json.Unmarshal(line, &event) != nil {
		return completionRecord{}, false
	}
	kind := event.Payload.Type
	if event.Type == "task_started" || event.Type == "task_complete" {
		kind = event.Type
	}
	if kind != "task_started" && kind != "task_complete" {
		return completionRecord{}, false
	}
	record := completionRecord{kind: kind, offset: offset}
	if event.Timestamp != "" {
		if parsed, err := time.Parse(time.RFC3339Nano, event.Timestamp); err == nil {
			record.timestamp = parsed.UTC()
		}
	}
	return record, true
}

func completionEventID(identity model.SessionIdentity, recordID string, offset int64) string {
	hash := sha256.New()
	_, _ = io.WriteString(hash, "hmux-codex-task-complete-v1\x00")
	_, _ = io.WriteString(hash, identity.ID)
	_, _ = io.WriteString(hash, "\x00")
	_, _ = io.WriteString(hash, strconv.FormatInt(identity.CreatedAt, 10))
	_, _ = io.WriteString(hash, "\x00")
	_, _ = io.WriteString(hash, recordID)
	_, _ = io.WriteString(hash, "\x00")
	_, _ = io.WriteString(hash, strconv.FormatInt(offset, 10))
	return hex.EncodeToString(hash.Sum(nil))
}

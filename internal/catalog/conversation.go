package catalog

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/timing"
)

const (
	conversationTailLimit   = int64(4 * 1024 * 1024)
	conversationLineLimit   = 1 * 1024 * 1024
	conversationMessageSize = 256 * 1024
	conversationTextLimit   = 512 * 1024
	conversationEncodedMax  = 2*1024*1024 - 4096 // reserve agent newline and app envelope
	conversationMessageMax  = 200
)

type conversationBinding struct {
	provider    string
	providerPID int
	filePID     int
	path        string
	root        string
	recordID    string
}

type conversationDependencies struct {
	runner  Runner
	home    string
	binding func(context.Context, int, string) (conversationBinding, sessionBindingStatus)
}

// ReadConversation returns public Codex/Claude user and assistant messages for one
// exact Home tmux session instance. Operational discovery failures are
// represented by the status field so source paths and command details never
// cross the API boundary.
func ReadConversation(ctx context.Context, id string, createdAt int64) (model.Conversation, error) {
	if err := model.ValidateSessionID(id); err != nil || createdAt < 1 {
		return model.Conversation{}, errors.New("invalid conversation request")
	}
	if err := ctx.Err(); err != nil {
		return model.Conversation{}, err
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	return readConversation(ctx, id, createdAt, conversationDependencies{
		runner:  TmuxRunner{},
		home:    home,
		binding: inspectConversationBinding,
	})
}

func readConversation(ctx context.Context, id string, createdAt int64, dependencies conversationDependencies) (model.Conversation, error) {
	if err := model.ValidateSessionID(id); err != nil || createdAt < 1 || dependencies.runner == nil || dependencies.binding == nil {
		return model.Conversation{}, errors.New("invalid conversation request")
	}
	if err := ctx.Err(); err != nil {
		return model.Conversation{}, err
	}
	first, err := readConversationSession(ctx, dependencies.runner, id, createdAt)
	if err != nil {
		if ctx.Err() != nil {
			return model.Conversation{}, ctx.Err()
		}
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	if first == nil || first.PanePID < 1 {
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}

	binding, status := dependencies.binding(ctx, first.PanePID, dependencies.home)
	if err := ctx.Err(); err != nil {
		return model.Conversation{}, err
	}
	if status != sessionBindingReady {
		return emptyConversation(id, createdAt, bindingModelStatus(status)), nil
	}
	file, info, err := openConversationBinding(dependencies.home, binding)
	if err != nil {
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	messages, truncated, readErr := readConversationMessages(ctx, file, info, binding.provider)
	closeErr := file.Close()
	if readErr != nil || closeErr != nil {
		if ctx.Err() != nil {
			return model.Conversation{}, ctx.Err()
		}
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}

	second, err := readConversationSession(ctx, dependencies.runner, id, createdAt)
	if err != nil {
		if ctx.Err() != nil {
			return model.Conversation{}, ctx.Err()
		}
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	if second == nil || second.PanePID != first.PanePID {
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	secondBinding, secondStatus := dependencies.binding(ctx, second.PanePID, dependencies.home)
	if err := ctx.Err(); err != nil {
		return model.Conversation{}, err
	}
	if secondStatus != sessionBindingReady {
		return emptyConversation(id, createdAt, bindingModelStatus(secondStatus)), nil
	}
	if secondBinding.provider != binding.provider || secondBinding.providerPID != binding.providerPID || secondBinding.filePID != binding.filePID || secondBinding.path != binding.path || secondBinding.root != binding.root || secondBinding.recordID != binding.recordID {
		return emptyConversation(id, createdAt, model.ConversationAmbiguous), nil
	}
	secondFile, secondInfo, err := openConversationBinding(dependencies.home, secondBinding)
	if err != nil {
		return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
	}
	_ = secondFile.Close()
	if !os.SameFile(info, secondInfo) || secondInfo.Size() < info.Size() {
		return emptyConversation(id, createdAt, model.ConversationAmbiguous), nil
	}

	result := model.Conversation{
		SessionID: id,
		CreatedAt: createdAt,
		Provider:  binding.provider,
		Status:    model.ConversationReady,
		Messages:  messages,
		Truncated: truncated,
	}
	for conversationTextBytes(result.Messages) > conversationTextLimit {
		result.Truncated = true
		result.Messages = result.Messages[1:]
	}
	for {
		encoded, marshalErr := json.Marshal(result)
		if marshalErr != nil {
			return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
		}
		if len(encoded) <= conversationEncodedMax {
			break
		}
		result.Truncated = true
		if len(result.Messages) == 0 {
			return emptyConversation(id, createdAt, model.ConversationUnavailable), nil
		}
		result.Messages = result.Messages[1:]
	}
	if err := ctx.Err(); err != nil {
		return model.Conversation{}, err
	}
	return result, nil
}

func emptyConversation(id string, createdAt int64, status string) model.Conversation {
	return model.Conversation{
		SessionID: id,
		CreatedAt: createdAt,
		Status:    status,
		Messages:  []model.ConversationMessage{},
	}
}

func bindingModelStatus(status sessionBindingStatus) string {
	if status == sessionBindingAmbiguous {
		return model.ConversationAmbiguous
	}
	return model.ConversationUnavailable
}

func readConversationSession(ctx context.Context, runner Runner, id string, createdAt int64) (*model.Session, error) {
	catalog, err := ReadBasic(ctx, runner)
	if err != nil {
		return nil, errors.New("conversation catalog unavailable")
	}
	for index := range catalog.Sessions {
		session := &catalog.Sessions[index]
		if session.ID == id && session.CreatedAt == createdAt {
			return session, nil
		}
	}
	return nil, nil
}

func inspectConversationBinding(ctx context.Context, panePID int, home string) (conversationBinding, sessionBindingStatus) {
	defer timing.Start(ctx, "conversation-binding", false)()
	inspector := systemProcessInspector{}
	nodes, err := inspector.processSnapshot(ctx)
	if err != nil {
		return conversationBinding{}, sessionBindingUnavailable
	}
	binding := inspector.resolveSessionBindings(ctx, nodes, []int{panePID}, home)[panePID]
	if binding.provider != "codex" && binding.provider != "claude" {
		return conversationBinding{}, sessionBindingUnavailable
	}
	return conversationBinding{provider: binding.provider, providerPID: binding.providerPID, filePID: binding.filePID, path: binding.path, root: binding.root, recordID: binding.recordID}, binding.status
}

func openBoundConversationFile(home, path string) (*os.File, os.FileInfo, error) {
	if filepath.Ext(path) != ".jsonl" {
		return nil, nil, errors.New("conversation source unavailable")
	}
	return openSessionRecord(filepath.Join(home, ".codex", "sessions"), path)
}

func openConversationBinding(home string, binding conversationBinding) (*os.File, os.FileInfo, error) {
	if binding.root == "" {
		return openBoundConversationFile(home, binding.path)
	}
	if filepath.Ext(binding.path) != ".jsonl" {
		return nil, nil, errors.New("conversation source unavailable")
	}
	return openSessionRecord(binding.root, binding.path)
}

func readConversationMessages(ctx context.Context, file *os.File, info os.FileInfo, providers ...string) ([]model.ConversationMessage, bool, error) {
	defer timing.Start(ctx, "conversation-read", false)()
	if err := ctx.Err(); err != nil {
		return nil, false, err
	}
	parser := parseConversationMessage
	if len(providers) > 0 && providers[0] == "claude" {
		parser = parseClaudeConversationMessage
	}
	size := info.Size()
	if size < 0 {
		return nil, false, errors.New("conversation source unavailable")
	}
	start := size - conversationTailLimit
	truncated := start > 0
	if start < 0 {
		start = 0
	}
	data, err := io.ReadAll(&conversationContextReader{
		ctx:    ctx,
		reader: io.NewSectionReader(file, start, size-start),
	})
	if err != nil || int64(len(data)) != size-start {
		return nil, false, errors.New("conversation source unavailable")
	}
	baseOffset := start
	if start > 0 {
		newline := bytes.IndexByte(data, '\n')
		if newline < 0 {
			return []model.ConversationMessage{}, true, nil
		}
		data = data[newline+1:]
		baseOffset += int64(newline + 1)
	}
	if len(data) > 0 && data[len(data)-1] != '\n' {
		truncated = true
		lastNewline := bytes.LastIndexByte(data, '\n')
		if lastNewline < 0 {
			return []model.ConversationMessage{}, true, nil
		}
		data = data[:lastNewline+1]
	}
	identity := conversationFileIdentity(info)
	handoffs, err := conversationCompactedHandoffs(ctx, data)
	if err != nil {
		return nil, false, err
	}
	messages := make([]model.ConversationMessage, 0, conversationMessageMax)
	lineStart := 0
	lineNumber := 0
	for lineStart < len(data) {
		lineEndRelative := bytes.IndexByte(data[lineStart:], '\n')
		if lineEndRelative < 0 {
			break
		}
		lineEnd := lineStart + lineEndRelative
		line := data[lineStart:lineEnd]
		if lineNumber%64 == 0 {
			if err := ctx.Err(); err != nil {
				return nil, false, err
			}
		}
		lineNumber++
		if len(line) > conversationLineLimit {
			truncated = true
		} else if message, ok, omitted := parser(line, identity, baseOffset+int64(lineStart)); omitted {
			truncated = true
		} else if ok {
			_, compacted := handoffs[sha256.Sum256([]byte(strings.TrimSpace(message.Text)))]
			if message.Role == "assistant" && compacted {
				lineStart = lineEnd + 1
				continue
			}
			if len(messages) == conversationMessageMax {
				copy(messages, messages[1:])
				messages = messages[:conversationMessageMax-1]
				truncated = true
			}
			messages = append(messages, message)
		}
		lineStart = lineEnd + 1
	}
	return messages, truncated, nil
}

const conversationContinuationPrefix = "Another language model started to solve this problem and produced a summary of its thinking process."
const conversationContinuationSummary = "Here is the summary produced by the other language model"

// Compaction output can use any heading and is stored as a public final answer.
// The subsequent compacted record identifies its exact text in the continuation
// envelope. Index hashes first so hidden summaries do not consume the public
// message limit. Only complete records from the already bounded 4 MiB tail are
// read. Compacted records may exceed the public-message line limit because they
// also contain replacement history; only their envelope is decoded here.
func conversationCompactedHandoffs(ctx context.Context, data []byte) (map[[32]byte]struct{}, error) {
	handoffs := make(map[[32]byte]struct{})
	for line := range bytes.SplitSeq(data, []byte{'\n'}) {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if !bytes.Contains(line, []byte(`"compacted"`)) {
			continue
		}
		var event struct {
			Type    string `json:"type"`
			Payload struct {
				Message string `json:"message"`
			} `json:"payload"`
		}
		if json.Unmarshal(line, &event) != nil || event.Type != "compacted" {
			continue
		}
		text := strings.TrimSpace(event.Payload.Message)
		if !strings.HasPrefix(text, conversationContinuationPrefix) {
			continue
		}
		_, rest, found := strings.Cut(text, conversationContinuationSummary)
		if !found {
			continue
		}
		_, summary, found := strings.Cut(rest, ":")
		summary = strings.TrimSpace(summary)
		if found && summary != "" {
			handoffs[sha256.Sum256([]byte(summary))] = struct{}{}
		}
	}
	return handoffs, nil
}

type conversationContextReader struct {
	ctx    context.Context
	reader io.Reader
}

func (reader *conversationContextReader) Read(buffer []byte) (int, error) {
	if err := reader.ctx.Err(); err != nil {
		return 0, err
	}
	if len(buffer) > 64*1024 {
		buffer = buffer[:64*1024]
	}
	return reader.reader.Read(buffer)
}

func conversationFileIdentity(info os.FileInfo) []byte {
	var identity [16]byte
	if stat, ok := info.Sys().(*syscall.Stat_t); ok {
		binary.BigEndian.PutUint64(identity[:8], uint64(stat.Dev))
		binary.BigEndian.PutUint64(identity[8:], uint64(stat.Ino))
	}
	return identity[:]
}

func conversationTextBytes(messages []model.ConversationMessage) int {
	total := 0
	for _, message := range messages {
		total += len(message.Text)
	}
	return total
}

func parseConversationMessage(line, fileIdentity []byte, offset int64) (model.ConversationMessage, bool, bool) {
	var event struct {
		Type    string `json:"type"`
		Payload struct {
			Type      string `json:"type"`
			Role      string `json:"role"`
			Recipient string `json:"recipient"`
			Channel   string `json:"channel"`
			Content   []struct {
				Type      string `json:"type"`
				Text      string `json:"text"`
				Recipient string `json:"recipient"`
				Channel   string `json:"channel"`
			} `json:"content"`
		} `json:"payload"`
	}
	if json.Unmarshal(line, &event) != nil || event.Type != "response_item" ||
		event.Payload.Type != "message" || !publicRecipient(event.Payload.Recipient) {
		return model.ConversationMessage{}, false, false
	}
	role := event.Payload.Role
	wantedContentType := ""
	switch role {
	case "user":
		wantedContentType = "input_text"
	case "assistant":
		if !publicAssistantChannel(event.Payload.Channel) {
			return model.ConversationMessage{}, false, false
		}
		wantedContentType = "output_text"
	default:
		return model.ConversationMessage{}, false, false
	}
	parts := make([]string, 0, len(event.Payload.Content))
	for _, content := range event.Payload.Content {
		if content.Type != wantedContentType || !publicRecipient(content.Recipient) || content.Text == "" {
			continue
		}
		if role == "assistant" && !publicAssistantChannel(content.Channel) {
			continue
		}
		if role == "user" && rejectedInjectedUserText(content.Text) {
			continue
		}
		parts = append(parts, content.Text)
	}
	if len(parts) == 0 {
		return model.ConversationMessage{}, false, false
	}
	text := strings.Join(parts, "\n")
	if rejectedConversationHandoff(text, role) {
		return model.ConversationMessage{}, false, false
	}
	if len(text) > conversationMessageSize {
		return model.ConversationMessage{}, false, true
	}
	hash := sha256.New()
	_, _ = hash.Write(fileIdentity)
	var offsetBytes [8]byte
	binary.BigEndian.PutUint64(offsetBytes[:], uint64(offset))
	_, _ = hash.Write(offsetBytes[:])
	_, _ = hash.Write(line)
	id := hex.EncodeToString(hash.Sum(nil)[:16])
	return model.ConversationMessage{ID: id, Role: role, Text: text}, true, false
}

// Some Codex rollouts store handoffs as ordinary final-answer messages without
// a summary channel. Match their envelope, not isolated words in public replies.
// User-authored task specifications must remain visible.
func rejectedConversationHandoff(text, role string) bool {
	text = strings.TrimSpace(text)
	if strings.HasPrefix(text, conversationContinuationPrefix) &&
		strings.Contains(text, conversationContinuationSummary) {
		return true
	}
	if role != "assistant" {
		return false
	}
	heading, rest, found := strings.Cut(text, "\n")
	return found && strings.TrimSpace(heading) == "## Task and constraints" &&
		strings.HasPrefix(strings.TrimSpace(rest), "Workspace:")
}

func publicRecipient(recipient string) bool {
	return recipient == "" || recipient == "all"
}

func publicAssistantChannel(channel string) bool {
	switch channel {
	case "", "commentary", "final":
		return true
	default:
		return false
	}
}

func rejectedInjectedUserText(text string) bool {
	lower := strings.ToLower(text)
	for _, marker := range []string{
		"# agents.md instructions",
		"<instructions",
		"</instructions>",
		"<environment_context",
		"</environment_context>",
		"<codex_internal_context",
		"</codex_internal_context>",
		"<recommended_plugins>",
		"</recommended_plugins>",
		"<available_deferred_tools>",
		"<tool_call",
		"<tool_result",
		"<function_call",
		"<function_result",
		"<user_shell_command",
		"</user_shell_command>",
		"<assistant recipient=",
		"<developer",
		"<system",
	} {
		if strings.Contains(lower, marker) {
			return true
		}
	}
	return false
}

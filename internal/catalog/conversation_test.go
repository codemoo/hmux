package catalog

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

func TestConversationParserIncludesOnlyPublicCanonicalMessages(t *testing.T) {
	identity := []byte("synthetic-file-id")
	tests := []struct {
		name string
		line string
		role string
		text string
		ok   bool
	}{
		{
			name: "user input and separate injected context",
			line: `{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Please review the change."},{"type":"input_text","text":"<environment_context>private wrapper</environment_context>"}]}}`,
			role: "user", text: "Please review the change.", ok: true,
		},
		{
			name: "assistant output",
			line: `{"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final","recipient":"all","content":[{"type":"output_text","text":"The review is complete.\n\nNo issues found."}]}}`,
			role: "assistant", text: "The review is complete.\n\nNo issues found.", ok: true,
		},
		{
			name: "injected goal context",
			line: conversationJSON("user", "input_text", "<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n</codex_internal_context>"),
		},
		{
			name: "incomplete injected goal context",
			line: conversationJSON("user", "input_text", "<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal."),
		},
		{
			name: "user input beside injected goal context",
			line: `{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Continue the requested fix."},{"type":"input_text","text":"<codex_internal_context source=\"goal\">internal goal</codex_internal_context>"}]}}`,
			role: "user", text: "Continue the requested fix.", ok: true,
		},
		{
			name: "assistant analysis channel",
			line: `{"type":"response_item","payload":{"type":"message","role":"assistant","channel":"analysis","content":[{"type":"output_text","text":"hidden reasoning"}]}}`,
		},
		{
			name: "assistant unknown channel",
			line: `{"type":"response_item","payload":{"type":"message","role":"assistant","channel":"internal","content":[{"type":"output_text","text":"hidden internal output"}]}}`,
		},
		{
			name: "assistant tool recipient",
			line: `{"type":"response_item","payload":{"type":"message","role":"assistant","recipient":"functions.exec","content":[{"type":"output_text","text":"hidden tool request"}]}}`,
		},
		{
			name: "reasoning item",
			line: `{"type":"response_item","payload":{"type":"reasoning","role":"assistant","content":[{"type":"output_text","text":"hidden reasoning"}]}}`,
		},
		{
			name: "system message",
			line: `{"type":"response_item","payload":{"type":"message","role":"system","content":[{"type":"input_text","text":"hidden system"}]}}`,
		},
		{
			name: "developer message",
			line: `{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"hidden developer"}]}}`,
		},
		{
			name: "duplicate event representation",
			line: `{"type":"event_msg","payload":{"type":"user_message","message":"duplicate user text"}}`,
		},
		{
			name: "tool call response item",
			line: `{"type":"response_item","payload":{"type":"function_call","role":"assistant","name":"exec","arguments":"hidden"}}`,
		},
		{
			name: "agents wrapper",
			line: `{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /private/path\n<INSTRUCTIONS>hidden</INSTRUCTIONS>"}]}}`,
		},
		{
			name: "user shell command wrapper",
			line: `{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>cat private-file</user_shell_command>"}]}}`,
		},
		{
			name: "malformed",
			line: `{"type":"response_item"`,
		},
	}
	for index, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			message, ok, _ := parseConversationMessage([]byte(test.line), identity, int64(index*100))
			if ok != test.ok {
				t.Fatalf("ok=%v want=%v message=%#v", ok, test.ok, message)
			}
			if ok && (message.Role != test.role || message.Text != test.text || len(message.ID) != 32) {
				t.Fatalf("message=%#v", message)
			}
		})
	}

	line := []byte(tests[1].line)
	first, _, _ := parseConversationMessage(line, identity, 10)
	repeated, _, _ := parseConversationMessage(line, identity, 10)
	moved, _, _ := parseConversationMessage(line, identity, 11)
	if first.ID != repeated.ID || first.ID == moved.ID {
		t.Fatalf("message IDs are not stable and offset-sensitive: %q %q %q", first.ID, repeated.ID, moved.ID)
	}
}

func TestConversationParserOmitsMessageOverPerMessageLimit(t *testing.T) {
	line := conversationJSON("assistant", "output_text", strings.Repeat("x", conversationMessageSize+1))
	message, ok, omitted := parseConversationMessage([]byte(line), []byte("synthetic-file-id"), 0)
	if ok || !omitted || message.Text != "" {
		t.Fatalf("ok=%v omitted=%v message=%#v", ok, omitted, message)
	}
}

func TestConversationParserFiltersHandoffsWithoutHidingPublicDiscussion(t *testing.T) {
	const handoff = "## Task and constraints\n\nWorkspace: `/example/project`, branch `work/example`. Preserve unrelated changes.\n\n## Next steps\nContinue the implementation."
	const continuation = "Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Here is the summary produced by the other language model:\n" + handoff
	tests := []struct {
		name, role, text string
		visible          bool
	}{
		{"assistant handoff", "assistant", handoff, false},
		{"handoff whitespace and CRLF", "assistant", " \n" + strings.ReplaceAll(handoff, "\n", "\r\n"), false},
		{"assistant continuation", "assistant", continuation, false},
		{"injected user continuation", "user", continuation, false},
		{"user task specification", "user", handoff, true},
		{"normal task heading", "assistant", "## Task and constraints\n\nThe requested fix is complete.", true},
		{"normal workspace answer", "assistant", "Workspace: `/example/project`\nThe tests passed.", true},
		{"quoted handoff", "assistant", "This is the internal summary format:\n\n```text\n" + handoff + "\n```", true},
		{"user reports handoff", "user", "Please hide this:\n" + continuation, true},
		{"ordinary assistant answer", "assistant", "The change is complete.", true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			contentType := "output_text"
			if test.role == "user" {
				contentType = "input_text"
			}
			line := conversationJSON(test.role, contentType, test.text)
			// Reproduce the observed final-answer shape; unknown content kinds also
			// occur on normal messages and must not be treated as a summary flag.
			var event map[string]any
			if err := json.Unmarshal([]byte(line), &event); err != nil {
				t.Fatal(err)
			}
			payload := event["payload"].(map[string]any)
			payload["phase"] = "final_answer"
			payload["internal_chat_message_metadata_passthrough"] = map[string]any{"content_item_kinds": []string{"unknown"}}
			raw, err := json.Marshal(event)
			if err != nil {
				t.Fatal(err)
			}
			message, ok, omitted := parseConversationMessage(raw, []byte("fixture"), 0)
			if ok != test.visible || omitted || (ok && message.Text != test.text) {
				t.Fatalf("visible=%v want=%v omitted=%v", ok, test.visible, omitted)
			}
		})
	}
}

func TestConversationTailFiltersConfirmedCompactionRegardlessOfHeading(t *testing.T) {
	const summary = "## Active goal and scope\n\nContinue the unfinished work.\n\n## Next actions\nRun the remaining checks."
	compacted := func(message string) string {
		raw, err := json.Marshal(map[string]any{"type": "compacted", "payload": map[string]any{"message": message}})
		if err != nil {
			t.Fatal(err)
		}
		return string(raw)
	}
	envelope := conversationContinuationPrefix + " More continuation instructions.\n" + conversationContinuationSummary + ", use the information in this summary to assist with your own analysis:\n\n" + summary
	for _, test := range []struct {
		name, record string
		hidden       bool
	}{
		{"confirmed summary", compacted(envelope) + "\n", true},
		{"unconfirmed heading", "", false},
		{"partial compaction record", compacted(envelope), false},
		{"ordinary compacted message", compacted(summary) + "\n", false},
		{"malformed record", `{"type":"compacted"` + "\n", false},
		{"oversized record", compacted(envelope+strings.Repeat(" ", conversationLineLimit)) + "\n", true},
	} {
		t.Run(test.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "synthetic.jsonl")
			// Fill the public budget before the summary. Filtering must precede capping.
			var content strings.Builder
			for i := 0; i < conversationMessageMax-1; i++ {
				content.WriteString(conversationJSON("assistant", "output_text", "Public reply "+strconv.Itoa(i)) + "\n")
			}
			// The same text, supplied by a user, is still public.
			content.WriteString(conversationJSON("user", "input_text", summary) + "\n")
			content.WriteString(conversationJSON("assistant", "output_text", summary) + "\n")
			content.WriteString(`{"type":"event_msg","payload":{"type":"token_count"}}` + "\n")
			content.WriteString(test.record)
			if err := os.WriteFile(path, []byte(content.String()), 0o600); err != nil {
				t.Fatal(err)
			}
			file, err := os.Open(path)
			if err != nil {
				t.Fatal(err)
			}
			defer file.Close()
			info, err := file.Stat()
			if err != nil {
				t.Fatal(err)
			}
			messages, truncated, err := readConversationMessages(context.Background(), file, info)
			if err != nil {
				t.Fatal(err)
			}
			if len(messages) != conversationMessageMax {
				t.Fatalf("count=%d", len(messages))
			}
			last := messages[len(messages)-1]
			if test.hidden {
				if (truncated && test.name != "oversized record") || messages[0].Text != "Public reply 0" || last.Role != "user" || last.Text != summary {
					t.Fatal("confirmed summary was not filtered before public budget")
				}
			} else if last.Role != "assistant" || last.Text != summary {
				t.Fatal("unconfirmed assistant reply was hidden")
			}
		})
	}
}

func TestConversationTailSkipsPartialOversizedAndBoundsMessageCount(t *testing.T) {
	path := filepath.Join(t.TempDir(), "synthetic.jsonl")
	var content strings.Builder
	content.WriteString(strings.Repeat("x", conversationLineLimit+1))
	content.WriteByte('\n')
	for index := 0; index < conversationMessageMax+5; index++ {
		content.WriteString(conversationJSON("user", "input_text", "message "+strconv.Itoa(index)))
		content.WriteByte('\n')
	}
	content.WriteString(`{"type":"response_item","payload":{"type":"message"`)
	if err := os.WriteFile(path, []byte(content.String()), 0o600); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		t.Fatal(err)
	}
	messages, truncated, err := readConversationMessages(context.Background(), file, info)
	if err != nil {
		t.Fatal(err)
	}
	if !truncated || len(messages) != conversationMessageMax {
		t.Fatalf("truncated=%v messages=%d", truncated, len(messages))
	}
	if messages[0].Text != "message 5" || messages[len(messages)-1].Text != "message 204" {
		t.Fatalf("unexpected bounded range: first=%q last=%q", messages[0].Text, messages[len(messages)-1].Text)
	}
}

func TestConversationTailReadsOnlyCompleteRecordsFromFourMiBWindow(t *testing.T) {
	path := filepath.Join(t.TempDir(), "synthetic.jsonl")
	valid := conversationJSON("assistant", "output_text", "visible tail") + "\n"
	content := strings.Repeat("p", int(conversationTailLimit)+128) + "\n" + valid
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	info, _ := file.Stat()
	messages, truncated, err := readConversationMessages(context.Background(), file, info)
	if err != nil {
		t.Fatal(err)
	}
	if !truncated || len(messages) != 1 || messages[0].Text != "visible tail" {
		t.Fatalf("truncated=%v messages=%#v", truncated, messages)
	}
}

func TestBoundConversationFileRejectsSymlinkAndEscape(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	realPath := filepath.Join(root, "synthetic.jsonl")
	if err := os.WriteFile(realPath, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	file, _, err := openBoundConversationFile(home, realPath)
	if err != nil {
		t.Fatalf("regular owned file rejected: %v", err)
	}
	file.Close()

	linkPath := filepath.Join(root, "linked.jsonl")
	if err := os.Symlink(realPath, linkPath); err != nil {
		t.Fatal(err)
	}
	if file, _, err := openBoundConversationFile(home, linkPath); err == nil {
		file.Close()
		t.Fatal("symlink source was accepted")
	}
	outside := filepath.Join(t.TempDir(), "outside.jsonl")
	if err := os.WriteFile(outside, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if file, _, err := openBoundConversationFile(home, outside); err == nil {
		file.Close()
		t.Fatal("source outside the sessions root was accepted")
	}
}

func TestNearestCodexProviderRejectsAmbiguousPeer(t *testing.T) {
	nodes := map[int]processNode{
		10: {PID: 10, Process: "zsh"},
		20: {PID: 20, PPID: 10, Process: "codex", Provider: "codex"},
		21: {PID: 21, PPID: 10, Process: "codex", Provider: "codex"},
		30: {PID: 30, PPID: 20, Process: "codex", Provider: "codex"},
	}
	if _, status := nearestSessionProvider(nodes, processChildren(nodes), 10); status != sessionBindingAmbiguous {
		t.Fatalf("status=%v", status)
	}
	delete(nodes, 21)
	pid, status := nearestSessionProvider(nodes, processChildren(nodes), 10)
	if status != sessionBindingReady || pid != 20 {
		t.Fatalf("pid=%d status=%v", pid, status)
	}
}

func TestConversationWrapperChainAllowsOnlyUniqueNonProviderDescendants(t *testing.T) {
	nodes := map[int]processNode{
		20: {PID: 20, Process: "codex", Provider: "codex"},
		21: {PID: 21, PPID: 20, Process: "codex-host"},
		22: {PID: 22, PPID: 21, Process: "worker"},
	}
	chain, status := sessionWrapperChain(nodes, processChildren(nodes), 20)
	if status != sessionBindingReady || len(chain) != 2 || chain[0] != 21 || chain[1] != 22 {
		t.Fatalf("chain=%v status=%v", chain, status)
	}
	nodes[23] = processNode{PID: 23, PPID: 20, Process: "sibling"}
	if _, status := sessionWrapperChain(nodes, processChildren(nodes), 20); status != sessionBindingAmbiguous {
		t.Fatalf("branched status=%v", status)
	}
	delete(nodes, 23)
	nodes[22] = processNode{PID: 22, PPID: 21, Process: "codex", Provider: "codex"}
	if _, status := sessionWrapperChain(nodes, processChildren(nodes), 20); status != sessionBindingAmbiguous {
		t.Fatalf("descendant provider status=%v", status)
	}
}

func TestReadConversationRechecksBindingAndBoundsJSON(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "synthetic.jsonl")
	var content strings.Builder
	largeText := strings.Repeat("v", 4096)
	for index := 0; index < conversationMessageMax; index++ {
		content.WriteString(conversationJSON("assistant", "output_text", largeText+strconv.Itoa(index)))
		content.WriteByte('\n')
	}
	if err := os.WriteFile(path, []byte(content.String()), 0o600); err != nil {
		t.Fatal(err)
	}
	runner := &conversationCatalogRunner{createdAt: 1700000000, panePID: 77}
	bindings := 0
	result, err := readConversation(context.Background(), "$3", 1700000000, conversationDependencies{
		runner: runner,
		home:   home,
		binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			bindings++
			return conversationBinding{providerPID: 88, filePID: 88, path: path}, sessionBindingReady
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := json.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != model.ConversationReady || !result.Truncated || len(result.Messages) >= conversationMessageMax ||
		conversationTextBytes(result.Messages) > conversationTextLimit || len(encoded) > conversationEncodedMax ||
		bindings != 2 || runner.catalogReads != 2 {
		t.Fatalf("status=%q truncated=%v messages=%d bytes=%d bindings=%d reads=%d",
			result.Status, result.Truncated, len(result.Messages), len(encoded), bindings, runner.catalogReads)
	}
}

func TestReadConversationReturnsNoContentWhenBindingChanges(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "synthetic.jsonl")
	if err := os.WriteFile(path, []byte(conversationJSON("assistant", "output_text", "must be discarded")+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	calls := 0
	result, err := readConversation(context.Background(), "$4", 1700000001, conversationDependencies{
		runner: &conversationCatalogRunner{createdAt: 1700000001, panePID: 90},
		home:   home,
		binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			calls++
			return conversationBinding{providerPID: 100 + calls, filePID: 100 + calls, path: path}, sessionBindingReady
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != model.ConversationAmbiguous || len(result.Messages) != 0 {
		t.Fatalf("result=%#v", result)
	}
}

func TestReadConversationReturnsNoContentWhenSessionIdentityChanges(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "synthetic.jsonl")
	if err := os.WriteFile(path, []byte(conversationJSON("assistant", "output_text", "must be discarded")+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	runner := &conversationCatalogRunner{createdAt: 1700000000, panePID: 77, changeIdentity: true}
	result, err := readConversation(context.Background(), "$3", 1700000000, conversationDependencies{
		runner: runner,
		home:   home,
		binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			return conversationBinding{providerPID: 88, filePID: 88, path: path}, sessionBindingReady
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != model.ConversationUnavailable || len(result.Messages) != 0 || runner.catalogReads != 2 {
		t.Fatalf("result=%#v reads=%d", result, runner.catalogReads)
	}
}

func TestReadConversationReturnsAmbiguousWithoutOpeningSource(t *testing.T) {
	result, err := readConversation(context.Background(), "$5", 1700000002, conversationDependencies{
		runner: &conversationCatalogRunner{createdAt: 1700000002, panePID: 91},
		home:   t.TempDir(),
		binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			return conversationBinding{}, sessionBindingAmbiguous
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != model.ConversationAmbiguous || len(result.Messages) != 0 {
		t.Fatalf("result=%#v", result)
	}
}

func TestReadConversationHonorsCanceledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := readConversation(ctx, "$6", 1700000003, conversationDependencies{
		runner: &conversationCatalogRunner{createdAt: 1700000003, panePID: 92},
		home:   t.TempDir(),
		binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			t.Fatal("binding must not run after cancellation")
			return conversationBinding{}, sessionBindingUnavailable
		},
	})
	if err != context.Canceled {
		t.Fatalf("error=%v", err)
	}
}

func conversationJSON(role, contentType, text string) string {
	value := map[string]any{
		"type": "response_item",
		"payload": map[string]any{
			"type":    "message",
			"role":    role,
			"content": []map[string]string{{"type": contentType, "text": text}},
		},
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return string(encoded)
}

type conversationCatalogRunner struct {
	createdAt      int64
	panePID        int
	catalogReads   int
	changeIdentity bool
}

func (runner *conversationCatalogRunner) Output(_ context.Context, args ...string) ([]byte, error) {
	switch args[0] {
	case "list-sessions":
		runner.catalogReads++
		createdAt := runner.createdAt
		if runner.changeIdentity && runner.catalogReads > 1 {
			createdAt++
		}
		fields := []string{"$3", "synthetic", strconv.FormatInt(createdAt, 10), "1700000100", "0", "1", "", "0"}
		if runner.createdAt == 1700000001 {
			fields[0] = "$4"
		} else if runner.createdAt == 1700000002 {
			fields[0] = "$5"
		} else if runner.createdAt == 1700000003 {
			fields[0] = "$6"
		}
		return []byte(strings.Join(fields, separator) + "\n"), nil
	case "list-windows":
		fields := []string{sessionIDForCreatedAt(runner.createdAt), "main", "1", "/synthetic", "codex", "120", "40", strconv.Itoa(runner.panePID)}
		return []byte(strings.Join(fields, separator) + "\n"), nil
	default:
		return nil, nil
	}
}

func sessionIDForCreatedAt(createdAt int64) string {
	switch createdAt {
	case 1700000001:
		return "$4"
	case 1700000002:
		return "$5"
	case 1700000003:
		return "$6"
	default:
		return "$3"
	}
}

func TestReadConversationRejectsOriginalActivePaneSwitch(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "synthetic.jsonl")
	if err := os.WriteFile(path, []byte("{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"synthetic\"}]}}\n"), 0600); err != nil {
		t.Fatal(err)
	}
	runner := &conversationCatalogRunner{createdAt: 1700000000, panePID: 77}
	result, err := readConversation(context.Background(), "$3", 1700000000, conversationDependencies{
		runner: runner, home: home, binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
			runner.panePID = 99
			return conversationBinding{providerPID: 88, filePID: 88, path: path}, sessionBindingReady
		},
	})
	if err != nil || result.Status != model.ConversationUnavailable || len(result.Messages) != 0 {
		t.Fatal("active pane switch returned stale conversation")
	}

}

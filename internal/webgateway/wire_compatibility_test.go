package webgateway

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type wireFixture struct {
	Name       string `json:"name"`
	Input      string `json:"input"`
	GoAccept   bool   `json:"go_accept"`
	RustAccept bool   `json:"rust_accept"`
	Normalized string `json:"normalized,omitempty"`
	Delta      string `json:"delta,omitempty"`
}

// Synthetic protocol oracle. Changes to Go decoder behavior must deliberately
// update this checked-in corpus; no personal configuration or transcript is read.
func TestWireV1CompatibilityCorpus(t *testing.T) {
	cases := []wireFixture{
		{Name: "zero", Input: `{}`, RustAccept: true},
		{Name: "hello", Input: `{"type":"hello","capabilities":["terminal-output-flow-v1","web-upload-v1"]}`, RustAccept: true},
		{Name: "open", Input: `{"type":"open","id":"synthetic-view","session":{"id":"$7","created_at":42},"cols":80,"rows":24}`, RustAccept: true},
		{Name: "data", Input: `{"type":"data","id":"synthetic-view","data":"7ZWc6riAABtbMG0="}`, RustAccept: true},
		{Name: "bytes-array", Input: `{"type":"data","data":[0,1,255,null]}`, RustAccept: true},
		{Name: "base64-newline", Input: `{"type":"data","data":"AQ\r\nID"}`, RustAccept: true},
		{Name: "base64-trailing-bits", Input: `{"type":"data","data":"Zh=="}`, RustAccept: true},
		{Name: "base64-no-padding", Input: `{"type":"data","data":"AQ"}`},
		{Name: "base64-url", Input: `{"type":"data","data":"-_=="}`},
		{Name: "byte-overflow", Input: `{"type":"data","data":[256]}`},
		{Name: "null-fields", Input: `{"type":null,"id":null,"session":null,"header":null,"data":null,"cols":null,"capabilities":null,"payload":null}`, RustAccept: true},
		{Name: "payload-large-integer", Input: `{"type":"response","payload":{"value":9007199254740993,"opaque":{"anything":true}}}`, RustAccept: true},
		{Name: "payload-whitespace", Input: `{ "payload" : { "z" : 9007199254740993, "a" : " a \" b " } }`, RustAccept: true},
		{Name: "payload-array", Input: `{"type":"response","payload":[false,null,1]}`, RustAccept: true},
		{Name: "html-escaping", Input: `{"type":"response","payload":{"text":"<script>&</script>"}}`, RustAccept: true},
		{Name: "upload-header", Input: `{"type":"upload-start","id":"0123456789abcdef0123456789abcdef","header":{"protocol_version":1,"request_id":"0123456789abcdef0123456789abcdef","session":{"id":"$2","created_at":42},"file_count":1,"total_bytes":1,"files":[{"index":0,"size":1,"extension":"txt"}]}}`, RustAccept: true},
		{Name: "empty-header-not-authorized-by-codec", Input: `{"type":"upload-start","header":{}}`, RustAccept: true},
		{Name: "session-limits-owned-by-operation", Input: `{"type":"request","session":{"id":"$123456789012345","created_at":-1}}`, RustAccept: true},
		{Name: "integer-max", Input: `{"session":{"created_at":9223372036854775807},"cols":65535,"received":-9223372036854775808}`, RustAccept: true},
		{Name: "integer-overflow", Input: `{"session":{"created_at":9223372036854775808}}`},
		{Name: "u16-overflow", Input: `{"cols":65536}`},
		{Name: "u16-negative", Input: `{"cols":-1}`},
		{Name: "float-integer", Input: `{"cols":1.0}`},
		{Name: "exponent-integer", Input: `{"cols":1e0}`},
		{Name: "string-integer", Input: `{"cols":"1"}`},
		{Name: "unknown-field", Input: `{"type":"hello","unknown":true}`},
		{Name: "unknown-session-field", Input: `{"session":{"id":"$1","extra":true}}`},
		{Name: "unknown-header-field", Input: `{"header":{"extra":true}}`},
		{Name: "unknown-file-field", Input: `{"header":{"files":[{"name":"private.txt"}]}}`},
		{Name: "trailing-object", Input: `{} {}`},
		{Name: "trailing-null", Input: `{} null`},
		{Name: "whitespace", Input: " \n {\"type\":\"hello\"} \t\r\n", RustAccept: true},
		{Name: "top-array", Input: `[]`},
		{Name: "top-string", Input: `"hello"`},
		{Name: "truncated", Input: `{"type":`},
		{Name: "session-array", Input: `{"session":[]}`},
		{Name: "header-array", Input: `{"header":[]}`},
		{Name: "file-array", Input: `{"header":{"files":[[]]}}`},
		{Name: "file-null", Input: `{"header":{"files":[null]}}`, RustAccept: true},

		{Name: "top-null", Input: `null`, Delta: "Rust requires an envelope object"},
		{Name: "case-variant", Input: `{"TYPE":"open","Id":"x","SESSION":{"ID":"$1","CREATED_AT":42}}`, Delta: "Rust requires canonical field spelling"},
		{Name: "duplicate-scalar", Input: `{"type":"a","type":"b"}`, Delta: "Rust rejects duplicate envelope fields"},
		{Name: "duplicate-session-merge", Input: `{"session":{"id":"$1"},"session":{"created_at":42}}`, Delta: "Rust rejects duplicate envelope fields"},
		{Name: "id-max", Input: `{"id":"` + strings.Repeat("a", 64) + `"}`, RustAccept: true},
		{Name: "id-over", Input: `{"id":"` + strings.Repeat("a", 65) + `"}`},
		{Name: "capability-empty", Input: `{"capabilities":[""]}`},
		{Name: "capability-bytes", Input: `{"capabilities":["` + strings.Repeat("한", 22) + `"]}`},
	}
	for i := range cases {
		value, err := decodeMessage([]byte(cases[i].Input))
		cases[i].GoAccept = err == nil
		if err == nil {
			raw, err := json.Marshal(value)
			if err != nil {
				t.Fatal(err)
			}
			cases[i].Normalized = string(raw)
		}
		if cases[i].RustAccept && !cases[i].GoAccept {
			t.Fatalf("invalid expected parity: %s", cases[i].Name)
		}
		if cases[i].GoAccept != cases[i].RustAccept && cases[i].Delta == "" {
			t.Fatalf("undocumented difference: %s", cases[i].Name)
		}
	}
	path := filepath.Join("..", "..", "tests", "fixtures", "wire-v1", "messages.json")
	if os.Getenv("HMUX_REGENERATE_WIRE_FIXTURES") == "1" {
		raw, err := json.MarshalIndent(cases, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(path, append(raw, '\n'), 0644); err != nil {
			t.Fatal(err)
		}
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var expected []wireFixture
	if err = json.Unmarshal(raw, &expected); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(cases, expected) {
		t.Fatal("wire fixture drift: regenerate only after compatibility review")
	}
	// Rust-generated output is fed back into the real Go decoder in rust-compat.
	if dir := os.Getenv("HMUX_RUST_WIRE_OUTPUT"); dir != "" {
		for _, c := range cases {
			if !c.RustAccept {
				continue
			}
			encoded, err := os.ReadFile(filepath.Join(dir, c.Name+".json"))
			if err != nil {
				t.Fatal(err)
			}
			decoded, err := decodeMessage(encoded)
			if err != nil {
				t.Fatalf("Rust %s rejected: %v", c.Name, err)
			}
			normalized, err := json.Marshal(decoded)
			if err != nil || string(normalized) != c.Normalized {
				t.Fatalf("Rust %s changed semantics", c.Name)
			}
		}
	}
}

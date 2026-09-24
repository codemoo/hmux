package webgateway

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"
	"time"
)

// Synthetic public files only. Exercise the actual Go gateway/file server;
// random MIME boundaries are normalized, but their original lengths are kept.
func TestRustStaticOracle(t *testing.T) {
	files := map[string]string{
		"index.html":    "<!doctype html><title>HMux fixture</title>",
		"manifest.json": `{"name":"HMux fixture"}`,
		"sw.js":         "// fixture worker\n", "assets/app-test.js": "// fixture app\n",
		"assets/app-test.css": "body{margin:0}\n", "fonts/NOTICE.txt": "Synthetic font notice\n",
		"fonts/THIRD_PARTY_NOTICES.md": "# Synthetic notices\n",
		"fonts/test.woff2":             "wOF2fixture", "folder/index.html": "nested", "empty.txt": "",
		"range.txt": "0123456789abcdefghijklmnopqrstuvwxyz",
	}
	modified := time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)
	fsys := fstest.MapFS{}
	for name, content := range files {
		fsys[name] = &fstest.MapFile{Data: []byte(content), ModTime: modified}
	}
	s := &Server{host: "hmux.example", assets: http.FileServer(http.FS(fsys))}
	type sample struct {
		Method   string            `json:"method"`
		Path     string            `json:"path"`
		Headers  map[string]string `json:"headers"`
		Status   int               `json:"status"`
		Response map[string]string `json:"response"`
		Body     string            `json:"body"`
	}
	var cases []sample
	add := func(method, path string, headers map[string]string) {
		req := httptest.NewRequest(method, "https://hmux.example"+path, nil)
		for key, value := range headers {
			req.Header.Set(key, value)
		}
		res := httptest.NewRecorder()
		s.ServeHTTP(res, req)
		selected := map[string]string{}
		for _, name := range []string{"Content-Type", "Content-Length", "Content-Range", "Last-Modified", "Accept-Ranges", "Location", "Cache-Control", "X-Content-Type-Options", "Content-Security-Policy", "Referrer-Policy", "Permissions-Policy"} {
			if value := res.Header().Get(name); value != "" {
				selected[name] = value
			}
		}
		// macOS sniffs Markdown notices as text/plain; Linux MIME databases may
		// advertise text/markdown. The candidate explicitly publishes notices as
		// plain text. Normalize only this documented platform-dependent header.
		if strings.HasSuffix(path, ".md") && selected["Content-Type"] == "text/markdown; charset=utf-8" {
			selected["Content-Type"] = "text/plain; charset=utf-8"
		}
		body := res.Body.String()
		if contentType := selected["Content-Type"]; strings.HasPrefix(contentType, "multipart/byteranges; boundary=") {
			boundary := strings.TrimPrefix(contentType, "multipart/byteranges; boundary=")
			selected["Content-Type"] = "multipart/byteranges; boundary=BOUNDARY"
			body = strings.ReplaceAll(body, boundary, "BOUNDARY")
		}
		cases = append(cases, sample{method, path, headers, res.Code, selected, body})
	}
	for _, path := range []string{"/", "/manifest.json", "/sw.js", "/assets/app-test.js", "/assets/app-test.css", "/fonts/NOTICE.txt", "/fonts/THIRD_PARTY_NOTICES.md", "/fonts/test.woff2", "/folder/", "/empty.txt", "/range.txt", "/missing", "/index.html?x=1", "/folder?x=1", "/range.txt/?x=1"} {
		add("GET", path, nil)
		add("HEAD", path, nil)
	}
	for _, rangeValue := range []string{"bytes=0-3", "bytes=5-", "bytes=-4", "bytes=-0", "bytes=--0", "bytes=0-1,4-5", "bytes=0-35,0-35", "bytes=99-", "bytes=4-2", "nibbles=0-1", "bytes=", "bytes= 0 - 3 ", "bytes=99-bad,1-2", "bytes=-1-2", "bytes=9223372036854775808-"} {
		add("GET", "/range.txt", map[string]string{"Range": rangeValue})
	}
	for _, rangeValue := range []string{"bytes=0-", "bytes=-0", "bytes=-4", "nibbles=0-1"} {
		add("GET", "/empty.txt", map[string]string{"Range": rangeValue})
	}
	old := modified.Add(-time.Hour).Format(http.TimeFormat)
	current := modified.Format(http.TimeFormat)
	future := modified.Add(time.Hour).Format(http.TimeFormat)
	for _, headers := range []map[string]string{
		{"If-Match": "*"}, {"If-Match": `"absent"`}, {"If-Match": `W/"absent", *`},
		{"If-None-Match": "*"}, {"If-None-Match": `"absent"`, "If-Modified-Since": future},
		{"If-Modified-Since": current}, {"If-Modified-Since": old}, {"If-Modified-Since": "invalid"},
		{"If-Unmodified-Since": old}, {"If-Unmodified-Since": future}, {"If-Match": "*", "If-Unmodified-Since": old},
		{"Range": "bytes=0-3", "If-Range": current}, {"Range": "bytes=0-3", "If-Range": old}, {"Range": "bytes=0-3", "If-Range": `"absent"`},
	} {
		add("GET", "/range.txt", headers)
	}
	raw, err := json.MarshalIndent(map[string]any{"files": files, "modified": modified.Unix(), "cases": cases}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "static-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_STATIC_FIXTURE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go static oracle changed")
	}
}

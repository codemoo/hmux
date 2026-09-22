package filestage

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

var testSession = SessionIdentity{ID: "$7", CreatedAt: 1_700_000_000}

const testRequestID = "00112233445566778899aabbccddeeff"

func TestRoundTripPreservesBytesAndUsesOpaquePrivateNames(t *testing.T) {
	firstData := []byte("\x89PNG\r\n\x1a\nopaque-image-data")
	secondData := []byte("plain opaque bytes")
	header := Header{ProtocolVersion: 1, RequestID: testRequestID, Session: testSession, FileCount: 2, TotalBytes: int64(len(firstData) + len(secondData)), Files: []FileHeader{{Index: 0, Size: int64(len(firstData)), Extension: "png"}, {Index: 1, Size: int64(len(secondData))}}}
	hashes := []string{}
	for _, data := range [][]byte{firstData, secondData} {
		sum := sha256.Sum256(data)
		hashes = append(hashes, hex.EncodeToString(sum[:]))
	}
	request := bytes.NewBuffer(encodeRequest(t, header, append(append([]byte{}, firstData...), secondData...)))
	root := testRoot(t)
	var responseData bytes.Buffer
	verifyCalls := 0
	verify := func(_ context.Context, identity SessionIdentity) error {
		verifyCalls++
		if identity != testSession {
			return errors.New("wrong identity")
		}
		return nil
	}
	now := time.Unix(1_700_000_100, 0)
	if err := Receive(context.Background(), root, request, &responseData, verify, now); err != nil {
		t.Fatal(err)
	}
	if verifyCalls != 2 {
		t.Fatalf("session verification calls=%d, want 2", verifyCalls)
	}
	response, err := DecodeResponse(responseData.Bytes())
	if err != nil {
		t.Fatal(err)
	}
	if err := ValidateResponseForHeader(response, header, hashes); err != nil {
		t.Fatal(err)
	}
	if got, want := filepath.Base(response.Files[0].Path), "file-0001.png"; got != want {
		t.Fatalf("first staged name=%q want %q", got, want)
	}
	if got, want := filepath.Base(response.Files[1].Path), "file-0002"; got != want {
		t.Fatalf("second staged name=%q want %q", got, want)
	}
	for index, want := range [][]byte{firstData, secondData} {
		got, readErr := os.ReadFile(response.Files[index].Path)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("file %d bytes changed", index+1)
		}
		assertMode(t, response.Files[index].Path, 0o600)
	}
	stageDirectory := filepath.Dir(response.Files[0].Path)
	assertMode(t, root, 0o700)
	assertMode(t, stageDirectory, 0o700)
	assertMode(t, filepath.Join(stageDirectory, "manifest.json"), 0o600)
}

func TestProtocolRejectsMalformedHeadersAndBodies(t *testing.T) {
	validHeader := Header{
		ProtocolVersion: ProtocolVersion,
		RequestID:       testRequestID,
		Session:         testSession,
		FileCount:       1,
		TotalBytes:      1,
		Files:           []FileHeader{{Index: 0, Size: 1, Extension: "png"}},
	}
	tests := []struct {
		name string
		data func() []byte
	}{
		{"bad magic", func() []byte { value := encodeRequest(t, validHeader, []byte("x")); value[0] = 'X'; return value }},
		{"zero header length", func() []byte { return append(append([]byte{}, magic...), 0, 0, 0, 0) }},
		{"oversized header length", func() []byte {
			value := append([]byte{}, magic...)
			var length [4]byte
			binary.BigEndian.PutUint32(length[:], MaximumHeaderBytes+1)
			return append(value, length[:]...)
		}},
		{"truncated header", func() []byte {
			value := append([]byte{}, magic...)
			var length [4]byte
			binary.BigEndian.PutUint32(length[:], 10)
			return append(value, append(length[:], []byte("{}")...)...)
		}},
		{"unknown field", func() []byte {
			return encodeRawRequest(t, `{"protocol_version":1,"request_id":"00112233445566778899aabbccddeeff","session":{"id":"$7","created_at":1700000000},"file_count":1,"total_bytes":1,"files":[{"index":0,"size":1}],"unknown":true}`, []byte("x"))
		}},
		{"trailing header json", func() []byte { return encodeRawRequest(t, string(mustJSON(t, validHeader))+` {}`, []byte("x")) }},
		{"wrong version", func() []byte {
			header := validHeader
			header.ProtocolVersion++
			return encodeRequest(t, header, []byte("x"))
		}},
		{"short body", func() []byte {
			header := validHeader
			header.TotalBytes = 2
			header.Files[0].Size = 2
			return encodeRequest(t, header, []byte("x"))
		}},
		{"extra body", func() []byte { return encodeRequest(t, validHeader, []byte("xy")) }},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			verifyCalls := 0
			err := Receive(context.Background(), testRoot(t), bytes.NewReader(test.data()), io.Discard, func(context.Context, SessionIdentity) error {
				verifyCalls++
				return nil
			}, time.Unix(1_700_000_100, 0))
			if err == nil {
				t.Fatal("malformed request was accepted")
			}
			if test.name == "bad magic" || test.name == "zero header length" || test.name == "oversized header length" || test.name == "truncated header" || test.name == "unknown field" || test.name == "trailing header json" || test.name == "wrong version" {
				if verifyCalls != 0 {
					t.Fatalf("verification ran for invalid header: %d", verifyCalls)
				}
			}
		})
	}
}

func TestHeaderRejectsFileAndSizeLimits(t *testing.T) {
	base := Header{ProtocolVersion: 1, RequestID: testRequestID, Session: testSession}
	tests := []struct {
		name   string
		header Header
	}{
		{"seventeen files", func() Header {
			header := base
			header.FileCount = MaximumFiles + 1
			header.TotalBytes = int64(MaximumFiles + 1)
			for index := 0; index < MaximumFiles+1; index++ {
				header.Files = append(header.Files, FileHeader{Index: index, Size: 1})
			}
			return header
		}()},
		{"oversized file", func() Header {
			header := base
			header.FileCount = 1
			header.TotalBytes = MaximumFileBytes + 1
			header.Files = []FileHeader{{Index: 0, Size: MaximumFileBytes + 1}}
			return header
		}()},
		{"aggregate mismatch", func() Header {
			header := base
			header.FileCount = 1
			header.TotalBytes = 2
			header.Files = []FileHeader{{Index: 0, Size: 1}}
			return header
		}()},
		{"unsafe extension", func() Header {
			header := base
			header.FileCount = 1
			header.TotalBytes = 1
			header.Files = []FileHeader{{Index: 0, Size: 1, Extension: "../sh"}}
			return header
		}()},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if err := validateHeader(test.header); err == nil {
				t.Fatal("invalid header was accepted")
			}
		})
	}
}

func TestReceiveSessionChangeBeforeCommitRemovesIncomingStage(t *testing.T) {
	header := Header{
		ProtocolVersion: 1, RequestID: testRequestID, Session: testSession,
		FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1}},
	}
	root := testRoot(t)
	verifyCalls := 0
	err := Receive(context.Background(), root, bytes.NewReader(encodeRequest(t, header, []byte("x"))), io.Discard, func(context.Context, SessionIdentity) error {
		verifyCalls++
		if verifyCalls == 2 {
			return errors.New("session changed")
		}
		return nil
	}, time.Unix(1_700_000_100, 0))
	if err == nil || verifyCalls != 2 {
		t.Fatalf("err=%v verification calls=%d", err, verifyCalls)
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.Name() != ".lock" {
			t.Fatalf("partial stage survived: %q", entry.Name())
		}
	}
}

func TestReceiveCancellationRemovesPartialIncomingStage(t *testing.T) {
	header := Header{
		ProtocolVersion: ProtocolVersion, RequestID: testRequestID, Session: testSession,
		FileCount: 1, TotalBytes: 2, Files: []FileHeader{{Index: 0, Size: 2}},
	}
	root := testRoot(t)
	ctx, cancel := context.WithCancel(context.Background())
	reader, writer := io.Pipe()
	done := make(chan error, 1)
	go func() {
		done <- Receive(ctx, root, reader, io.Discard, func(context.Context, SessionIdentity) error { return nil }, time.Unix(1_700_000_100, 0))
	}()
	if err := WriteHeader(ctx, writer, header); err != nil {
		t.Fatal(err)
	}
	if _, err := writer.Write([]byte{1}); err != nil {
		t.Fatal(err)
	}
	cancel()
	_ = writer.CloseWithError(ctx.Err())
	if err := <-done; err == nil {
		t.Fatal("canceled receive succeeded")
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.Name() != ".lock" {
			t.Fatalf("partial stage survived cancellation: %q", entry.Name())
		}
	}
}

func TestReceiveWithTTLExpiresFromCompletionAndSweepRemovesOnlyExpired(t *testing.T) {
	header := Header{
		ProtocolVersion: ProtocolVersion, RequestID: testRequestID, Session: testSession,
		FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1, Extension: "txt"}},
	}
	root := testRoot(t)
	started := time.Unix(1_700_000_000, 0)
	completed := started.Add(10 * time.Minute)
	times := []time.Time{started, completed}
	now := func() time.Time {
		value := times[0]
		times = times[1:]
		return value
	}
	var output bytes.Buffer
	if err := ReceiveWithTTL(context.Background(), root, bytes.NewReader(encodeRequest(t, header, []byte("x"))), &output,
		func(context.Context, SessionIdentity) error { return nil }, now, 3*time.Hour); err != nil {
		t.Fatal(err)
	}
	response, err := DecodeResponse(output.Bytes())
	if err != nil {
		t.Fatal(err)
	}
	if want := completed.Add(3 * time.Hour).Unix(); response.ExpiresAtUnix != want {
		t.Fatalf("expires=%d want completion+3h=%d", response.ExpiresAtUnix, want)
	}
	if err := SweepExpired(context.Background(), root, completed.Add(3*time.Hour-time.Second)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(response.Files[0].Path); err != nil {
		t.Fatalf("fresh stage removed: %v", err)
	}
	if err := SweepExpired(context.Background(), root, completed.Add(3*time.Hour)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(response.Files[0].Path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("expired stage survived: %v", err)
	}

	unsafe := filepath.Join(root, "unrecognized")
	if err := os.Mkdir(unsafe, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := SweepExpired(context.Background(), root, completed.Add(24*time.Hour)); err == nil {
		t.Fatal("unsafe spool entry was accepted")
	}
	if _, err := os.Stat(unsafe); err != nil {
		t.Fatalf("unsafe entry was touched: %v", err)
	}
}

func TestCleanupExpiresOnlyRecognizedPrivateStageDirectories(t *testing.T) {
	root := testRoot(t)
	if err := ensureStageRoot(root); err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_700_000_100, 0)
	expired := fmt.Sprintf("%010d-%s", now.Unix()-1, "00112233445566778899aabbccddeeff")
	fresh := fmt.Sprintf("%010d-%s", now.Unix()+1000, "11112222333344445555666677778888")
	incoming := ".incoming-99990000111122223333444455556666"
	for _, name := range []string{expired, fresh, incoming} {
		path := filepath.Join(root, name)
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	stale := now.Add(-IncomingTTL - time.Second)
	if err := os.Chtimes(filepath.Join(root, incoming), stale, stale); err != nil {
		t.Fatal(err)
	}
	if err := cleanupAndCheckQuota(root, now, 1); err != nil {
		t.Fatal(err)
	}
	for _, removed := range []string{expired, incoming} {
		if _, err := os.Lstat(filepath.Join(root, removed)); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("expired stage %q survived: %v", removed, err)
		}
	}
	if _, err := os.Lstat(filepath.Join(root, fresh)); err != nil {
		t.Fatalf("fresh stage was removed: %v", err)
	}

	unsafe := filepath.Join(root, "unknown")
	if err := os.Mkdir(unsafe, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := cleanupAndCheckQuota(root, now, 1); err == nil {
		t.Fatal("unknown spool entry was accepted")
	}
	if _, err := os.Lstat(unsafe); err != nil {
		t.Fatalf("unknown entry was unexpectedly removed: %v", err)
	}
}

func TestReceiveQuotaReservesMaximumManifestBytes(t *testing.T) {
	now := time.Unix(1_700_000_100, 0)
	header := Header{
		ProtocolVersion: ProtocolVersion, RequestID: testRequestID, Session: testSession,
		FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1}},
	}
	verify := func(context.Context, SessionIdentity) error { return nil }
	setupUsedBytes := func(t *testing.T, used int64) string {
		t.Helper()
		root := testRoot(t)
		if err := ensureStageRoot(root); err != nil {
			t.Fatal(err)
		}
		name := fmt.Sprintf("%010d-%s", now.Unix()+1000, "11112222333344445555666677778888")
		directory := filepath.Join(root, name)
		if err := os.Mkdir(directory, 0o700); err != nil {
			t.Fatal(err)
		}
		file, err := os.OpenFile(filepath.Join(directory, "file-0001"), os.O_CREATE|os.O_WRONLY, 0o600)
		if err != nil {
			t.Fatal(err)
		}
		if err := file.Truncate(used); err != nil {
			_ = file.Close()
			t.Fatal(err)
		}
		if err := file.Close(); err != nil {
			t.Fatal(err)
		}
		return root
	}

	exactRoot := setupUsedBytes(t, MaximumSpoolBytes-MaximumManifestBytes-header.TotalBytes)
	if err := Receive(context.Background(), exactRoot, bytes.NewReader(encodeRequest(t, header, []byte("x"))), io.Discard, verify, now); err != nil {
		t.Fatalf("exact quota boundary was rejected: %v", err)
	}

	overRoot := setupUsedBytes(t, MaximumSpoolBytes-MaximumManifestBytes-header.TotalBytes+1)
	if err := Receive(context.Background(), overRoot, bytes.NewReader(encodeRequest(t, header, []byte("x"))), io.Discard, verify, now); err == nil {
		t.Fatal("request exceeding quota by its manifest reservation was accepted")
	}
}

func TestReceiveRejectsUnsafeSpoolModesAndSymlinks(t *testing.T) {
	now := time.Unix(1_700_000_100, 0)
	header := Header{ProtocolVersion: 1, RequestID: testRequestID, Session: testSession, FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1}}}
	for _, setup := range []func(string){
		func(root string) {
			if err := os.MkdirAll(root, 0o700); err != nil {
				t.Fatal(err)
			}
			if err := os.Chmod(root, 0o777); err != nil {
				t.Fatal(err)
			}
		},
		func(root string) {
			parent := filepath.Dir(root)
			if err := os.MkdirAll(parent, 0o700); err != nil {
				t.Fatal(err)
			}
			target := filepath.Join(t.TempDir(), "target")
			if err := os.Mkdir(target, 0o700); err != nil {
				t.Fatal(err)
			}
			if err := os.Symlink(target, root); err != nil {
				t.Fatal(err)
			}
		},
	} {
		root := filepath.Join(t.TempDir(), "hmux", "staged-files-v1")
		setup(root)
		err := Receive(context.Background(), root, bytes.NewReader(encodeRequest(t, header, []byte("x"))), io.Discard, func(context.Context, SessionIdentity) error { return nil }, now)
		if err == nil {
			t.Fatal("unsafe spool was accepted")
		}
	}
}

func TestConcurrentReceiversCannotRaceStageQuota(t *testing.T) {
	root := testRoot(t)
	if err := ensureStageRoot(root); err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_700_000_100, 0)
	for index := 0; index < MaximumStages-1; index++ {
		name := fmt.Sprintf("%010d-%032x", now.Unix()+1000, index+1)
		if err := os.Mkdir(filepath.Join(root, name), 0o700); err != nil {
			t.Fatal(err)
		}
	}
	header := Header{ProtocolVersion: 1, RequestID: testRequestID, Session: testSession, FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1}}}
	var start sync.WaitGroup
	start.Add(1)
	results := make(chan error, 2)
	for index := 0; index < 2; index++ {
		go func() {
			start.Wait()
			results <- Receive(context.Background(), root, bytes.NewReader(encodeRequest(t, header, []byte("x"))), io.Discard, func(context.Context, SessionIdentity) error { return nil }, now)
		}()
	}
	start.Done()
	successes := 0
	failures := 0
	for index := 0; index < 2; index++ {
		if err := <-results; err == nil {
			successes++
		} else {
			failures++
		}
	}
	if successes != 1 || failures != 1 {
		t.Fatalf("successes=%d failures=%d", successes, failures)
	}
}

func TestRootLockWaitHonorsContextCancellation(t *testing.T) {
	root := testRoot(t)
	if err := ensureStageRoot(root); err != nil {
		t.Fatal(err)
	}
	first, err := openRootLock(root)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if err := syscall.Flock(int(first.Fd()), syscall.LOCK_EX); err != nil {
		t.Fatal(err)
	}
	defer syscall.Flock(int(first.Fd()), syscall.LOCK_UN) //nolint:errcheck
	second, err := openRootLock(root)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 80*time.Millisecond)
	defer cancel()
	started := time.Now()
	err = acquireRootLock(ctx, second)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("lock cancellation took %v", elapsed)
	}
}

func TestResponseDecoderAndValidatorRejectMaliciousPayloads(t *testing.T) {
	if _, err := DecodeResponse([]byte(`{"protocol_version":1,"unknown":true}`)); err == nil {
		t.Fatal("unknown response field was accepted")
	}
	if _, err := DecodeResponse([]byte(`{} {}`)); err == nil {
		t.Fatal("trailing response JSON was accepted")
	}
	if _, err := DecodeResponse(bytes.Repeat([]byte("x"), MaximumResponseBytes+1)); err == nil {
		t.Fatal("oversized response was accepted")
	}

	header := Header{ProtocolVersion: 1, RequestID: testRequestID, Session: testSession, FileCount: 1, TotalBytes: 1, Files: []FileHeader{{Index: 0, Size: 1, Extension: "txt"}}}
	hash := sha256.Sum256([]byte("x"))
	stageID := "ffeeddccbbaa99887766554433221100"
	expires := int64(1_700_086_400)
	base := Response{
		ProtocolVersion: 1, RequestID: testRequestID, StageID: stageID, Session: testSession,
		ExpiresAtUnix: expires,
		Files:         []StagedFile{{Index: 0, Path: filepath.Join("/Users/test/Library/Caches/hmux/staged-files-v1", fmt.Sprintf("%d-%s", expires, stageID), "file-0001.txt"), Size: 1, SHA256: hex.EncodeToString(hash[:])}},
	}
	if err := ValidateResponseForHeader(base, header, []string{hex.EncodeToString(hash[:])}); err != nil {
		t.Fatalf("valid response rejected: %v", err)
	}
	mutations := []func(*Response){
		func(response *Response) { response.RequestID = "11112222333344445555666677778888" },
		func(response *Response) { response.Session.CreatedAt++ },
		func(response *Response) { response.Files[0].SHA256 = strings.Repeat("0", 64) },
		func(response *Response) { response.Files[0].Size++ },
		func(response *Response) { response.Files[0].Path = "/tmp/file-0001.txt" },
		func(response *Response) {
			response.Files[0].Path = strings.Replace(response.Files[0].Path, "file-0001.txt", "file-0001.txt\n", 1)
		},
	}
	for index, mutate := range mutations {
		value := base
		value.Files = append([]StagedFile(nil), base.Files...)
		mutate(&value)
		if err := ValidateResponseForHeader(value, header, []string{hex.EncodeToString(hash[:])}); err == nil {
			t.Fatalf("malicious mutation %d was accepted", index)
		}
	}
}

func encodeRequest(t *testing.T, header Header, body []byte) []byte {
	t.Helper()
	return encodeRawRequest(t, string(mustJSON(t, header)), body)
}

func encodeRawRequest(t *testing.T, rawHeader string, body []byte) []byte {
	t.Helper()
	var result bytes.Buffer
	result.Write(magic)
	if err := binary.Write(&result, binary.BigEndian, uint32(len(rawHeader))); err != nil {
		t.Fatal(err)
	}
	result.WriteString(rawHeader)
	result.Write(body)
	return result.Bytes()
}

func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func testRoot(t *testing.T) string {
	t.Helper()
	return filepath.Join(t.TempDir(), "hmux", "staged-files-v1")
}

func writeTestFile(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func assertMode(t *testing.T, path string, mode os.FileMode) {
	t.Helper()
	info, err := os.Lstat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != mode {
		t.Fatalf("%s mode=%#o want %#o", path, info.Mode().Perm(), mode)
	}
}

package filestage

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode/utf8"

	"golang.org/x/sys/unix"
)

const (
	ProtocolVersion      = 1
	MaximumFiles         = 16
	MaximumFileBytes     = int64(32 * 1024 * 1024)
	MaximumRequestBytes  = int64(128 * 1024 * 1024)
	MaximumSpoolBytes    = int64(512 * 1024 * 1024)
	MaximumStages        = 100
	MaximumHeaderBytes   = 16 * 1024
	MaximumResponseBytes = 64 * 1024
	MaximumManifestBytes = MaximumResponseBytes + 1
	StageTTL             = 24 * time.Hour
	IncomingTTL          = 10 * time.Minute
)

var (
	magic             = []byte("HMXSTG1\n")
	hexIDPattern      = regexp.MustCompile(`^[0-9a-f]{32}$`)
	shaPattern        = regexp.MustCompile(`^[0-9a-f]{64}$`)
	extensionPattern  = regexp.MustCompile(`^[a-z0-9]{1,16}$`)
	committedPattern  = regexp.MustCompile(`^[0-9]{10}-[0-9a-f]{32}$`)
	incomingPattern   = regexp.MustCompile(`^\.incoming-[0-9a-f]{32}$`)
	stagedFilePattern = regexp.MustCompile(`^file-[0-9]{4}(\.[a-z0-9]{1,16})?$`)
)

type SessionIdentity struct {
	ID        string `json:"id"`
	CreatedAt int64  `json:"created_at"`
}

type FileHeader struct {
	Index     int    `json:"index"`
	Size      int64  `json:"size"`
	Extension string `json:"extension,omitempty"`
}

type Header struct {
	ProtocolVersion int             `json:"protocol_version"`
	RequestID       string          `json:"request_id"`
	Session         SessionIdentity `json:"session"`
	FileCount       int             `json:"file_count"`
	TotalBytes      int64           `json:"total_bytes"`
	Files           []FileHeader    `json:"files"`
}

type StagedFile struct {
	Index  int    `json:"index"`
	Path   string `json:"path"`
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
}

type Response struct {
	ProtocolVersion int             `json:"protocol_version"`
	RequestID       string          `json:"request_id"`
	StageID         string          `json:"stage_id"`
	Session         SessionIdentity `json:"session"`
	ExpiresAtUnix   int64           `json:"expires_at_unix"`
	Files           []StagedFile    `json:"files"`
}

func DefaultRoot() (string, error) {
	cache, err := os.UserCacheDir()
	if err != nil || !filepath.IsAbs(cache) {
		return "", errors.New("HMux staging cache is unavailable")
	}
	return filepath.Join(cache, "hmux", "staged-files-v1"), nil
}

func NewRequestID() (string, error) {
	var value [16]byte
	if _, err := rand.Read(value[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(value[:]), nil
}

func ValidRequestID(value string) bool { return hexIDPattern.MatchString(value) }

// ValidateHeader validates metadata supplied by a streaming client before any
// staging directory is created. File names and source paths are deliberately
// absent from Header.
func ValidateHeader(header Header) error { return validateHeader(header) }

// WriteHeader writes the bounded file-stage prefix without buffering any file
// contents. It is used by the web upload bridge.
func WriteHeader(ctx context.Context, writer io.Writer, header Header) error {
	if writer == nil || validateHeader(header) != nil {
		return errors.New("file-stage header is invalid")
	}
	headerData, err := json.Marshal(header)
	if err != nil || len(headerData) < 1 || len(headerData) > MaximumHeaderBytes {
		return errors.New("file-stage header is invalid")
	}
	var prefix bytes.Buffer
	prefix.Write(magic)
	_ = binary.Write(&prefix, binary.BigEndian, uint32(len(headerData)))
	prefix.Write(headerData)
	return writeAll(ctx, writer, prefix.Bytes())
}

// ValidateResponseForHeader binds a Home response to the gateway-owned
// request/session metadata and to hashes computed while streaming the body.
func ValidateResponseForHeader(response Response, header Header, hashes []string) error {
	if validateHeader(header) != nil || response.ProtocolVersion != ProtocolVersion || response.RequestID != header.RequestID ||
		response.Session != header.Session || !hexIDPattern.MatchString(response.StageID) || response.ExpiresAtUnix < 1 ||
		len(response.Files) != len(header.Files) || len(hashes) != len(header.Files) {
		return errors.New("invalid file-stage response identity")
	}
	stageDir := strconv.FormatInt(response.ExpiresAtUnix, 10) + "-" + response.StageID
	for index, staged := range response.Files {
		metadata := header.Files[index]
		if staged.Index != index || staged.Size != metadata.Size || staged.SHA256 != hashes[index] ||
			!shaPattern.MatchString(staged.SHA256) || !ValidStagedPath(staged.Path, stageDir, index, metadata.Extension) {
			return errors.New("invalid file-stage response payload")
		}
	}
	return nil
}

func ValidStagedPath(path, stageDir string, index int, extension string) bool {
	if !utf8.ValidString(path) || len(path) < 1 || len(path) > 4096 || strings.IndexByte(path, 0) >= 0 ||
		strings.ContainsAny(path, "\r\n\t\x1b") || !filepath.IsAbs(path) || filepath.Clean(path) != path ||
		filepath.Base(filepath.Dir(path)) != stageDir || filepath.Base(filepath.Dir(filepath.Dir(path))) != "staged-files-v1" {
		return false
	}
	name := fmt.Sprintf("file-%04d", index+1)
	if extension != "" {
		name += "." + extension
	}
	return filepath.Base(path) == name && stagedFilePattern.MatchString(name)
}

type VerifySession func(context.Context, SessionIdentity) error

func Receive(ctx context.Context, root string, reader io.Reader, writer io.Writer, verify VerifySession, now time.Time) error {
	return ReceiveWithTTL(ctx, root, reader, writer, verify, func() time.Time { return now }, StageTTL)
}

// ReceiveWithTTL runs the same hardened receiver with a caller-scoped
// retention period. The expiry is measured at commit, after the complete body
// and the second session identity check have succeeded.
func ReceiveWithTTL(ctx context.Context, root string, reader io.Reader, writer io.Writer, verify VerifySession, now func() time.Time, ttl time.Duration) error {
	if reader == nil || writer == nil || verify == nil || now == nil || ttl <= 0 || ttl > StageTTL {
		return errors.New("file-stage receiver is unavailable")
	}
	startedAt := now()
	if startedAt.IsZero() {
		return errors.New("file-stage receiver is unavailable")
	}
	header, err := readHeader(reader)
	if err != nil {
		return err
	}
	if err := verify(ctx, header.Session); err != nil {
		return err
	}
	if err := ensureStageRoot(root); err != nil {
		return err
	}
	lock, err := openRootLock(root)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := acquireRootLock(ctx, lock); err != nil {
		return err
	}
	defer syscall.Flock(int(lock.Fd()), syscall.LOCK_UN) //nolint:errcheck
	reservation := header.TotalBytes + MaximumManifestBytes
	if err := cleanupAndCheckQuota(root, startedAt, reservation); err != nil {
		return err
	}
	stageID, err := NewRequestID()
	if err != nil {
		return errors.New("file-stage identity generation failed")
	}
	incomingName := ".incoming-" + stageID
	incoming := filepath.Join(root, incomingName)
	if err := os.Mkdir(incoming, 0o700); err != nil {
		return errors.New("file-stage directory could not be created")
	}
	committed := false
	defer func() {
		if !committed {
			_ = removeStageChild(root, incomingName)
		}
	}()
	staged := make([]StagedFile, 0, len(header.Files))
	for index, metadata := range header.Files {
		name := fmt.Sprintf("file-%04d", index+1)
		if metadata.Extension != "" {
			name += "." + metadata.Extension
		}
		path := filepath.Join(incoming, name)
		fd, openErr := unix.Open(path, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
		if openErr != nil {
			return fmt.Errorf("file %d could not be staged", index+1)
		}
		file := os.NewFile(uintptr(fd), "hmux-staged-destination")
		hasher := sha256.New()
		written, copyErr := copyExact(ctx, io.MultiWriter(file, hasher), reader, metadata.Size)
		syncErr := file.Sync()
		closeErr := file.Close()
		if copyErr != nil || written != metadata.Size || syncErr != nil || closeErr != nil {
			return fmt.Errorf("file %d staging was interrupted", index+1)
		}
		staged = append(staged, StagedFile{
			Index: index, Size: metadata.Size, SHA256: hex.EncodeToString(hasher.Sum(nil)),
		})
	}
	var extra [1]byte
	count, readErr := reader.Read(extra[:])
	if count != 0 || !errors.Is(readErr, io.EOF) {
		return errors.New("file-stage request contains trailing data")
	}
	if err := verify(ctx, header.Session); err != nil {
		return err
	}
	completedAt := now()
	if completedAt.IsZero() || completedAt.Before(startedAt) {
		return errors.New("file-stage completion time is invalid")
	}
	expires := completedAt.Add(ttl).Unix()
	finalName := strconv.FormatInt(expires, 10) + "-" + stageID
	for index := range staged {
		staged[index].Path = filepath.Join(root, finalName, fileName(header.Files[index], index))
	}
	response := Response{
		ProtocolVersion: ProtocolVersion, RequestID: header.RequestID, StageID: stageID,
		Session: header.Session, ExpiresAtUnix: expires, Files: staged,
	}
	responseData, err := json.Marshal(response)
	if err != nil || len(responseData)+1 > MaximumResponseBytes {
		return errors.New("file-stage response is invalid")
	}
	manifestData, err := json.MarshalIndent(response, "", "  ")
	if err != nil || len(manifestData) > MaximumResponseBytes {
		return errors.New("file-stage manifest is invalid")
	}
	if err := writeExclusiveFile(filepath.Join(incoming, "manifest.json"), append(manifestData, '\n')); err != nil {
		return err
	}
	if err := syncDirectory(incoming); err != nil {
		return errors.New("file-stage directory could not be synchronized")
	}
	finalPath := filepath.Join(root, finalName)
	if err := os.Rename(incoming, finalPath); err != nil {
		return errors.New("file-stage commit failed")
	}
	committed = true
	if err := syncDirectory(root); err != nil {
		_ = removeStageChild(root, finalName)
		return errors.New("file-stage commit could not be synchronized")
	}
	if err := writeAll(ctx, writer, append(responseData, '\n')); err != nil {
		_ = removeStageChild(root, finalName)
		return errors.New("file-stage response could not be delivered")
	}
	return nil
}

// SweepExpired removes only recognized expired stage directories below the
// validated private spool root. Unknown or unsafe entries make the sweep fail
// closed and are never removed.
func SweepExpired(ctx context.Context, root string, now time.Time) error {
	if ctx == nil || now.IsZero() {
		return errors.New("file-stage cleanup is unavailable")
	}
	if err := ensureStageRoot(root); err != nil {
		return err
	}
	lock, err := openRootLock(root)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := acquireRootLock(ctx, lock); err != nil {
		return err
	}
	defer syscall.Flock(int(lock.Fd()), syscall.LOCK_UN) //nolint:errcheck
	return cleanupAndCheckQuota(root, now, 0)
}

func acquireRootLock(ctx context.Context, lock *os.File) error {
	if lock == nil {
		return errors.New("file-stage spool lock is unavailable")
	}
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err == nil {
			return nil
		} else if !errors.Is(err, syscall.EWOULDBLOCK) && !errors.Is(err, syscall.EAGAIN) {
			return errors.New("file-stage spool lock is unavailable")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

func DecodeResponse(data []byte) (Response, error) {
	var response Response
	if len(data) < 1 || len(data) > MaximumResponseBytes {
		return response, errors.New("file-stage response exceeds size limit")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&response); err != nil {
		return response, errors.New("file-stage response is invalid")
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return response, errors.New("file-stage response contains trailing data")
	}
	return response, nil
}

func readHeader(reader io.Reader) (Header, error) {
	var header Header
	prefix := make([]byte, len(magic)+4)
	if _, err := io.ReadFull(reader, prefix); err != nil || !bytes.Equal(prefix[:len(magic)], magic) {
		return header, errors.New("invalid file-stage protocol magic")
	}
	length := binary.BigEndian.Uint32(prefix[len(magic):])
	if length < 1 || length > MaximumHeaderBytes {
		return header, errors.New("invalid file-stage header length")
	}
	data := make([]byte, int(length))
	if _, err := io.ReadFull(reader, data); err != nil {
		return header, errors.New("truncated file-stage header")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&header); err != nil {
		return header, errors.New("invalid file-stage header")
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return header, errors.New("file-stage header contains trailing data")
	}
	if err := validateHeader(header); err != nil {
		return header, err
	}
	return header, nil
}

func validateHeader(header Header) error {
	if header.ProtocolVersion != ProtocolVersion || !ValidRequestID(header.RequestID) || !validSession(header.Session) ||
		header.FileCount < 1 || header.FileCount > MaximumFiles || header.FileCount != len(header.Files) ||
		header.TotalBytes < 1 || header.TotalBytes > MaximumRequestBytes {
		return errors.New("invalid file-stage header fields")
	}
	var total int64
	for index, file := range header.Files {
		if file.Index != index || file.Size < 1 || file.Size > MaximumFileBytes ||
			(file.Extension != "" && !extensionPattern.MatchString(file.Extension)) {
			return errors.New("invalid file-stage file metadata")
		}
		total += file.Size
		if total > MaximumRequestBytes {
			return errors.New("file-stage request exceeds total size limit")
		}
	}
	if total != header.TotalBytes {
		return errors.New("file-stage total size mismatch")
	}
	return nil
}

func validSession(session SessionIdentity) bool {
	if session.CreatedAt < 1 || len(session.ID) < 2 || len(session.ID) > 32 || session.ID[0] != '$' {
		return false
	}
	for _, value := range session.ID[1:] {
		if value < '0' || value > '9' {
			return false
		}
	}
	return true
}

func fileName(metadata FileHeader, index int) string {
	name := fmt.Sprintf("file-%04d", index+1)
	if metadata.Extension != "" {
		name += "." + metadata.Extension
	}
	return name
}

func ensureStageRoot(root string) error {
	root = filepath.Clean(root)
	if !filepath.IsAbs(root) || root == string(os.PathSeparator) || filepath.Base(root) != "staged-files-v1" {
		return errors.New("unsafe file-stage root")
	}
	parent := filepath.Dir(root)
	if filepath.Base(parent) != "hmux" {
		return errors.New("unsafe file-stage parent")
	}
	if err := os.MkdirAll(parent, 0o700); err != nil {
		return errors.New("file-stage parent could not be created")
	}
	if err := validatePrivateDirectory(parent); err != nil {
		return err
	}
	if err := os.Mkdir(root, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return errors.New("file-stage root could not be created")
	}
	return validatePrivateDirectory(root)
}

func validatePrivateDirectory(path string) error {
	info, err := os.Lstat(path)
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() || info.Mode().Perm()&0o077 != 0 {
		return errors.New("file-stage directory must be private and non-symlinked")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("file-stage directory must be owned by the current user")
	}
	return nil
}

func openRootLock(root string) (*os.File, error) {
	path := filepath.Join(root, ".lock")
	fd, err := unix.Open(path, unix.O_RDWR|unix.O_CREAT|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, errors.New("file-stage lock could not be opened")
	}
	file := os.NewFile(uintptr(fd), "hmux-file-stage-lock")
	info, statErr := file.Stat()
	if statErr != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 {
		file.Close()
		return nil, errors.New("file-stage lock is unsafe")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() || stat.Nlink != 1 {
		file.Close()
		return nil, errors.New("file-stage lock ownership is unsafe")
	}
	return file, nil
}

func cleanupAndCheckQuota(root string, now time.Time, incomingReservationBytes int64) error {
	entries, err := os.ReadDir(root)
	if err != nil {
		return errors.New("file-stage spool could not be inspected")
	}
	var bytesUsed int64
	stages := 0
	for _, entry := range entries {
		name := entry.Name()
		if name == ".lock" {
			continue
		}
		path := filepath.Join(root, name)
		info, infoErr := os.Lstat(path)
		if infoErr != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
			return errors.New("file-stage spool contains an unsafe entry")
		}
		if incomingPattern.MatchString(name) {
			if now.Sub(info.ModTime()) >= IncomingTTL {
				if err := removeStageChild(root, name); err != nil {
					return err
				}
				continue
			}
		} else if committedPattern.MatchString(name) {
			expires, parseErr := strconv.ParseInt(strings.SplitN(name, "-", 2)[0], 10, 64)
			if parseErr != nil {
				return errors.New("file-stage spool contains invalid metadata")
			}
			if expires <= now.Unix() {
				if err := removeStageChild(root, name); err != nil {
					return err
				}
				continue
			}
		} else {
			return errors.New("file-stage spool contains an unknown entry")
		}
		stages++
		size, sizeErr := safeDirectorySize(path)
		if sizeErr != nil {
			return sizeErr
		}
		bytesUsed += size
		if bytesUsed > MaximumSpoolBytes {
			return errors.New("file-stage spool quota is exhausted")
		}
	}
	if incomingReservationBytes < 0 || incomingReservationBytes > MaximumSpoolBytes-bytesUsed ||
		incomingReservationBytes > 0 && stages >= MaximumStages {
		return errors.New("file-stage spool quota is exhausted")
	}
	return nil
}

func safeDirectorySize(path string) (int64, error) {
	entries, err := os.ReadDir(path)
	if err != nil || len(entries) > MaximumFiles+1 {
		return 0, errors.New("file-stage directory is invalid")
	}
	var total int64
	for _, entry := range entries {
		info, infoErr := os.Lstat(filepath.Join(path, entry.Name()))
		if infoErr != nil || info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 {
			return 0, errors.New("file-stage directory contains an unsafe file")
		}
		if entry.Name() != "manifest.json" && !stagedFilePattern.MatchString(entry.Name()) {
			return 0, errors.New("file-stage directory contains an unknown file")
		}
		total += info.Size()
	}
	return total, nil
}

func removeStageChild(root, name string) error {
	if !incomingPattern.MatchString(name) && !committedPattern.MatchString(name) {
		return errors.New("refusing unsafe file-stage cleanup")
	}
	path := filepath.Join(root, name)
	if filepath.Dir(path) != root {
		return errors.New("refusing file-stage cleanup outside spool")
	}
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil || info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("refusing unsafe file-stage cleanup target")
	}
	if err := os.RemoveAll(path); err != nil {
		return errors.New("file-stage cleanup failed")
	}
	return nil
}

func writeExclusiveFile(path string, data []byte) error {
	fd, err := unix.Open(path, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		return errors.New("file-stage manifest could not be created")
	}
	file := os.NewFile(uintptr(fd), "hmux-file-stage-manifest")
	if _, err := file.Write(data); err != nil {
		file.Close()
		return errors.New("file-stage manifest could not be written")
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return errors.New("file-stage manifest could not be synchronized")
	}
	if err := file.Close(); err != nil {
		return errors.New("file-stage manifest could not be closed")
	}
	return nil
}

func syncDirectory(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()
	return file.Sync()
}

func copyExact(ctx context.Context, writer io.Writer, reader io.Reader, size int64) (int64, error) {
	limited := &io.LimitedReader{R: reader, N: size}
	buffer := make([]byte, 64*1024)
	var total int64
	for limited.N > 0 {
		select {
		case <-ctx.Done():
			return total, ctx.Err()
		default:
		}
		readSize := len(buffer)
		if int64(readSize) > limited.N {
			readSize = int(limited.N)
		}
		count, readErr := limited.Read(buffer[:readSize])
		if count > 0 {
			written, writeErr := writer.Write(buffer[:count])
			total += int64(written)
			if writeErr != nil {
				return total, writeErr
			}
			if written != count {
				return total, io.ErrShortWrite
			}
		}
		if readErr != nil {
			if errors.Is(readErr, io.EOF) && limited.N == 0 {
				break
			}
			return total, readErr
		}
		if count == 0 {
			return total, io.ErrUnexpectedEOF
		}
	}
	return total, nil
}

func writeAll(ctx context.Context, writer io.Writer, data []byte) error {
	for len(data) > 0 {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		written, err := writer.Write(data)
		if err != nil {
			return err
		}
		if written < 1 {
			return io.ErrShortWrite
		}
		data = data[written:]
	}
	return nil
}

package main

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/safeexec"
)

const (
	appWorkspaceSourceEnvironment = "HMUX_WORKSPACE_SOURCE_KEY"
	appWorkspaceSourceDomain      = "hmux-workspace-source-key-v1"
	appWorkspaceSSHOutputLimit    = 1024 * 1024
	appWorkspaceSSHTimeout        = 3 * time.Second
)

var errAppWorkspaceSource = errors.New("HMux workspace source could not be resolved")

// appWorkspaceSourceKey returns an opaque identity for the exact local or
// effective remote workspace selected by the loaded client configuration.
func appWorkspaceSourceKey(ctx context.Context, cfg config.ClientConfig) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	fields := []string{
		"role", cfg.Role,
		"client_id", cfg.ClientID,
		"home_alias", cfg.HomeAlias,
		"agent_path", cfg.AgentPath,
		"state_dir", cfg.StateDir,
	}
	switch cfg.Role {
	case "home":
		hostname, err := os.Hostname()
		if err != nil || strings.TrimSpace(hostname) == "" || !utf8.ValidString(hostname) {
			return "", errAppWorkspaceSource
		}
		fields = append(fields,
			"local_hostname", hostname,
			"local_uid", strconv.Itoa(os.Getuid()),
		)
	case "remote":
		if !validAppWorkspaceSSHAlias(cfg.HomeAlias) {
			return "", errAppWorkspaceSource
		}
		probeCtx, cancel := context.WithTimeout(ctx, appWorkspaceSSHTimeout)
		defer cancel()
		command := exec.CommandContext(probeCtx, "ssh",
			"-G",
			"-o", "BatchMode=yes",
			"-o", "ForwardAgent=no",
			"-o", "ClearAllForwardings=yes",
			cfg.HomeAlias,
		)
		output, err := safeexec.Output(command, appWorkspaceSSHOutputLimit)
		if err != nil {
			if ctxErr := ctx.Err(); ctxErr != nil {
				return "", ctxErr
			}
			return "", errAppWorkspaceSource
		}
		if err := validateAppWorkspaceSSHOutput(output); err != nil {
			return "", errAppWorkspaceSource
		}
		fields = append(fields, "ssh_g", string(output))
	default:
		return "", errAppWorkspaceSource
	}

	digest := sha256.New()
	writeAppWorkspaceHashField(digest.Write, appWorkspaceSourceDomain)
	for _, field := range fields {
		writeAppWorkspaceHashField(digest.Write, field)
	}
	return hex.EncodeToString(digest.Sum(nil)), nil
}

// requireAppWorkspaceSource requires a binding from an accepted app catalog
// and fails closed if it no longer identifies the current client configuration.
func requireAppWorkspaceSource(ctx context.Context, cfg config.ClientConfig, getenv func(string) string) error {
	if getenv == nil {
		return errors.New("HMUX workspace source environment is unavailable")
	}
	expected := getenv(appWorkspaceSourceEnvironment)
	if expected == "" {
		return errors.New("HMUX workspace source key is required; reopen HMux")
	}
	if !validAppWorkspaceSourceKey(expected) {
		return errors.New("HMUX workspace source key is invalid")
	}
	actual, err := appWorkspaceSourceKey(ctx, cfg)
	if err != nil {
		return err
	}
	if subtle.ConstantTimeCompare([]byte(expected), []byte(actual)) != 1 {
		return errors.New("HMux client configuration changed; reopen HMux")
	}
	return nil
}

func writeAppWorkspaceHashField(write func([]byte) (int, error), value string) {
	var size [8]byte
	binary.BigEndian.PutUint64(size[:], uint64(len(value)))
	_, _ = write(size[:])
	_, _ = write([]byte(value))
}

func validAppWorkspaceSourceKey(value string) bool {
	if len(value) != sha256.Size*2 {
		return false
	}
	for _, character := range value {
		if (character < '0' || character > '9') && (character < 'a' || character > 'f') {
			return false
		}
	}
	return true
}

func validAppWorkspaceSSHAlias(value string) bool {
	if value == "" || len(value) > 128 || value[0] == '-' {
		return false
	}
	for _, character := range value {
		if (character < 'a' || character > 'z') &&
			(character < 'A' || character > 'Z') &&
			(character < '0' || character > '9') &&
			!strings.ContainsRune("._-", character) {
			return false
		}
	}
	return true
}

func validateAppWorkspaceSSHOutput(output []byte) error {
	if len(output) == 0 || len(output) > appWorkspaceSSHOutputLimit || !utf8.Valid(output) ||
		strings.IndexByte(string(output), 0) >= 0 {
		return errAppWorkspaceSource
	}
	values := make(map[string][]string)
	for _, line := range strings.Split(strings.TrimSuffix(string(output), "\n"), "\n") {
		if line == "" || strings.HasSuffix(line, "\r") {
			return errAppWorkspaceSource
		}
		parts := strings.Fields(line)
		if len(parts) < 2 || !validAppWorkspaceSSHKey(parts[0]) {
			return errAppWorkspaceSource
		}
		for _, character := range line {
			if character < 0x20 && character != '\t' {
				return errAppWorkspaceSource
			}
		}
		values[parts[0]] = append(values[parts[0]], strings.Join(parts[1:], " "))
	}
	if !singleNonemptyAppWorkspaceValue(values, "hostname") ||
		!singleNonemptyAppWorkspaceValue(values, "user") ||
		!hasSingleAppWorkspaceValue(values, "batchmode", "yes") ||
		!hasSingleAppWorkspaceValue(values, "forwardagent", "no") ||
		!hasSingleAppWorkspaceValue(values, "clearallforwardings", "yes") {
		return errAppWorkspaceSource
	}
	ports := values["port"]
	if len(ports) != 1 {
		return errAppWorkspaceSource
	}
	port, err := strconv.Atoi(ports[0])
	if err != nil || port < 1 || port > 65535 {
		return errAppWorkspaceSource
	}
	return nil
}

func validAppWorkspaceSSHKey(value string) bool {
	if value == "" || len(value) > 128 {
		return false
	}
	for _, character := range value {
		if (character < 'a' || character > 'z') &&
			(character < '0' || character > '9') && character != '-' {
			return false
		}
	}
	return true
}

func singleNonemptyAppWorkspaceValue(values map[string][]string, key string) bool {
	return len(values[key]) == 1 && strings.TrimSpace(values[key][0]) != ""
}

func hasSingleAppWorkspaceValue(values map[string][]string, key, expected string) bool {
	return len(values[key]) == 1 && values[key][0] == expected
}

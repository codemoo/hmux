package model

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"regexp"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"
)

const (
	SchemaVersion          = 1
	ProtocolVersion        = 1
	MaximumHostMemoryBytes = uint64(1) << 60
)

var stableIDPattern = regexp.MustCompile(`^[a-z][a-z0-9-]{0,62}$`)
var sessionIDPattern = regexp.MustCompile(`^\$[0-9]{1,12}$`)
var sshAliasPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`)
var hostAddressPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._:%-]{0,252}$`)
var userPattern = regexp.MustCompile(`^[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}$`)
var identityPathPattern = regexp.MustCompile(`^~/.ssh/[A-Za-z0-9][A-Za-z0-9._/-]{0,239}$`)

type Inventory struct {
	SchemaVersion int           `toml:"schema_version" json:"schema_version"`
	Revision      string        `toml:"revision" json:"revision"`
	Clients       []Client      `toml:"clients" json:"clients"`
	IdentityRefs  []IdentityRef `toml:"identity_refs" json:"identity_refs"`
	Hosts         []Host        `toml:"hosts" json:"hosts"`
	Profiles      []Profile     `toml:"profiles" json:"profiles"`
}

type Client struct {
	ID        string   `toml:"id" json:"id"`
	Role      string   `toml:"role" json:"role"`
	Hostnames []string `toml:"hostnames" json:"hostnames"`
}

type IdentityRef struct {
	ID   string `toml:"id" json:"id"`
	Path string `toml:"path" json:"path"`
}

type Host struct {
	ID                  string   `toml:"id" json:"id"`
	SSHAlias            string   `toml:"ssh_alias" json:"ssh_alias"`
	Address             string   `toml:"address" json:"address"`
	User                string   `toml:"user" json:"user"`
	Port                int      `toml:"port" json:"port"`
	ProxyJump           string   `toml:"proxy_jump" json:"proxy_jump,omitempty"`
	IdentityRef         string   `toml:"identity_ref" json:"identity_ref"`
	Tags                []string `toml:"tags" json:"tags"`
	ServerAliveInterval int      `toml:"server_alive_interval" json:"server_alive_interval,omitempty"`
	ServerAliveCountMax int      `toml:"server_alive_count_max" json:"server_alive_count_max,omitempty"`
	ConnectTimeout      int      `toml:"connect_timeout" json:"connect_timeout,omitempty"`
}

type Profile struct {
	ID               string   `toml:"id" json:"id"`
	Label            string   `toml:"label" json:"label"`
	DefaultDirectory string   `toml:"default_directory" json:"default_directory"`
	Command          []string `toml:"command" json:"command"`
	Tags             []string `toml:"tags" json:"tags"`
}

type Catalog struct {
	ProtocolVersion int          `json:"protocol_version"`
	GeneratedAt     time.Time    `json:"generated_at"`
	Sessions        []Session    `json:"sessions"`
	OpenTabs        []string     `json:"open_tabs,omitempty"`
	CurrentTabID    string       `json:"current_tab_id,omitempty"`
	HostMetrics     *HostMetrics `json:"host_metrics,omitempty"`
}

// HostMetrics is a bounded snapshot sampled on the Home Mac. It is attached
// only to explicitly negotiated catalog streams so legacy strict decoders do
// not see a field they do not understand.
type HostMetrics struct {
	ObservedAt       time.Time `json:"observed_at"`
	CPUPercent       *float64  `json:"cpu_percent,omitempty"`
	GPUPercent       *float64  `json:"gpu_percent,omitempty"`
	MemoryUsedBytes  *uint64   `json:"memory_used_bytes,omitempty"`
	MemoryTotalBytes *uint64   `json:"memory_total_bytes,omitempty"`
	DiskUsedBytes    *uint64   `json:"disk_used_bytes,omitempty"`
	DiskTotalBytes   *uint64   `json:"disk_total_bytes,omitempty"`
}

// UnmarshalJSON makes this optional observation fail-open without changing
// Catalog decoding. This is deliberately scoped here because Catalog is
// embedded in larger app envelopes whose fields must remain decodable.
func (m *HostMetrics) UnmarshalJSON(data []byte) error {
	type hostMetricsWire HostMetrics
	var wire hostMetricsWire
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&wire); err != nil {
		*m = HostMetrics{}
		return nil
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		*m = HostMetrics{}
		return nil
	}
	value := HostMetrics(wire)
	if err := ValidateHostMetrics(&value); err != nil {
		*m = HostMetrics{}
		return nil
	}
	*m = value
	return nil
}

func ValidateHostMetrics(value *HostMetrics) error {
	if value == nil {
		return nil
	}
	if value.ObservedAt.IsZero() {
		return errors.New("host metrics observed_at is required")
	}
	_, offset := value.ObservedAt.Zone()
	if offset != 0 {
		return errors.New("host metrics observed_at must be UTC")
	}
	if value.CPUPercent == nil && value.GPUPercent == nil &&
		value.MemoryUsedBytes == nil && value.MemoryTotalBytes == nil && value.DiskUsedBytes == nil && value.DiskTotalBytes == nil {
		return errors.New("host metrics must contain an observation")
	}
	for name, percent := range map[string]*float64{
		"cpu_percent": value.CPUPercent,
		"gpu_percent": value.GPUPercent,
	} {
		if percent != nil && (math.IsNaN(*percent) || math.IsInf(*percent, 0) || *percent < 0 || *percent > 100) {
			return fmt.Errorf("host metrics %s must be finite and between 0 and 100", name)
		}
	}
	if (value.MemoryUsedBytes == nil) != (value.MemoryTotalBytes == nil) {
		return errors.New("host metrics memory byte fields must be present together")
	}
	if value.MemoryTotalBytes != nil {
		if *value.MemoryTotalBytes == 0 || *value.MemoryTotalBytes > MaximumHostMemoryBytes {
			return errors.New("host metrics memory_total_bytes is out of bounds")
		}
		if *value.MemoryUsedBytes > *value.MemoryTotalBytes {
			return errors.New("host metrics memory_used_bytes exceeds total")
		}
	}
	if (value.DiskUsedBytes == nil) != (value.DiskTotalBytes == nil) {
		return errors.New("disk byte fields must be present together")
	}
	if value.DiskTotalBytes != nil && (*value.DiskTotalBytes == 0 || *value.DiskTotalBytes > 1<<53 || *value.DiskUsedBytes > *value.DiskTotalBytes) {
		return errors.New("disk byte fields are out of bounds")
	}
	return nil
}

// WorkflowSummary is the bounded, presentation-safe aggregate attached to a
// tmux session. Counts describe the currently active workflow set, or the most
// recently updated workflow when nothing is active.
type WorkflowSummary struct {
	Running         int   `json:"running"`
	WaitingApproval int   `json:"waiting_approval"`
	WaitingInput    int   `json:"waiting_input"`
	Completed       int   `json:"completed"`
	Failed          int   `json:"failed"`
	Interrupted     int   `json:"interrupted"`
	Stale           int   `json:"stale"`
	UpdatedAt       int64 `json:"updated_at"`
}

// Workflow contains only sanitized lifecycle metadata. Provider identifiers
// are one-way hashes; prompt, response, transcript, cwd and tool payloads are
// deliberately absent from this protocol.
type Workflow struct {
	ID        string         `json:"id"`
	Source    string         `json:"source"`
	SessionID string         `json:"session_id,omitempty"`
	TurnID    string         `json:"turn_id,omitempty"`
	Status    string         `json:"status"`
	Model     string         `json:"model,omitempty"`
	StartedAt int64          `json:"started_at"`
	UpdatedAt int64          `json:"updated_at"`
	EndedAt   int64          `json:"ended_at,omitempty"`
	Nodes     []WorkflowNode `json:"nodes"`
}

type WorkflowNode struct {
	ID        string `json:"id"`
	ParentID  string `json:"parent_id,omitempty"`
	Type      string `json:"type"`
	Provider  string `json:"provider"`
	Status    string `json:"status"`
	StartedAt int64  `json:"started_at"`
	UpdatedAt int64  `json:"updated_at"`
	EndedAt   int64  `json:"ended_at,omitempty"`
}

// SessionIdentity identifies one tmux lifetime, including across recovery.
type SessionIdentity struct {
	ID        string `json:"id"`
	CreatedAt int64  `json:"created_at"`
}

type Session struct {
	ID             string           `json:"id"`
	Name           string           `json:"name"`
	Alias          string           `json:"alias,omitempty"`
	Hidden         bool             `json:"hidden,omitempty"`
	CreatedAt      int64            `json:"created_at"`
	RestoredFrom   *SessionIdentity `json:"restored_from,omitempty"`
	ActivityAt     int64            `json:"activity_at"`
	Attached       int              `json:"attached_clients"`
	WindowCount    int              `json:"window_count"`
	WindowNames    []string         `json:"window_names"`
	ActiveWindow   string           `json:"active_window"`
	CurrentPath    string           `json:"current_path"`
	CurrentCommand string           `json:"current_command"`
	Profile        string           `json:"profile,omitempty"`
	Label          string           `json:"label,omitempty"`
	Tags           []string         `json:"tags,omitempty"`
	Kind           string           `json:"kind,omitempty"`
	Runtime        string           `json:"runtime,omitempty"`
	Model          string           `json:"model,omitempty"`
	State          string           `json:"state,omitempty"`
	Process        string           `json:"process,omitempty"`
	WorkingSince   int64            `json:"working_since,omitempty"`
	HostAlias      string           `json:"host_alias,omitempty"`
	Width          int              `json:"width,omitempty"`
	Height         int              `json:"height,omitempty"`
	Workflow       *WorkflowSummary `json:"workflow,omitempty"`
	Workflows      []Workflow       `json:"workflows,omitempty"`
	PanePID        int              `json:"-"`
}

type Manifest struct {
	SchemaVersion     int       `json:"schema_version"`
	Version           string    `json:"version"`
	Platform          string    `json:"platform"`
	Artifact          string    `json:"artifact"`
	SHA256            string    `json:"sha256"`
	Size              int64     `json:"size"`
	PublishedAt       time.Time `json:"published_at"`
	MinProtocol       int       `json:"min_protocol"`
	SignatureType     string    `json:"signature_type,omitempty"`
	Signature         string    `json:"signature,omitempty"`
	ManifestSignature string    `json:"manifest_signature,omitempty"`
	KeyID             string    `json:"key_id,omitempty"`
}

func (i Inventory) Validate() error {
	if i.SchemaVersion != SchemaVersion {
		return fmt.Errorf("schema_version must be %d, got %d", SchemaVersion, i.SchemaVersion)
	}
	if len(i.Clients) == 0 {
		return errors.New("at least one client is required")
	}
	if len(i.Hosts) == 0 {
		return errors.New("at least one host is required")
	}
	if len(i.Profiles) == 0 {
		return errors.New("at least one profile is required")
	}
	if !safeMetadata(i.Revision, 128) {
		return errors.New("revision is required and must be safe")
	}
	seen := map[string]string{}
	checkID := func(kind, id string) error {
		if !stableIDPattern.MatchString(id) {
			return fmt.Errorf("%s id %q is invalid", kind, id)
		}
		if previous, ok := seen[id]; ok {
			return fmt.Errorf("stable id %q is shared by %s and %s", id, previous, kind)
		}
		seen[id] = kind
		return nil
	}
	homeClients := 0
	for _, c := range i.Clients {
		if err := checkID("client", c.ID); err != nil {
			return err
		}
		if c.Role != "home" && c.Role != "remote" {
			return fmt.Errorf("client %q role must be home or remote", c.ID)
		}
		if c.Role == "home" {
			homeClients++
		}
		for _, hostname := range c.Hostnames {
			if !hostAddressPattern.MatchString(hostname) {
				return fmt.Errorf("client %q has invalid hostname", c.ID)
			}
		}
	}
	if homeClients != 1 {
		return fmt.Errorf("inventory must contain exactly one home client, got %d", homeClients)
	}
	identityRefs := map[string]bool{}
	for _, r := range i.IdentityRefs {
		if err := checkID("identity_ref", r.ID); err != nil {
			return err
		}
		if !identityPathPattern.MatchString(r.Path) ||
			strings.Contains(strings.TrimPrefix(r.Path, "~/.ssh/"), "..") {
			return fmt.Errorf("identity_ref %q path must be a safe ~/.ssh/ path", r.ID)
		}
		identityRefs[r.ID] = true
	}
	hostIDs := map[string]bool{}
	aliases := map[string]bool{}
	for _, h := range i.Hosts {
		if err := checkID("host", h.ID); err != nil {
			return err
		}
		if !sshAliasPattern.MatchString(h.SSHAlias) {
			return fmt.Errorf("host %q has invalid ssh_alias", h.ID)
		}
		if aliases[h.SSHAlias] {
			return fmt.Errorf("duplicate ssh_alias %q", h.SSHAlias)
		}
		aliases[h.SSHAlias] = true
		if !hostAddressPattern.MatchString(h.Address) {
			return fmt.Errorf("host %q has invalid address", h.ID)
		}
		if !userPattern.MatchString(h.User) {
			return fmt.Errorf("host %q has invalid user", h.ID)
		}
		if h.Port < 1 || h.Port > 65535 {
			return fmt.Errorf("host %q has invalid port", h.ID)
		}
		if h.ServerAliveInterval < 0 || h.ServerAliveInterval > 86400 {
			return fmt.Errorf("host %q has invalid server_alive_interval", h.ID)
		}
		if h.ServerAliveCountMax < 0 || h.ServerAliveCountMax > 100 {
			return fmt.Errorf("host %q has invalid server_alive_count_max", h.ID)
		}
		if h.ConnectTimeout < 0 || h.ConnectTimeout > 120 {
			return fmt.Errorf("host %q has invalid connect_timeout", h.ID)
		}
		if !identityRefs[h.IdentityRef] {
			return fmt.Errorf("host %q references unknown identity_ref %q", h.ID, h.IdentityRef)
		}
		if len(h.Tags) > 64 {
			return fmt.Errorf("host %q has too many tags", h.ID)
		}
		for _, tag := range h.Tags {
			if !safeMetadata(tag, 64) {
				return fmt.Errorf("host %q has invalid tag", h.ID)
			}
		}
		hostIDs[h.ID] = true
	}
	for _, h := range i.Hosts {
		if h.ProxyJump != "" && !hostIDs[h.ProxyJump] {
			return fmt.Errorf("host %q references unknown proxy_jump %q", h.ID, h.ProxyJump)
		}
		if h.ProxyJump == h.ID {
			return fmt.Errorf("host %q cannot proxy through itself", h.ID)
		}
	}
	jumps := make(map[string]string, len(i.Hosts))
	for _, h := range i.Hosts {
		jumps[h.ID] = h.ProxyJump
	}
	for _, h := range i.Hosts {
		seenJumps := map[string]bool{}
		for current := h.ID; current != ""; current = jumps[current] {
			if seenJumps[current] {
				return fmt.Errorf("host %q is part of a proxy_jump cycle", h.ID)
			}
			seenJumps[current] = true
		}
	}
	for _, p := range i.Profiles {
		if err := checkID("profile", p.ID); err != nil {
			return err
		}
		if !safeMetadata(p.Label, 256) {
			return fmt.Errorf("profile %q label is required", p.ID)
		}
		if len(p.Command) == 0 || p.Command[0] == "" {
			return fmt.Errorf("profile %q command is required", p.ID)
		}
		if len(p.Command) > 64 {
			return fmt.Errorf("profile %q has too many command arguments", p.ID)
		}
		for _, arg := range p.Command {
			if len(arg) > 4096 || hasControl(arg) {
				return fmt.Errorf("profile %q has an invalid command argument", p.ID)
			}
		}
		if p.DefaultDirectory == "" || len(p.DefaultDirectory) > 4096 ||
			hasControl(p.DefaultDirectory) {
			return fmt.Errorf("profile %q has invalid default_directory", p.ID)
		}
		if len(p.Tags) > 64 {
			return fmt.Errorf("profile %q has too many tags", p.ID)
		}
		for _, tag := range p.Tags {
			if !safeMetadata(tag, 64) {
				return fmt.Errorf("profile %q has invalid tag", p.ID)
			}
		}
	}
	return nil
}

func hasControl(value string) bool {
	for _, r := range value {
		if unsafeTextRune(r) {
			return true
		}
	}
	return false
}

func safeMetadata(value string, max int) bool {
	if value == "" || len(value) > max {
		return false
	}
	return !hasControl(value)
}

func ValidateSessionID(id string) error {
	if !sessionIDPattern.MatchString(id) {
		return fmt.Errorf("invalid tmux stable session id %q", id)
	}
	return nil
}

func ValidateStableID(id string) error {
	if !stableIDPattern.MatchString(id) {
		return fmt.Errorf("invalid stable id %q", id)
	}
	return nil
}

func SafeText(value string, max int) string {
	if max < 1 {
		return ""
	}
	var b strings.Builder
	for _, r := range value {
		if unsafeTextRune(r) {
			r = ' '
		}
		if b.Len()+utf8.RuneLen(r) > max {
			break
		}
		b.WriteRune(r)
	}
	return strings.TrimSpace(b.String())
}

func unsafeTextRune(r rune) bool {
	return unicode.IsControl(r) || unicode.Is(unicode.Bidi_Control, r)
}

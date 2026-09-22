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

type Inventory struct {
	SchemaVersion int       `toml:"schema_version" json:"schema_version"`
	Revision      string    `toml:"revision" json:"revision"`
	Profiles      []Profile `toml:"profiles" json:"profiles"`
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
	HostMetrics     *HostMetrics `json:"host_metrics,omitempty"`
}

// HostMetrics is a bounded snapshot sampled on the Home Mac. It is attached
// to web catalog streams. Unsupported observations are omitted.
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
// embedded in larger protocol envelopes whose fields must remain decodable.
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
	Width          int              `json:"width,omitempty"`
	Height         int              `json:"height,omitempty"`
	Workflow       *WorkflowSummary `json:"workflow,omitempty"`
	Workflows      []Workflow       `json:"workflows,omitempty"`
	PanePID        int              `json:"-"`
}

func (i Inventory) Validate() error {
	if i.SchemaVersion != SchemaVersion {
		return fmt.Errorf("schema_version must be %d, got %d", SchemaVersion, i.SchemaVersion)
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

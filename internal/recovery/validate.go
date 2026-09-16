package recovery

import (
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/model"
)

const (
	maximumSessions       = 512
	maximumWindows        = 4096
	maximumPanes          = 8192
	maximumWindowsSession = 512
	maximumPanesWindow    = 512
)

func validateBootID(value string) (string, error) {
	if value == "" || len(value) > 256 || !utf8.ValidString(value) || strings.IndexFunc(value, unicode.IsControl) >= 0 {
		return "", errors.New("invalid operating system boot identity")
	}
	return value, nil
}

func validateState(current diskState) error {
	if current.Version != stateVersion {
		return fmt.Errorf("unsupported recovery state version %d", current.Version)
	}
	if _, err := validateBootID(current.BootID); err != nil {
		return err
	}
	if _, err := timeValue(current.UpdatedAt, true); err != nil {
		return err
	}
	if err := validateSnapshot(current.Checkpoint); err != nil {
		return err
	}
	if current.Pending != nil {
		if _, err := validateBootID(current.Pending.BootID); err != nil {
			return err
		}
		if err := validateSnapshot(current.Pending.Snapshot); err != nil {
			return err
		}
		if len(current.Pending.Completed) > maximumSessions {
			return errors.New("recovery completed-session count exceeds limit")
		}
		if len(current.Pending.Intents) > maximumSessions {
			return errors.New("too many recovery intents")
		}
		for key, path := range current.Pending.Intents {
			found := false
			for _, session := range current.Pending.Snapshot.Sessions {
				if identityKey(session.Identity) == key {
					found = true
					break
				}
			}
			if !found || validateCwd(path) != nil || filepath.Base(path) != "ready" || !strings.HasPrefix(filepath.Base(filepath.Dir(path)), "launch-") {
				return errors.New("invalid recovery intent")
			}
		}
		for key, mapping := range current.Pending.Completed {
			if key != identityKey(mapping.From) {
				return errors.New("recovery completed-session key mismatch")
			}
			if err := validateMapping(mapping); err != nil {
				return err
			}
		}
	}
	if len(current.Mappings) > maximumSessions {
		return errors.New("recovery identity mapping count exceeds limit")
	}
	seenTarget := map[string]bool{}
	seenSource := map[string]bool{}
	for _, mapping := range current.Mappings {
		if err := validateMapping(mapping); err != nil {
			return err
		}
		key := identityKey(mapping.To)
		if seenTarget[key] || seenSource[identityKey(mapping.From)] {
			return errors.New("duplicate recovery target identity")
		}
		seenTarget[key] = true
		seenSource[identityKey(mapping.From)] = true
	}
	return nil
}

func timeValue(raw string, optional bool) (int64, error) {
	if raw == "" && optional {
		return 0, nil
	}
	value, err := time.Parse(time.RFC3339Nano, raw)
	if err != nil || value.Location() != time.UTC {
		return 0, errors.New("invalid recovery timestamp")
	}
	return value.UnixNano(), nil
}

func validateSnapshot(value snapshot) error {
	if len(value.Sessions) > maximumSessions {
		return errors.New("recovery session count exceeds limit")
	}
	sessionNames := map[string]bool{}
	sessionIDs := map[string]bool{}
	windowTotal, paneTotal := 0, 0
	for _, session := range value.Sessions {
		if err := validateIdentity(session.Identity); err != nil {
			return err
		}
		if err := validateSessionName(session.Name); err != nil {
			return err
		}
		if sessionNames[session.Name] || sessionIDs[session.Identity.ID] {
			return errors.New("duplicate recovery session name")
		}
		sessionNames[session.Name] = true
		sessionIDs[session.Identity.ID] = true
		if len(session.Windows) == 0 || len(session.Windows) > maximumWindowsSession {
			return errors.New("recovery window count is out of bounds")
		}
		if model.SafeText(session.Alias, 256) != session.Alias || model.SafeText(session.Profile, 128) != session.Profile || model.SafeText(session.Label, 256) != session.Label || len(session.Tags) > 128 {
			return errors.New("invalid recovery session metadata")
		}
		for _, tag := range session.Tags {
			if model.SafeText(tag, 128) != tag {
				return errors.New("invalid recovery session tag")
			}
		}
		indices := map[int]bool{}
		activeWindows := 0
		for _, window := range session.Windows {
			windowTotal++
			if window.Index < 0 || window.Index > 1_000_000 || indices[window.Index] {
				return errors.New("invalid recovery window index")
			}
			indices[window.Index] = true
			if err := validateSafeText(window.Name, 256, false); err != nil || validateLayout(window.Layout) != nil {
				return errors.New("invalid recovery window")
			}
			if window.Active {
				activeWindows++
			}
			if len(window.Panes) == 0 || len(window.Panes) > maximumPanesWindow {
				return errors.New("recovery pane count is out of bounds")
			}
			paneIndices, activePanes := map[int]bool{}, 0
			for _, pane := range window.Panes {
				paneTotal++
				if pane.Index < 0 || pane.Index > 1_000_000 || paneIndices[pane.Index] {
					return errors.New("invalid recovery pane index")
				}
				paneIndices[pane.Index] = true
				if err := validateCwd(pane.Cwd); err != nil {
					return err
				}
				if pane.Active {
					activePanes++
				}
				if pane.Resume != nil {
					if err := pane.Resume.Validate(); err != nil {
						return err
					}
				}
			}
			if activePanes != 1 {
				return errors.New("recovery window must have one active pane")
			}
		}
		if activeWindows != 1 {
			return errors.New("recovery session must have one active window")
		}
	}
	if windowTotal > maximumWindows || paneTotal > maximumPanes {
		return errors.New("recovery topology exceeds limit")
	}
	return nil
}

func validateMapping(value restoredIdentity) error {
	if err := validateIdentity(value.From); err != nil {
		return err
	}
	if err := validateIdentity(value.To); err != nil {
		return err
	}
	if err := validateSessionName(value.Name); err != nil {
		return err
	}
	if len(value.Panes) > maximumPanes {
		return errors.New("recovery mapping pane count exceeds limit")
	}
	for key, id := range value.Panes {
		if !validPositionKey(key) || !validTmuxID(id, '%') {
			return errors.New("invalid recovered pane mapping")
		}
	}
	if value.Gate != "" {
		if err := validateCwd(value.Gate); err != nil {
			return err
		}
		if filepath.Base(value.Gate) != "ready" || !strings.HasPrefix(filepath.Base(filepath.Dir(value.Gate)), "launch-") {
			return errors.New("invalid recovery gate")
		}
	}

	return nil
}

func validPositionKey(value string) bool {
	parts := strings.Split(value, "/")
	if len(parts) != 2 {
		return false
	}
	for _, part := range parts {
		if _, err := parseNonnegative(part, "pane position"); err != nil {
			return false
		}
	}
	return true
}

func validateIdentity(value model.SessionIdentity) error {
	if err := model.ValidateSessionID(value.ID); err != nil {
		return err
	}
	if value.CreatedAt < 1 {
		return errors.New("invalid recovery session creation time")
	}
	return nil
}

func validateSessionName(value string) error {
	if err := validateSafeText(value, 512, false); err != nil {
		return err
	}
	if strings.ContainsAny(value, ":.") {
		return errors.New("tmux session name contains a target separator")
	}
	return nil
}

func validateSafeText(value string, maximum int, empty bool) error {
	if (!empty && value == "") || len(value) > maximum || !utf8.ValidString(value) || strings.IndexFunc(value, unicode.IsControl) >= 0 {
		return errors.New("unsafe recovery text")
	}
	return nil
}

func validateCwd(value string) error {
	if !filepath.IsAbs(value) || filepath.Clean(value) != value {
		return errors.New("recovery working directory is not a clean absolute path")
	}
	return validateSafeText(value, 4096, false)
}

func validateLayout(value string) error {
	if err := validateSafeText(value, 64*1024, false); err != nil {
		return err
	}
	if len(value) < 6 || value[4] != ',' {
		return errors.New("invalid tmux layout checksum")
	}
	for _, c := range value[:4] {
		if !strings.ContainsRune("0123456789abcdef", c) {
			return errors.New("invalid tmux layout checksum")
		}
	}
	at := 5
	var number = func() bool {
		begin := at
		for at < len(value) && value[at] >= '0' && value[at] <= '9' {
			at++
		}
		return at > begin && at-begin <= 10
	}
	var cell func(int) bool
	cell = func(depth int) bool {
		if depth > 128 {
			return false
		}
		for _, delimiter := range []byte{'x', ',', ','} {
			if !number() || at >= len(value) || value[at] != delimiter {
				return false
			}
			at++
		}
		if !number() || at >= len(value) {
			return false
		}
		if value[at] == ',' {
			at++
			return number()
		}
		close := byte('}')
		if value[at] == '[' {
			close = ']'
		} else if value[at] != '{' {
			return false
		}
		at++
		if !cell(depth + 1) {
			return false
		}
		count := 1
		for at < len(value) && value[at] == ',' {
			at++
			count++
			if !cell(depth + 1) {
				return false
			}
		}
		if count < 2 || at >= len(value) || value[at] != close {
			return false
		}
		at++
		return true
	}
	if !cell(0) || at != len(value) {
		return errors.New("invalid tmux layout geometry")
	}
	return nil
}

func cloneSnapshot(value snapshot) snapshot {
	raw, _ := json.Marshal(value)
	var cloned snapshot
	_ = json.Unmarshal(raw, &cloned)
	return cloned
}

func completedMappings(values map[string]restoredIdentity) []restoredIdentity {
	result := make([]restoredIdentity, 0, len(values))
	for _, value := range values {
		result = append(result, restoredIdentity{From: value.From, To: value.To, Name: value.Name})
	}
	sort.Slice(result, func(i, j int) bool { return identityKey(result[i].From) < identityKey(result[j].From) })
	return result
}

func parseNonnegative(raw, field string) (int, error) {
	value, err := strconv.Atoi(raw)
	if err != nil || value < 0 || value > 1_000_000 {
		return 0, fmt.Errorf("invalid %s", field)
	}
	return value, nil
}

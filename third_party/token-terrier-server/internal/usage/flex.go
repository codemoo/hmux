package usage

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
)

// flexFloat decodes a JSON number that the upstream may emit as either
// a JSON number or a quoted string. Mirrors Swift's decodeLossy(Double:)
// which silently swallows type mismatches.
//
// Used for codex usage fields where the OpenAI side has historically
// flipped between string and number across rollouts.
type flexFloat struct {
	Set   bool
	Value float64
}

// UnmarshalJSON accepts numbers, numeric strings, and the JSON null literal.
func (f *flexFloat) UnmarshalJSON(data []byte) error {
	s := strings.TrimSpace(string(data))
	if s == "null" || s == "" {
		return nil
	}
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		s = s[1 : len(s)-1]
	}
	if s == "" {
		return nil
	}
	v, err := strconv.ParseFloat(s, 64)
	if err != nil {
		if errors.Is(err, strconv.ErrRange) {
			return fmt.Errorf("number out of range")
		}
		// Tolerant — return without setting Set so caller treats as nil.
		// Daemon-wide policy: rather a missing field than a snapshot
		// failure when upstream emits something unexpected.
		var any json.RawMessage
		if e := json.Unmarshal(data, &any); e != nil {
			return e
		}
		return nil
	}
	if math.IsNaN(v) || math.IsInf(v, 0) {
		return fmt.Errorf("non-finite number")
	}
	f.Set = true
	f.Value = v
	return nil
}

// Ptr returns the value as *float64 if set, else nil.
func (f flexFloat) Ptr() *float64 {
	if !f.Set {
		return nil
	}
	v := f.Value
	return &v
}

// flexInt — same idea for ints (window minutes, seconds).
type flexInt struct {
	Set   bool
	Value int
}

func (f *flexInt) UnmarshalJSON(data []byte) error {
	s := strings.TrimSpace(string(data))
	if s == "null" || s == "" {
		return nil
	}
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		s = s[1 : len(s)-1]
	}
	if s == "" {
		return nil
	}
	if v, err := strconv.Atoi(s); err == nil {
		f.Set = true
		f.Value = v
		return nil
	}
	if v, err := strconv.ParseFloat(s, 64); err == nil {
		maxInt := float64(int64(^uint(0) >> 1))
		minInt := -maxInt - 1
		if math.IsNaN(v) || math.IsInf(v, 0) || v > maxInt || v < minInt {
			return fmt.Errorf("integer out of range")
		}
		f.Set = true
		f.Value = int(v)
		return nil
	} else if errors.Is(err, strconv.ErrRange) {
		return fmt.Errorf("integer out of range")
	}
	return nil
}

func (f flexInt) Ptr() *int {
	if !f.Set {
		return nil
	}
	v := f.Value
	return &v
}

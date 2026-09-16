package model

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
)

const (
	maximumCatalogJSONDepth        = 64
	maximumCatalogJSONObjectFields = 256
	maximumCatalogJSONArrayItems   = 10_000
)

// ValidateCatalogJSONStructure performs a streaming, allocation-bounded shape
// pass before callers decode an untrusted remote catalog into Go slices. Byte
// limits alone do not prevent tiny JSON elements such as {} from expanding
// into very large slices during json.Unmarshal.
func ValidateCatalogJSONStructure(data []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	limits := map[string]int{
		"sessions":     10_000,
		"open_tabs":    256,
		"window_names": 10_000,
		"tags":         64,
		"workflows":    32,
		"nodes":        128,
	}
	if err := validateCatalogJSONValue(decoder, "", 0, limits); err != nil {
		return err
	}
	if _, err := decoder.Token(); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("remote catalog contains trailing JSON")
		}
		return fmt.Errorf("decode remote catalog trailing JSON: %w", err)
	}
	return nil
}

func validateCatalogJSONValue(decoder *json.Decoder, key string, depth int, limits map[string]int) error {
	if depth > maximumCatalogJSONDepth {
		return errors.New("remote catalog JSON nesting exceeds limit")
	}
	token, err := decoder.Token()
	if err != nil {
		return fmt.Errorf("decode remote catalog structure: %w", err)
	}
	delimiter, ok := token.(json.Delim)
	if !ok {
		return nil
	}
	switch delimiter {
	case '{':
		fields := 0
		for decoder.More() {
			fields++
			if fields > maximumCatalogJSONObjectFields {
				return errors.New("remote catalog object field count exceeds limit")
			}
			nameToken, err := decoder.Token()
			if err != nil {
				return fmt.Errorf("decode remote catalog object key: %w", err)
			}
			name, ok := nameToken.(string)
			if !ok {
				return errors.New("remote catalog object key is invalid")
			}
			if err := validateCatalogJSONValue(decoder, name, depth+1, limits); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim('}') {
			return errors.New("remote catalog object is incomplete")
		}
	case '[':
		limit := maximumCatalogJSONArrayItems
		if specific, exists := limits[key]; exists {
			limit = specific
		}
		count := 0
		for decoder.More() {
			count++
			if count > limit {
				return fmt.Errorf("remote catalog %s array count exceeds limit", key)
			}
			if err := validateCatalogJSONValue(decoder, "", depth+1, limits); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim(']') {
			return errors.New("remote catalog array is incomplete")
		}
	default:
		return errors.New("remote catalog JSON delimiter is invalid")
	}
	return nil
}

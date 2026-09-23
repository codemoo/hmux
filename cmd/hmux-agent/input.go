package main

import (
	"errors"
	"fmt"
	"io"
	"strings"
)

func readBoundedSingleLine(reader io.Reader, maxBytes int64) (string, error) {
	if maxBytes < 1 {
		return "", errors.New("invalid input limit")
	}
	data, err := io.ReadAll(io.LimitReader(reader, maxBytes+2))
	if err != nil {
		return "", err
	}
	if int64(len(data)) > maxBytes+1 {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	text := string(data)
	text = strings.TrimSuffix(text, "\n")
	text = strings.TrimSuffix(text, "\r")
	if int64(len(text)) > maxBytes {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	if strings.ContainsAny(text, "\r\n") {
		return "", errors.New("input must be a single line")
	}
	return text, nil
}

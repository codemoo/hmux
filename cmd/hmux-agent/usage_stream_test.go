package main

import (
	"context"
	"errors"
	"io"
	"strings"
	"testing"
)

func TestAgentUsageStreamStopsCleanlyOnInputEOF(t *testing.T) {
	err := serveAgentUsageStream(context.Background(), strings.NewReader(""), func(ctx context.Context) error {
		<-ctx.Done()
		return ctx.Err()
	})
	if err != nil {
		t.Fatalf("EOF stream error=%v", err)
	}
}

func TestAgentUsageStreamRejectsClientInput(t *testing.T) {
	err := serveAgentUsageStream(context.Background(), strings.NewReader("refresh\n"), func(ctx context.Context) error {
		<-ctx.Done()
		return ctx.Err()
	})
	if err == nil || !strings.Contains(err.Error(), "read-only") {
		t.Fatalf("client input error=%v", err)
	}
}

func TestAgentUsageStreamReturnsProducerFailure(t *testing.T) {
	reader, writer := io.Pipe()
	want := errors.New("usage failed")
	err := serveAgentUsageStream(context.Background(), reader, func(context.Context) error {
		return want
	})
	_ = writer.Close()
	_ = reader.Close()
	if !errors.Is(err, want) {
		t.Fatalf("producer error=%v", err)
	}
}

package main

import (
	"context"
	"errors"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/model"
)

func TestAgentCatalogStreamStopsCleanlyOnInputEOF(t *testing.T) {
	err := serveAgentCatalogStream(context.Background(), strings.NewReader(""), func(ctx context.Context) error {
		<-ctx.Done()
		return ctx.Err()
	})
	if err != nil {
		t.Fatalf("EOF stream error=%v", err)
	}
}

func TestAgentCatalogStreamRejectsClientInput(t *testing.T) {
	err := serveAgentCatalogStream(context.Background(), strings.NewReader("refresh\n"), func(ctx context.Context) error {
		<-ctx.Done()
		return ctx.Err()
	})
	if err == nil || !strings.Contains(err.Error(), "read-only") {
		t.Fatalf("client input error=%v", err)
	}
}

func TestAgentCatalogStreamReturnsProducerFailure(t *testing.T) {
	reader, writer := io.Pipe()
	want := errors.New("snapshot failed")
	err := serveAgentCatalogStream(context.Background(), reader, func(context.Context) error {
		return want
	})
	_ = writer.Close()
	_ = reader.Close()
	if !errors.Is(err, want) {
		t.Fatalf("producer error=%v", err)
	}
}

func TestParseCatalogStreamArgsAllowsOnlyExplicitMetricsOptIn(t *testing.T) {
	if enabled, err := parseCatalogStreamArgs([]string{"--stdio"}); err != nil || enabled {
		t.Fatalf("legacy args enabled=%t err=%v", enabled, err)
	}
	if enabled, err := parseCatalogStreamArgs([]string{"--stdio", "--host-metrics"}); err != nil || !enabled {
		t.Fatalf("metrics args enabled=%t err=%v", enabled, err)
	}
	for _, args := range [][]string{
		nil,
		{"--host-metrics", "--stdio"},
		{"--stdio", "--host-metrics", "extra"},
		{"--stdio", "--unknown"},
	} {
		if _, err := parseCatalogStreamArgs(args); err == nil {
			t.Fatalf("unsafe args accepted: %v", args)
		}
	}
}

type fixedCatalogMetricsSampler struct {
	value *model.HostMetrics
	calls int
}

func (s *fixedCatalogMetricsSampler) Sample(context.Context) *model.HostMetrics {
	s.calls++
	return s.value
}

func TestCatalogMetricsSamplerIsFailOpenAndStreamScoped(t *testing.T) {
	baseCalls := 0
	base := catalogstream.Fetch(func(context.Context) (model.Catalog, error) {
		baseCalls++
		return model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: []model.Session{}}, nil
	})
	failed := &fixedCatalogMetricsSampler{}
	value, err := catalogFetchWithHostMetrics(base, failed)(context.Background())
	if err != nil || value.HostMetrics != nil || failed.calls != 1 || baseCalls != 1 {
		t.Fatalf("failed metrics changed catalog: value=%#v calls=%d/%d err=%v", value, failed.calls, baseCalls, err)
	}
	percent := 25.0
	valid := &fixedCatalogMetricsSampler{value: &model.HostMetrics{ObservedAt: time.Now().UTC(), CPUPercent: &percent}}
	value, err = catalogFetchWithHostMetrics(base, valid)(context.Background())
	if err != nil || value.HostMetrics == nil || value.HostMetrics.CPUPercent == nil || valid.calls != 1 || baseCalls != 2 {
		t.Fatalf("valid metrics missing: value=%#v calls=%d/%d err=%v", value, valid.calls, baseCalls, err)
	}
}

package home

import (
	"context"
	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/config"
	"os"
	"testing"
	"time"
)

func TestReadOnlyHomeCatalogTiming(t *testing.T) {
	if os.Getenv("HMUX_RUN_LOADING_READ_TEST") != "1" {
		t.Skip("live read timing opt-in")
	}
	cfg, err := config.LoadHome("")
	if err != nil {
		t.Fatal("config unavailable")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	for i := 0; i < 3; i++ {
		basicStart := time.Now()
		_, basicErr := agent.BasicCatalogAt(ctx, cfg.StateDir)
		if basicErr != nil {
			t.Fatal("basic catalog unavailable")
		}
		t.Logf("basic_home_catalog_ms=%d", time.Since(basicStart).Milliseconds())
		start := time.Now()
		c, err := Catalog(ctx, cfg)
		if err != nil {
			t.Fatal("catalog unavailable")
		}
		t.Logf("home_catalog_ms=%d sessions=%d", time.Since(start).Milliseconds(), len(c.Sessions))
	}
}

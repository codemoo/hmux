package homeservice

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSyntheticDiagnosticLogCost(t *testing.T) {
	if os.Getenv("HMUX_RUN_LOADING_COST_TEST") != "1" {
		t.Skip("manual timing probe")
	}
	l, err := OpenLog(filepath.Join(tempDir(t), "diagnostic.log"))
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	start := time.Now()
	for i := 0; i < 1000; i++ {
		if _, err := l.Write([]byte("2026/01/01 00:00:00 stage=request-complete reason=ok duration_ms=10\n")); err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("synthetic_1000_log_writes_us=%d", time.Since(start).Microseconds())
}

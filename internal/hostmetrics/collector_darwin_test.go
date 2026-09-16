//go:build darwin

package hostmetrics

import (
	"context"
	"errors"
	"reflect"
	"sort"
	"sync"
	"testing"
	"time"
)

func TestDarwinSamplerUsesOnlyFixedBoundedCommands(t *testing.T) {
	type call struct {
		path  string
		args  []string
		limit int64
	}
	var mutex sync.Mutex
	var calls []call
	run := func(_ context.Context, path string, args []string, limit int64) ([]byte, error) {
		mutex.Lock()
		calls = append(calls, call{path: path, args: append([]string(nil), args...), limit: limit})
		mutex.Unlock()
		switch path {
		case topPath:
			return []byte("CPU usage: 20% user, 5% sys, 75% idle\nCPU usage: 25% user, 5% sys, 70% idle\n"), nil
		case vmStatPath:
			return []byte("Mach Virtual Memory Statistics: (page size of 4096 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 10.\nPages occupied by compressor: 5.\n"), nil
		case sysctlPath:
			return []byte("1048576\n"), nil
		case ioregPath:
			return []byte(`<plist><dict><key>Device Utilization %</key><integer>20</integer></dict></plist>`), nil
		default:
			return nil, errors.New("unexpected executable")
		}
	}
	value := newDarwinSampler(run)(context.Background())
	if value.cpuPercent == nil || *value.cpuPercent != 30 || value.gpuPercent == nil || *value.gpuPercent != 20 || value.memoryUsedBytes == nil {
		t.Fatalf("measurements=%#v", value)
	}
	mutex.Lock()
	defer mutex.Unlock()
	sort.Slice(calls, func(i, j int) bool { return calls[i].path < calls[j].path })
	var gotTop, gotVM, gotSysctl, gotIOReg bool
	for _, call := range calls {
		switch call.path {
		case topPath:
			gotTop = call.limit == textMax && reflect.DeepEqual(call.args, []string{"-l", "2", "-s", "1", "-n", "0", "-stats", "pid"})
		case vmStatPath:
			gotVM = call.limit == textMax && len(call.args) == 0
		case sysctlPath:
			gotSysctl = call.limit == 256 && reflect.DeepEqual(call.args, []string{"-n", "hw.memsize"})
		case ioregPath:
			gotIOReg = call.limit == ioregMax && reflect.DeepEqual(call.args, []string{"-a", "-r", "-c", "IOAccelerator"})
		default:
			t.Fatalf("unexpected path %q", call.path)
		}
	}
	if !gotTop || !gotVM || !gotSysctl || !gotIOReg {
		t.Fatalf("fixed command set missing: %#v", calls)
	}
}

func TestDarwinSamplerFailuresReturnNoObservation(t *testing.T) {
	run := func(context.Context, string, []string, int64) ([]byte, error) {
		return nil, errors.New("unavailable")
	}
	value := newDarwinSampler(run)(context.Background())
	if value.cpuPercent != nil || value.gpuPercent != nil || value.memoryUsedBytes != nil {
		t.Fatalf("failed samplers returned %#v", value)
	}
	if metrics := buildHostMetrics(stringsTime(), value); metrics != nil {
		t.Fatalf("failed samplers produced metrics %#v", metrics)
	}
}

func stringsTime() time.Time {
	return time.Unix(1, 0)
}

//go:build darwin

package hostmetrics

import (
	"context"
	"os/exec"
	"sync"

	"github.com/codemoo/hmux/internal/safeexec"
)

const (
	topPath    = "/usr/bin/top"
	vmStatPath = "/usr/bin/vm_stat"
	sysctlPath = "/usr/sbin/sysctl"
	ioregPath  = "/usr/sbin/ioreg"
	textMax    = int64(64 * 1024)
	ioregMax   = int64(2 * 1024 * 1024)
)

type commandRunner func(context.Context, string, []string, int64) ([]byte, error)

func newPlatformSampler() sampler {
	return newDarwinSampler(runCommand)
}

func runCommand(ctx context.Context, executable string, args []string, limit int64) ([]byte, error) {
	command := exec.CommandContext(ctx, executable, args...)
	command.Env = []string{"PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LANG=C", "LC_ALL=C"}
	return safeexec.Output(command, limit)
}

func newDarwinSampler(run commandRunner) sampler {
	return func(ctx context.Context) measurements {
		var result measurements
		var wait sync.WaitGroup
		wait.Add(3)
		go func() {
			defer wait.Done()
			// The second logging sample is used because top documents the
			// first sample's per-process CPU deltas as invalid.
			output, err := run(ctx, topPath, []string{"-l", "2", "-s", "1", "-n", "0", "-stats", "pid"}, textMax)
			if err == nil {
				result.cpuPercent, _ = parseCPUPercent(output)
			}
		}()
		go func() {
			defer wait.Done()
			vmStat, vmErr := run(ctx, vmStatPath, nil, textMax)
			if vmErr != nil {
				return
			}
			total, totalErr := run(ctx, sysctlPath, []string{"-n", "hw.memsize"}, 256)
			if totalErr == nil {
				result.memoryUsedBytes, result.memoryTotalBytes, _ = parseMemory(vmStat, total)
			}
		}()
		go func() {
			defer wait.Done()
			for _, className := range []string{"IOAccelerator", "AGXAccelerator"} {
				output, err := run(ctx, ioregPath, []string{"-a", "-r", "-c", className}, ioregMax)
				if err != nil {
					continue
				}
				if percent, parseErr := parseGPUPercent(output); parseErr == nil {
					result.gpuPercent = percent
					return
				}
			}
		}()
		wait.Wait()
		return result
	}
}

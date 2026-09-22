package hostmetrics

import (
	"bytes"
	"strconv"
	"strings"
)

// cpuTimes returns idle (idle+iowait) and total jiffies from the aggregate
// "cpu" line of /proc/stat.
func cpuTimes(stat []byte) (idle, total uint64, ok bool) {
	for _, line := range strings.Split(string(stat), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 5 || fields[0] != "cpu" {
			continue
		}
		for i, field := range fields[1:] {
			// guest and guest_nice are already included in user and nice.
			if i >= 8 {
				break
			}
			n, err := strconv.ParseUint(field, 10, 64)
			if err != nil {
				return 0, 0, false
			}
			total += n
			if i == 3 || i == 4 {
				idle += n
			}
		}
		return idle, total, total > 0
	}
	return 0, 0, false
}

func cpuPercentBetween(first, second []byte) *float64 {
	idle1, total1, ok1 := cpuTimes(first)
	idle2, total2, ok2 := cpuTimes(second)
	if !ok1 || !ok2 || total2 <= total1 || idle2 < idle1 {
		return nil
	}
	busy := float64((total2-total1)-(idle2-idle1)) / float64(total2-total1) * 100
	return &busy
}

// parseMeminfo reports used memory as MemTotal - MemAvailable.
func parseMeminfo(raw []byte) (used, total *uint64) {
	values := map[string]uint64{}
	for _, line := range bytes.Split(raw, []byte("\n")) {
		fields := strings.Fields(string(line))
		if len(fields) < 2 || (fields[0] != "MemTotal:" && fields[0] != "MemAvailable:") {
			continue
		}
		n, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			return nil, nil
		}
		unit := uint64(1)
		if len(fields) > 2 && fields[2] == "kB" {
			unit = 1024
		}
		values[strings.TrimSuffix(fields[0], ":")] = n * unit
	}
	t, okTotal := values["MemTotal"]
	available, okAvailable := values["MemAvailable"]
	if !okTotal || !okAvailable || t == 0 || available > t {
		return nil, nil
	}
	u := t - available
	return &u, &t
}

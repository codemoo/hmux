package hostmetrics

import (
	"bytes"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"math"
	"regexp"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/model"
)

var (
	cpuIdlePattern  = regexp.MustCompile(`(?i)([0-9]+(?:\.[0-9]+)?)%[[:space:]]*idle`)
	pageSizePattern = regexp.MustCompile(`page size of ([0-9]+) bytes`)
)

func parseCPUPercent(output []byte) (*float64, error) {
	var idle *float64
	validSamples := 0
	for _, line := range strings.Split(string(output), "\n") {
		if !strings.HasPrefix(strings.TrimSpace(line), "CPU usage:") {
			continue
		}
		match := cpuIdlePattern.FindStringSubmatch(line)
		if len(match) != 2 {
			continue
		}
		value, err := strconv.ParseFloat(match[1], 64)
		if err != nil || math.IsNaN(value) || math.IsInf(value, 0) || value < 0 || value > 100 {
			continue
		}
		idle = &value
		validSamples++
	}
	if idle == nil || validSamples < 2 {
		return nil, errors.New("top output has no valid second CPU usage sample")
	}
	used := 100 - *idle
	if used < 0 && used > -1e-9 {
		used = 0
	}
	return &used, nil
}

func parseMemory(vmStatOutput, totalOutput []byte) (*uint64, *uint64, error) {
	header := strings.SplitN(string(vmStatOutput), "\n", 2)[0]
	match := pageSizePattern.FindStringSubmatch(header)
	if len(match) != 2 {
		return nil, nil, errors.New("vm_stat output has no page size")
	}
	pageSize, err := strconv.ParseUint(match[1], 10, 64)
	if err != nil || pageSize == 0 {
		return nil, nil, errors.New("vm_stat page size is invalid")
	}
	wanted := map[string]*uint64{
		"Anonymous pages":              nil,
		"Pages wired down":             nil,
		"Pages purgeable":              nil,
		"Pages occupied by compressor": nil,
	}
	for _, line := range strings.Split(string(vmStatOutput), "\n") {
		name, raw, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		if _, exists := wanted[name]; !exists {
			continue
		}
		value, parseErr := strconv.ParseUint(strings.TrimSuffix(strings.TrimSpace(raw), "."), 10, 64)
		if parseErr != nil {
			return nil, nil, fmt.Errorf("vm_stat %s is invalid", name)
		}
		copy := value
		wanted[name] = &copy
	}
	for name, value := range wanted {
		if value == nil {
			return nil, nil, fmt.Errorf("vm_stat output is missing %s", name)
		}
	}
	// vm_stat defines anonymous pages separately from file-backed pages. To
	// approximate Activity Monitor's app + wired + compressed definition,
	// treat purgeable pages as reclaimable rather than app memory (clamped at
	// anonymous to avoid underflow). Resident used RAM is therefore:
	// (anonymous - purgeable) + wired + compressor-resident pages.
	anonymous := *wanted["Anonymous pages"]
	purgeable := *wanted["Pages purgeable"]
	if purgeable > anonymous {
		purgeable = anonymous
	}
	pages := anonymous - purgeable
	for _, name := range []string{"Pages wired down", "Pages occupied by compressor"} {
		if pages > ^uint64(0)-*wanted[name] {
			return nil, nil, errors.New("vm_stat used page count overflows")
		}
		pages += *wanted[name]
	}
	if pages > ^uint64(0)/pageSize {
		return nil, nil, errors.New("vm_stat used byte count overflows")
	}
	used := pages * pageSize
	total, err := strconv.ParseUint(strings.TrimSpace(string(totalOutput)), 10, 64)
	if err != nil || total == 0 || total > model.MaximumHostMemoryBytes || used > total {
		return nil, nil, errors.New("physical memory values are out of bounds")
	}
	return &used, &total, nil
}

func parseGPUPercent(output []byte) (*float64, error) {
	decoder := xml.NewDecoder(bytes.NewReader(output))
	values := map[string][]float64{
		"Device Utilization %": nil,
		"GPU Activity(%)":      nil,
	}
	var pending string
	for {
		token, err := decoder.Token()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, err
		}
		start, ok := token.(xml.StartElement)
		if !ok {
			continue
		}
		if start.Name.Local == "key" {
			var key string
			if err := decoder.DecodeElement(&key, &start); err != nil {
				return nil, err
			}
			if _, tracked := values[key]; tracked {
				pending = key
			} else {
				pending = ""
			}
			continue
		}
		if pending == "" {
			continue
		}
		if start.Name.Local != "integer" && start.Name.Local != "real" && start.Name.Local != "string" {
			pending = ""
			continue
		}
		var raw string
		if err := decoder.DecodeElement(&raw, &start); err != nil {
			return nil, err
		}
		value, parseErr := strconv.ParseFloat(strings.TrimSpace(raw), 64)
		if parseErr == nil && !math.IsNaN(value) && !math.IsInf(value, 0) && value >= 0 && value <= 100 {
			values[pending] = append(values[pending], value)
		}
		pending = ""
	}
	// Device Utilization is the canonical accelerator field. GPU Activity is
	// used only when the canonical field is absent. Multiple devices use the
	// maximum current utilization; power-related registry values are ignored.
	for _, key := range []string{"Device Utilization %", "GPU Activity(%)"} {
		if len(values[key]) == 0 {
			continue
		}
		maximum := values[key][0]
		for _, value := range values[key][1:] {
			if value > maximum {
				maximum = value
			}
		}
		return &maximum, nil
	}
	return nil, errors.New("IORegistry output has no valid GPU utilization")
}

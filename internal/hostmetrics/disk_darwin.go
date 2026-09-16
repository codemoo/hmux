//go:build darwin

package hostmetrics

import "golang.org/x/sys/unix"

// DiskUsage samples the Home startup data volume once per catalog observation.
// APFS shares free space across its container; this is used/total container capacity.
func DiskUsage() (used, total uint64, ok bool) {
	var value unix.Statfs_t
	if err := unix.Statfs("/System/Volumes/Data", &value); err != nil {
		if err = unix.Statfs("/", &value); err != nil {
			return 0, 0, false
		}
	}
	return diskBytes(value.Blocks, value.Bfree, uint64(value.Bsize))
}

package hostmetrics

// diskBytes reports filesystem/container allocation, not a sum of directory sizes.
func diskBytes(blocks, free, blockSize uint64) (used, total uint64, ok bool) {
	if blockSize == 0 || blocks == 0 || free > blocks || blocks > (1<<53)/blockSize {
		return 0, 0, false
	}
	return (blocks - free) * blockSize, blocks * blockSize, true
}

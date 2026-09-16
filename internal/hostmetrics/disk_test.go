package hostmetrics

import "testing"

func TestDiskBytes(t *testing.T) {
	used, total, ok := diskBytes(100, 25, 4096)
	if !ok || used != 75*4096 || total != 100*4096 {
		t.Fatal(used, total, ok)
	}
	for _, v := range [][3]uint64{{0, 0, 4096}, {100, 101, 4096}, {100, 25, 0}, {1 << 53, 0, 4096}} {
		if _, _, ok := diskBytes(v[0], v[1], v[2]); ok {
			t.Fatal("invalid allocation accepted", v)
		}
	}
}

package webgateway

import (
	"bytes"
	"encoding/json"
	"net/netip"
	"os"
	"path/filepath"
	"sort"
	"testing"
)

// Runs the current Go outbound-address policy on synthetic literals only.
func TestRustLocationAddressOracle(t *testing.T) {
	type row struct {
		Address string `json:"address"`
		Allowed bool   `json:"allowed"`
	}
	cases := map[string]bool{}
	add := func(raw string) { cases[raw] = publicLocationIP(raw) }
	for _, raw := range []string{"8.8.8.8", "1.1.1.1", "9.9.9.9", "0.0.0.0", "255.255.255.255", "::", "::1", "2000::", "2001:4860:4860::8888", "2606:4700:4700::1111", "3fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", "4000::", "fc00::1", "fe80::1", "ff02::1", "::ffff:8.8.8.8", "::ffff:10.0.0.1", "::ffff:127.0.0.1", "::192.0.2.1", "1.2.3.04", "256.1.1.1", "8.8.8.8\n", "fe80::1%en0", "not-an-address", ""} {
		add(raw)
	}
	for _, value := range []string{"0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16", "172.16.0.0/12", "192.0.0.0/24", "192.0.2.0/24", "192.88.99.0/24", "192.168.0.0/16", "198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "224.0.0.0/4", "240.0.0.0/4", "::/96", "64:ff9b::/96", "64:ff9b:1::/48", "100::/64", "2001::/23", "2001:db8::/32", "2002::/16", "3fff::/20", "5f00::/16", "fc00::/7", "fe80::/10", "ff00::/8"} {
		prefix := netip.MustParsePrefix(value)
		first := prefix.Masked().Addr()
		var raw []byte
		if first.Is4() {
			a := first.As4()
			raw = append(raw, a[:]...)
		} else {
			a := first.As16()
			raw = append(raw, a[:]...)
		}
		for bit := prefix.Bits(); bit < len(raw)*8; bit++ {
			raw[bit/8] |= 1 << uint(7-bit%8)
		}
		last, ok := netip.AddrFromSlice(raw)
		if !ok {
			t.Fatal("invalid oracle prefix")
		}
		for _, addr := range []netip.Addr{first, first.Next(), first.Prev(), last, last.Prev(), last.Next()} {
			if !addr.IsValid() {
				continue
			}
			add(addr.String())
			if addr.Is4() {
				add("::ffff:" + addr.String())
			}
		}
	}
	var names []string
	for name := range cases {
		names = append(names, name)
	}
	sort.Strings(names)
	rows := make([]row, 0, len(names))
	for _, name := range names {
		rows = append(rows, row{name, cases[name]})
	}
	raw, e := json.MarshalIndent(rows, "", "  ")
	if e != nil {
		t.Fatal(e)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "session-location-v1", "go-addresses.json")
	if os.Getenv("UPDATE_HMUX_RUST_LOCATION_ADDRESS_FIXTURE") == "1" {
		if e = os.WriteFile(path, raw, 0644); e != nil {
			t.Fatal(e)
		}
	}
	expected, e := os.ReadFile(path)
	if e != nil {
		t.Fatal(e)
	}
	if !bytes.Equal(raw, expected) {
		t.Fatal("Go session location address policy changed")
	}
}

Synthetic credential bodies only. `go-oracle.json` is produced by the vendored
Go parser in `internal/auth/rust_oracle_test.go`; both Go and Rust tests compare
against it. The oracle contains test tokens, never host credentials.

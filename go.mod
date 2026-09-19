module github.com/codemoo/hmux

go 1.24.0

require (
	github.com/BurntSushi/toml v1.5.0
	github.com/SherClockHolmes/webpush-go v1.4.0
	github.com/codemoo/token-terrier/server-go v0.0.0
	github.com/coder/websocket v1.8.14
	github.com/creack/pty v1.1.24
	golang.org/x/crypto v0.45.0
	golang.org/x/sys v0.38.0
	golang.org/x/term v0.37.0
)

require github.com/golang-jwt/jwt/v5 v5.3.0 // indirect

replace github.com/codemoo/token-terrier/server-go => ./third_party/token-terrier-server

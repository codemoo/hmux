#!/bin/sh
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
cd "$ROOT"
npm ci --prefix web
npm run build --prefix web
rm -rf dist/web-linux-amd64/web
mkdir -p dist/web-linux-amd64/web dist/web-darwin-arm64
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -o dist/web-linux-amd64/hmux-web ./cmd/hmux-web
GOOS=darwin GOARCH=arm64 go build -trimpath -o dist/web-darwin-arm64/hmux-web ./cmd/hmux-web
VERSION=$(tr -d '[:space:]' <VERSION)
GOOS=darwin GOARCH=arm64 go build -trimpath -ldflags "-X main.version=$VERSION" -o dist/web-darwin-arm64/hmux-agent ./cmd/hmux-agent
cp -R web/dist/. dist/web-linux-amd64/web/
mkdir -p dist/web-linux-amd64/licenses
cp THIRD_PARTY_NOTICES.md dist/web-linux-amd64/
cp third_party/licenses/* dist/web-linux-amd64/licenses/
cp third_party/token-terrier-server/LICENSE dist/web-linux-amd64/licenses/token-terrier-LICENSE.txt
if [ -f LICENSE ]; then cp LICENSE dist/web-linux-amd64/; fi
cp deploy/web/hmux-web.service deploy/web/nginx.conf.example dist/web-linux-amd64/
COPYFILE_DISABLE=1 tar --no-xattrs -czf dist/hmux-web-linux-amd64.tar.gz -C dist/web-linux-amd64 .

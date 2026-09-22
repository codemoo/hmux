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
for hmux_bundle in dist/web-linux-amd64 dist/web-darwin-arm64; do
  mkdir -p "$hmux_bundle/licenses"
  cp THIRD_PARTY_NOTICES.md "$hmux_bundle/"
  cp third_party/licenses/* "$hmux_bundle/licenses/"
  cp third_party/token-terrier-server/LICENSE "$hmux_bundle/licenses/token-terrier-LICENSE.txt"
  cp third_party/token-terrier-server/NOTICE "$hmux_bundle/licenses/token-terrier-NOTICE.txt"
  if [ -f LICENSE ]; then cp LICENSE "$hmux_bundle/"; fi
done
cp deploy/web/hmux-web.service deploy/web/nginx.conf.example dist/web-linux-amd64/
COPYFILE_DISABLE=1 tar --no-xattrs -czf dist/hmux-web-linux-amd64.tar.gz -C dist/web-linux-amd64 .

# Third-party components

HMux retains each component's license; a top-level source license does not
replace those terms. The tables below cover direct runtime dependencies and
bundled assets. Package lockfiles and native dependency pins are authoritative
for versions. This is not a complete native static-library SBOM.

| Component | Version/source | License location |
| --- | --- | --- |
| BurntSushi/toml | v1.5.0 | `third_party/licenses/toml-LICENSE.txt` |
| coder/websocket | v1.8.14 | `third_party/licenses/websocket-LICENSE.txt` |
| creack/pty | v1.1.24 | `third_party/licenses/pty-LICENSE.txt` |
| Go x/sys | v0.33.0 | `third_party/licenses/x-sys-LICENSE.txt` |
| Go x/term | v0.32.0 | `third_party/licenses/x-term-LICENSE.txt` |
| Token Terrier collector | in-tree source snapshot | `third_party/token-terrier-server/LICENSE`, `NOTICE`, `UPSTREAM.md` |
| xterm.js | 6.0.0 | `web/public/licenses/xterm-LICENSE.txt` |
| xterm addon-fit | 0.11.0 | `web/public/licenses/xterm-addon-fit-LICENSE.txt` |
| xterm addon-web-links | 0.12.0 | `web/public/licenses/xterm-addon-web-links-LICENSE.txt` |
| es-hangul | package-lock.json | `web/public/licenses/` |
| marked | package-lock.json | `web/public/licenses/marked-LICENSE.txt` |
| entities | package-lock.json | `web/public/licenses/entities-LICENSE.txt` |
| Monatendard / underlying fonts | bundled files | `web/public/fonts/NOTICE.txt`, `THIRD_PARTY_NOTICES.md` and license texts |
| Ghostty / native libraries and sprites | pinned native manifest | `macos/HMux/ThirdPartyNotices.md`, `Dependencies.lock.json` |

Web builds copy public font and JavaScript notices into the served assets.
`deploy/web/build.sh` includes Go runtime notices in its deployment archive.
Before distributing native binaries, complete the remaining license/SBOM and
signing gates in [docs/RELEASING.md](docs/RELEASING.md).

# Third-party components

HMux retains each component's license; a top-level source license does not
replace those terms. The tables below cover direct runtime dependencies and
bundled assets. Go modules, the web package lockfile and `Cargo.lock` record the
versions used by their respective runtimes. Native Rust bundles include
`licenses/rust/INDEX.json`: host/target transitive normal/build dependency lists,
declared license expressions and hashes of retained upstream notices. Development-only
dependencies are excluded; build dependencies are included conservatively. The
bundle also retains Rust standard-library copyright/license texts and ring's
per-source copyright blocks. `deny.toml` defines the dependency license/source policy.

| Component | Version/source | License location |
| --- | --- | --- |
| BurntSushi/toml | v1.5.0 | `third_party/licenses/toml-LICENSE.txt` |
| coder/websocket | v1.8.14 | `third_party/licenses/websocket-LICENSE.txt` |
| creack/pty | v1.1.24 | `third_party/licenses/pty-LICENSE.txt` |
| quick-xml (candidate Rust Home metric parser) | v0.41.0, default features disabled | `third_party/licenses/quick-xml-LICENSE-MIT.txt` |
| idna (candidate Rust Home proxy bypass) | v1.1.0 | `third_party/licenses/idna-LICENSE-MIT.txt` |
| idna_adapter (pinned Unicode Rust backend) | v1.1.0 | `third_party/licenses/idna_adapter-LICENSE-MIT.txt` |
| idna_mapping (Unicode mapping tables) | v1.1.0 | `third_party/licenses/idna_mapping-LICENSE-MIT.txt`, `idna_mapping-LICENSE-UNICODE.txt` |
| tinyvec (IDNA backend dependency) | v1.13.3 | `third_party/licenses/tinyvec-LICENSE-APACHE.txt`, `third_party/licenses/tinyvec-LICENSE-MIT.txt`, `third_party/licenses/tinyvec-LICENSE-ZLIB.txt` |
| unicode-bidi (IDNA backend dependency) | v0.3.18 | `third_party/licenses/unicode-bidi-LICENSE-APACHE.txt`, `third_party/licenses/unicode-bidi-LICENSE-MIT.txt` |
| unicode-joining-type (IDNA backend dependency) | v1.0.0 | `third_party/licenses/unicode-joining-type-LICENSE.txt` |
| unicode-normalization (IDNA backend dependency) | v0.1.25 | `third_party/licenses/unicode-normalization-LICENSE-APACHE.txt`, `third_party/licenses/unicode-normalization-LICENSE-MIT.txt` |
| utf8_iter (IDNA backend dependency) | v1.0.4 | `third_party/licenses/utf8_iter-LICENSE-APACHE.txt`, `third_party/licenses/utf8_iter-LICENSE-MIT.txt` |
| libproc (candidate macOS process identity) | v0.14.11 | `third_party/licenses/libproc-LICENSE.txt` |
| uzers (candidate native service account lookup) | v0.12.2, default features disabled | `third_party/licenses/uzers-LICENCE.txt` |
| Go Unicode category ranges | Unicode 15.0.0, generated from Go 1.26.2 for Rust workspace slugs | `third_party/licenses/go-unicode-LICENSE.txt` |
| Go x/sys | v0.38.0 | `third_party/licenses/x-sys-LICENSE.txt` |
| Go x/term | v0.37.0 | `third_party/licenses/x-term-LICENSE.txt` |
| SherClockHolmes/webpush-go | v1.4.0 | `third_party/licenses/webpush-go-LICENSE.txt` |
| golang-jwt/jwt | v5.3.0 | `third_party/licenses/jwt-LICENSE.txt` |
| Go x/crypto | v0.45.0 | `third_party/licenses/x-crypto-LICENSE.txt` |
| Token Terrier collector and candidate Rust usage port | in-tree Go snapshot; bounded parsers/state port in `crates/hmux-usage` | `third_party/token-terrier-server/LICENSE`, `NOTICE`, `UPSTREAM.md` |
| xterm.js | 6.0.0 | `web/public/licenses/xterm-LICENSE.txt` |
| xterm addon-fit | 0.11.0 | `web/public/licenses/xterm-addon-fit-LICENSE.txt` |
| xterm addon-web-links | 0.12.0 | `web/public/licenses/xterm-addon-web-links-LICENSE.txt` |
| es-hangul | package-lock.json | `web/public/licenses/` |
| marked | package-lock.json | `web/public/licenses/marked-LICENSE.txt` |
| entities | package-lock.json | `web/public/licenses/entities-LICENSE.txt` |
| Pretendard Variable | v1.3.9, unmodified | `web/public/fonts/Pretendard-LICENSE.txt`, `NOTICE.txt` |
| Terminal palettes: Tokyo Night Storm, Catppuccin Mocha, Dracula, Nord | pinned upstream terminal ports | `web/public/licenses/terminal-themes-NOTICE.md` and linked license texts |
| Monatendard / underlying fonts | bundled files | `web/public/fonts/NOTICE.txt`, `THIRD_PARTY_NOTICES.md` and license texts |

Web builds copy public font and JavaScript notices into the served assets.
`deploy/web/build.sh` includes Go runtime notices in its deployment archive.
`deploy/web/build-rust.sh` generates the native Rust notice inventory offline from
the locked dependency sources and refuses absent licenses or unexpected sources.

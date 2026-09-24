# Third-party components

HMux retains each component's license; a top-level source license does not
replace those terms. The tables below cover direct runtime dependencies and
bundled assets. The web package lockfile and `Cargo.lock` record the
versions used by their respective runtimes. Native Rust bundles include
`licenses/rust/INDEX.json`: host/target transitive normal/build dependency lists,
declared license expressions and hashes of retained upstream notices. Development-only
dependencies are excluded; build dependencies are included conservatively. The
bundle also retains Rust standard-library copyright/license texts and ring's
per-source copyright blocks. `deny.toml` defines the dependency license/source policy.

| Component | Version/source | License location |
| --- | --- | --- |
| quick-xml (Rust Home metric parser) | v0.41.0, default features disabled | `third_party/licenses/quick-xml-LICENSE-MIT.txt` |
| idna (Rust Home proxy bypass) | v1.1.0 | `third_party/licenses/idna-LICENSE-MIT.txt` |
| idna_adapter (pinned Unicode Rust backend) | v1.1.0 | `third_party/licenses/idna_adapter-LICENSE-MIT.txt` |
| idna_mapping (Unicode mapping tables) | v1.1.0 | `third_party/licenses/idna_mapping-LICENSE-MIT.txt`, `idna_mapping-LICENSE-UNICODE.txt` |
| tinyvec (IDNA backend dependency) | v1.13.3 | `third_party/licenses/tinyvec-LICENSE-APACHE.txt`, `third_party/licenses/tinyvec-LICENSE-MIT.txt`, `third_party/licenses/tinyvec-LICENSE-ZLIB.txt` |
| unicode-bidi (IDNA backend dependency) | v0.3.18 | `third_party/licenses/unicode-bidi-LICENSE-APACHE.txt`, `third_party/licenses/unicode-bidi-LICENSE-MIT.txt` |
| unicode-joining-type (IDNA backend dependency) | v1.0.0 | `third_party/licenses/unicode-joining-type-LICENSE.txt` |
| unicode-normalization (IDNA backend dependency) | v0.1.25 | `third_party/licenses/unicode-normalization-LICENSE-APACHE.txt`, `third_party/licenses/unicode-normalization-LICENSE-MIT.txt` |
| utf8_iter (IDNA backend dependency) | v1.0.4 | `third_party/licenses/utf8_iter-LICENSE-APACHE.txt`, `third_party/licenses/utf8_iter-LICENSE-MIT.txt` |
| libproc (macOS process identity) | v0.14.11 | `third_party/licenses/libproc-LICENSE.txt` |
| uzers (native service account lookup) | v0.12.2, default features disabled | `third_party/licenses/uzers-LICENCE.txt` |
| Go Unicode category ranges | Unicode 15.0.0, generated from Go 1.26.2 for Rust workspace slugs | `third_party/licenses/go-unicode-LICENSE.txt` |
| Token Terrier Rust usage port | bounded parsers/state port in `crates/hmux-usage` | `third_party/token-terrier-server/LICENSE`, `NOTICE`, `UPSTREAM.md` |
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
`deploy/web/build.sh` generates the native Rust notice inventory offline from
locked dependency sources and refuses absent licenses or unexpected sources.
Token Terrier attribution and generated Go Unicode table attribution remain for
derived Rust code/data; their former Go runtime sources are no longer bundled.

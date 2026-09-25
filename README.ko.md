# HMux

[English](README.md) | [한국어](README.ko.md)

**Low memory, web terminal for AI agents.**

AI 에이전트를 위한 저메모리 웹 터미널입니다.

HMux는 직접 관리하는 호스트에서 터미널 세션을 실행하고, 데스크톱·휴대전화·태블릿의
가벼운 웹 인터페이스로 연결합니다. 브라우저를 닫거나 기기를 바꾸고 나중에 다시
접속해도 작업은 호스트에서 계속 실행됩니다. 사용자 인터페이스는 웹/PWA로만 제공합니다.

낮은 메모리 사용량과 간단한 설치가 핵심 요구사항입니다. 네이티브 바이너리,
공유 수집기, 크기가 제한된 터미널 버퍼를 사용합니다. 에이전트 CLI, tmux, 인증 정보와
작업 폴더는 호스트에 두며, Docker가 필요하지 않습니다.

## 메모리

격리된 Linux 환경에서 보관한 배포 산출물을 비교했을 때, Rust + Protobuf의
Gateway/Home 합산 메모리는 Go + JSON보다 **55~57% 적었습니다**.
각 구성 3회 측정한 PSS 중앙값입니다.

| Gateway + Home | Go | Rust |
| --- | ---: | ---: |
| 연결된 유휴 상태 | 24.88 MiB | 11.08 MiB |
| 터미널 에코 10,000회 후 | 25.65 MiB | 11.07 MiB |

합성 호스트 도구를 사용한 HMux 프로세스 측정이며, 브라우저·tmux·에이전트 CLI의
메모리는 제외했습니다. [측정 조건과 한계](bench/hmux/README.md#deployed-artifact-memory-comparison-2026-09-25)를 확인하세요.

## 작동 방식

```text
데스크톱 / 휴대전화 / 태블릿 브라우저
               │ HTTPS / WSS
               ▼
          Web Gateway
               ▲
               │ 호스트에서 나가는 WSS 연결
               │
         직접 관리하는 Home 호스트
          tmux ─┬─ Codex
               ├─ Claude Code
               └─ Shell
```

Home 호스트는 터미널 프로세스, 제공업체 인증 정보와 파일을 관리합니다. Gateway는
웹 인증과 연결을 처리하며 에이전트 작업을 실행하지 않습니다. 현재 검증된 운영 구성은
macOS Home과 Linux HTTPS Gateway입니다. 다른 운영체제에서 빌드된다는 사실만으로
동일한 수준의 지원이 검증된 것은 아닙니다.

## 주요 기능

- tmux 세션 유지, 데스크톱·모바일 탭, 검색과 재연결
- 비밀번호 로그인, 계정별 선택적 TOTP, 로그인 유지와 개별 로그인 세션 해제
- 반응형 웹/PWA, 한국어 입력, 선택·복사와 명시적인 링크 열기
- 영어·한국어 웹 UI와 설치기 — 기본 언어는 영어
- 호스트에 3시간 동안 보관되는 파일 첨부
- 제공업체 사용량, 호스트 시스템 사용량과 Codex/Claude 대화 보기
- 공유 호스트 상태와 재부팅 후 검증된 tmux/제공업체 세션 복구

웹 계정은 연결된 호스트의 터미널 접근 권한을 부여합니다. 여러 계정은 신뢰하는
협업자를 위한 기능이며, 호스트 파일과 셸 권한을 공유합니다. 서로 신뢰하지 않는
사용자를 격리하는 용도로 사용하면 안 됩니다. [보안 안내](SECURITY.md)를 확인하세요.

## 시작하기

```sh
git clone https://github.com/codemoo/hmux.git
cd hmux
```

[설치 가이드](docs/OPERATIONS.md)에 따라 사용할 호스트, Gateway, 도메인과 인증 정보를
구성하세요. 현재는 소스에서 빌드해 배포하며, 빌드에는 저장소에 지정된 Rust 도구체인,
Node.js 22 이상, Python 3이 필요합니다. 빌드된 네이티브 설치기를 실행할 때는
이 도구들이 필요하지 않습니다. Home에는 tmux와 사용할 제공업체 CLI가 필요합니다.
외부 접속에는 직접 운영하는 HTTPS Gateway와 연결 토큰이 필요합니다.

```sh
make build
```

현재 호스트용 묶음이 `dist/web-<platform>/`에 만들어집니다. 예를 들어 macOS ARM64는
`dist/web-darwin-arm64/`, Linux x86_64는 `dist/web-linux-amd64/`입니다.
빌드만으로 배포하거나 기존 세션을 변경하지 않습니다. 다른 대상을 빌드하려면 해당
Rust 타깃, 링커와 플랫폼 SDK를 준비한 뒤 `HMUX_RUST_TARGETS`를 지정하세요.
[빌드·설치 설명](docs/OPERATIONS.md#build-and-install)을 참고하세요.

사용할 플랫폼의 묶음에서 설치기를 실행합니다. 다음은 macOS ARM64 예시입니다.

```sh
./dist/web-darwin-arm64/hmux-web install --lang ko
```

**Gateway**, **Home**, **둘 다** 중 역할을 고르고, **이 장치** 또는 **SSH 원격 서버**를
선택합니다. Gateway는 Linux/systemd가 필요하며, Home은 macOS와 Linux를 지원합니다.
원격 설치는 대상 운영체제·CPU를 확인하고 해당 네이티브 묶음을 전송합니다.
Gateway HTTPS는 Nginx/Let's Encrypt 자동 구성 또는 기존 역방향 프록시를 사용할 수
있습니다. 첫 계정 생성과 TOTP 설정은 비공개 일회용 설정 토큰으로 웹에서 진행합니다.

웹 언어는 로그인 화면이나 **설정 → 터미널 → 언어**에서 바꿀 수 있습니다.
선택은 해당 브라우저에 저장되며, 터미널을 재연결하지 않고 화면에 반영됩니다.
설치기는 언어 선택을 제공하며, `--lang en` 또는 `--lang ko`로 미리 지정할 수 있습니다.
`HMUX_LANG` 환경 변수도 지원하고, 명령행 옵션이 우선합니다.

같은 호스트에 설치하면 연결 정보를 자동으로 전달합니다. 호스트를 나누면 비공개
연결 파일을 사용합니다. Home 설치에서는 작업 폴더(기본 `~/.hmux`)와 자동 시작 여부를
선택하며, 기존 경로와 에이전트 세션을 보존합니다.
[설치 옵션과 사전 조건](docs/OPERATIONS.md#build-and-install)을 확인하세요.

macOS 지원은 tmux를 실행하는 Home 호스트를 뜻합니다.
선택적으로 [Home 자동 시작 서비스](docs/OPERATIONS.md#automatic-home-startup-macos-and-linux)를
등록하면 로그인할 때 연결기를 시작하고, 종료되면 다시 실행하므로 터미널 창을
계속 열어둘 필요가 없습니다. macOS는 launchd, Linux는 systemd 사용자 서비스를 사용합니다.

Gateway, Home 연결기와 헬퍼는 Rust로 구현되어 있습니다. 과거 Go/Rust 비교 결과는
명시된 측정 조건 안에서만 유효하며, 장시간 운용이나 실제 기기 검증을 대신하지 않습니다.
[Rust 런타임 현황](docs/RUST_MIGRATION.md)을 참고하세요.

## 저장소 구성

| 경로 | 역할 |
| --- | --- |
| `web/` | 데스크톱·모바일 웹/PWA 인터페이스 |
| `crates/hmux-gateway/`, `crates/hmux-home/`, `crates/hmux-web/` | Gateway, Home 연결기와 네이티브 진입점 |
| `crates/hmux-agent/`, `crates/hmux-service/`, `crates/hmux-install/` | 관리 명령, 서비스 수명주기와 안전한 설치 |
| `crates/hmux-core/`, `crates/hmux-model/`, `crates/hmux-usage/` | 공통 계약, 자원 제한 처리와 Home 사용량 수집 |
| `proto/`, `tests/RUST.md` | 버전이 지정된 Home 프로토콜과 네이티브 검증 |
| `third_party/` | 외부 구성요소 출처와 라이선스 |
| `docs/` | 아키텍처, 설치, 보안과 검증 문서 |

## 개발과 릴리스

검증 방법은 [CONTRIBUTING.md](CONTRIBUTING.md), 변경 규칙은 [AGENTS.md](AGENTS.md)를
참고하세요. CI는 Rust 런타임, 네이티브 수명주기 테스트, 웹 클라이언트,
생성된 프로토콜 타입과 Rust 의존성 정책을 검사합니다.
소스 공개와 배포 절차는 [릴리스 문서](docs/RELEASING.md), 외부 구성요소 고지는
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)에 있습니다.

## 기여자

- [Kim Ji Yu (@Banal972)](https://github.com/Banal972) — Linux Home 시스템 사용량,
  제공업체 설정과 웹 세션 수명주기 개선

최초 공개 커밋은 기존 프로젝트를 구성요소별로 가져온 것으로, 이전 개발 이력을
재현하지 않습니다. 자세한 동작은 [문서 지도](docs/README.md), 검증 범위와 한계는
[검증 기록](docs/VALIDATION.md)을 확인하세요.

## 라이선스

HMux 소스 라이선스는 아직 결정되지 않았습니다. 저장소가 공개되어 있다는 사실만으로
재사용 권한이 부여되지는 않습니다. 포함된 외부 구성요소에는 각자의 라이선스와 고지가
적용됩니다. [소스 라이선스 정책](docs/RELEASING.md#source-license)을 확인하세요.

# Android 터미널 엔진 검토

검토일: 2026-09-09. 공식 저장소 README, 라이선스, 소스와 GitHub API를
확인한 정적 검토다. Android 빌드·실기기 입력·성능 시험은 수행하지 않았다.
Android 앱 구현이나 엔진 채택이 완료되었다는 의미는 아니다.

## 현재 상태

이 문서는 네이티브 Android 엔진 후보의 역사적 검토다. 이후 제품 방향은
TypeScript·xterm.js 기반 웹/PWA로 정해졌으며 현재 Android 지원도 이 경로다.
현행 구현·운영 지침은 [Web HMux](WEB.md)를 따른다. 아래 권고는 네이티브
Android 앱을 별도로 추진할 때에만 다시 검토한다.

## 당시 권고

**ConnectBot termlib를 첫 프로토타입 후보로 삼는다.** Compose UI와 SSH 바이트
스트림을 분리한 구조가 HMux에 적합하다. 단, 한글 IME, tmux 마우스,
bracketed paste 검증을 통과해야 채택한다. 통과하지 못하면 Termux의
terminal-emulator/terminal-view를 비교한다. xterm.js는 WebView 대안,
libghostty-vt는 장기 후보로 둔다.

| 후보 | 장점 | 비용·불확실성 | 판단 |
| --- | --- | --- | --- |
| ConnectBot termlib | Compose Canvas, libvterm JNI, CJK·결합 문자, 터치 선택·확대, Typeface·팔레트 API | IME 수정 전송 방식, 마우스·paste 연결 보완 필요, NDK 의존 | 우선 실험 |
| Termux emulator/view | Android 터미널 사용 사례, IME·고정폭 폰트 처리 코드 | View가 로컬 프로세스 기반 TerminalSession에 결합; SSH 직접 연결용 분리 필요 | 네이티브 대안 |
| xterm.js + WebView | tmux·마우스, CJK·IME, 테마·폰트·검색 addon, MIT | 모바일 WebView IME·포커스·키보드 resize·JS bridge 검증 필요 | 웹 기반 대안 |
| libghostty-vt | macOS와 같은 계열의 VT 상태·입력 인코딩, MIT | C API가 불안정하다고 명시; Android 렌더러·IME·선택·JNI 구현 필요 | 첫 버전에서는 보류 |

## 소스에서 확인한 내용

### ConnectBot termlib

- 확인 commit: `735c6dc646a845099f34083a15bd295be145a6ce`.
  GitHub 최근 release 조회 결과는 `0.0.36` (2026-04-25).
  이하 기능 확인은 main 소스 기준이며 해당 release에 모두 포함된다는 뜻은 아니다.
- Apache-2.0, 포함 libvterm은 MIT. compileSdk 36 / minSdk 24,
  Java 17, CMake/JNI 및 Android 4개 ABI가 빌드 설정에 있다.
- `Terminal`에 `Typeface`, 글자 크기 범위, 전경·배경·선택색,
  키보드 활성화, 포커스, 붙여넣기 요청 콜백이 있다.
- `TerminalEmulator`에 바이트 입력, 키 입력 출력 콜백, resize 콜백,
  `setAnsiPalette` / `applyColorScheme`이 있다. 기존 Flexoki 16색을 전달할 수 있다.
- 루트 README는 paste를 planned로 표시하지만 현재 UI에는 paste 요청
  콜백이 있다. 콜백 존재만으로 bracketed paste 구현 완료라고 판단하지 않는다.
- 확인한 Kotlin emulator/native 공개 API에는 mouse dispatch 및 전용
  bracketed paste API가 보이지 않았다. libvterm 기능과 Android에 노출된
  기능을 구분해야 한다. 필요하면 좁은 JNI/API 확장을 유지해야 한다.
- 일반 IME 입력 경로에서 조합 문자열 변경 시 이전 문자열 길이만큼
  backspace를 보내고 새 문자열을 쓰는 코드가 있다. 한글·surrogate pair·
  원격 TUI의 편집 방식에 대한 실제 호환성은 미검증이다. 별도 compose
  입력 모드도 있으므로 한글 작성용 입력창과 직접 키 입력을 각각 시험한다.
- JNI 콜백에서 emulator 메서드로 재진입하면 deadlock 위험이 있다는
  문서 경고가 있다. transport 송신·resize는 큐로 넘겨 재진입하지 않는다.

### Termux

- 확인 commit: `3b66f8799635a4dba4a206563048ff0e6792c487`.
- `TerminalView`의 InputConnection·Typeface 설정을 확인했다.
  `TerminalSession`은 final이며 JNI로 로컬 subprocess/PTY를 생성한다.
  원격 SSH 스트림만 주입하는 재사용에는 어댑터 또는 코드 분리가 필요하다.
- 저장소 LICENSE는 GPLv3-only이고 terminal-view/emulator에 사용된
  Android Terminal Emulator 유래 코드의 Apache-2.0 예외를 명시한다.
  **전체 Termux 앱을 가져오는 것과 일부 소스를 재사용하는 것은 다르다.**
  실제 채택 파일·수정분의 라이선스를 확인하기 전 모듈 전체를 Apache라고
  단정하지 않는다. 개인 전용 사용만을 이유로 기술 후보에서 배제하지 않는다.

### xterm.js / Ghostty

- xterm.js 확인 commit: `c58ea3637f3968e0e6e79cd92cf9aace7ef89ee2`.
  최근 release 조회 결과 `6.0.0` (2025-12-22). README의 CJK·IME 지원은
  Android WebView 실기기 품질 보증이 아니다. 사용한다면 로컬 번들만
  로드하고 SSH는 네이티브 계층에 둔다. 원격 문자열을 HTML로 삽입하지 않는다.
- Ghostty 확인 commit: `448062571c5edf010b7490d06869b88b5ebf8f80`.
  `include/ghostty/vt.h`는 API가 incomplete/work-in-progress라고 명시한다.
  terminal state/render-state API가 Android용 완성 화면을 제공하는 것은 아니다.
  macOS의 Ghostty surface를 그대로 Android에 붙이는 방안으로 취급하지 않는다.
- 네 저장소 모두 archived=false이며 2026년 9월 push가 확인되었다.
  최근 push만으로 안정성이나 모바일 품질을 평가하지 않는다.

## HMux 연결 구조

Compose 화면 → 엔진 어댑터 → SSH PTY 채널 → 기존 Home agent/tmux 경로.
카탈로그·대화·복원·사용량은 Home의 기존 공통 구현이 담당한다.
엔진은 화면·입력·색상만 담당하고 provider session ID를 추측하지 않는다.

현재 Go 클라이언트에는 외부 `ssh` 실행 의존성이 있으므로 Android에서 그대로
실행할 수 있다고 가정하지 않는다. SSH 전송 구현은 별도 선정하고,
ProxyJump에 해당하는 중계 채널·Home 호스트 키 검증·agent forwarding 금지를
유지한다. 엔진 선정과 SSH 라이브러리 선정은 별개다.

연결은 `{id, created_at}`와 검증된 복원 lineage를 사용한다. 모바일 연결이
데스크톱을 detach하지 않도록 기존 grouped view 경로를 재사용한다.
공유 window 크기에 미치는 영향은 실험으로 확인한다. 일반 CLI의 기본
`attach-session -d` 동작을 모바일 앱에 무심코 재사용하지 않는다.

엔진 상태는 Compose 재구성으로 새로 만들지 않는다. 포커스와 IME를 활성
세션 하나에만 연결하고, 검색·대화 보기 전환 시 터미널 geometry를 유지한다.
회전·키보드 표시 시 안정된 행/열을 SSH window-change로 전달한다.
네트워크 복귀는 같은 tmux 재접속이며 Codex/Claude 재실행 명령이 아니다.

## 채택 전 통과 기준

1. Samsung Keyboard와 Gboard에서 `한글 입력 테스트`, 받침 수정, 조합 중
   삭제·커서 이동·Enter·탭 전환을 반복한다. 중복·누락·다른 탭 전송이 없어야 한다.
2. Flexoki 16색·truecolor·선택·커서, 고정폭 폰트·한글 fallback·Nerd Font,
   박스 문자·이모지·결합 문자를 확인한다. CJK 2칸 정렬이 tmux와 일치해야 한다.
3. 격리된 `hmux-e2e-*` tmux에서 pane 선택, drag, wheel, copy-mode와
   앱 자체 텍스트 선택을 구분한다. 마우스 지원을 TERM 이름만으로 광고하지 않는다.
4. bracketed paste on/off를 확인하고 여러 줄 붙여넣기가 의도치 않게
   명령 실행으로 바뀌지 않는지 검사한다. 키보드의 Ctrl/Esc/Tab/방향키도 포함한다.
5. 키보드 열기·닫기, 회전, 100회 세션 전환 후에도 입력·선택과 행/열이
   정확해야 한다. tmux·Codex/Claude TUI redraw, alternate screen도 검사한다.
6. 가짜 ANSI 대량 출력에서 10분간 입력 지연·프레임·메모리를 측정한다.
   동일 기기에서 후보를 비교하며 메모리의 지속 증가나 ANR이 없어야 한다.
7. 화면 잠금·Wi-Fi/이동통신 전환·Android 프로세스 종료 후 같은 tmux로
   복귀하고, 기존 Mac 접속과 작업을 종료하거나 provider를 중복 실행하지 않는다.

이 검토에서는 기존 tmux에 접근하지 않았고, 앱 코드·설정·설치를 변경하지 않았다.

## 근거

- [termlib 소스 및 README](https://github.com/connectbot/termlib/tree/735c6dc646a845099f34083a15bd295be145a6ce)
- [termlib IME](https://github.com/connectbot/termlib/blob/735c6dc646a845099f34083a15bd295be145a6ce/lib/src/main/java/org/connectbot/terminal/ImeInputView.kt)
- [termlib emulator API](https://github.com/connectbot/termlib/blob/735c6dc646a845099f34083a15bd295be145a6ce/lib/src/main/java/org/connectbot/terminal/TerminalEmulator.kt)
- [Termux 소스·라이선스](https://github.com/termux/termux-app/tree/3b66f8799635a4dba4a206563048ff0e6792c487)
- [xterm.js 소스·지원 범위](https://github.com/xtermjs/xterm.js/tree/c58ea3637f3968e0e6e79cd92cf9aace7ef89ee2)
- [libghostty-vt API 상태](https://github.com/ghostty-org/ghostty/blob/448062571c5edf010b7490d06869b88b5ebf8f80/include/ghostty/vt.h)
- [HMux 현행 구조](ARCHITECTURE.md), [테마](THEME.md)

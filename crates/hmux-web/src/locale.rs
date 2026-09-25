//! Installer presentation language. Protocol, option and configuration values stay English.
use crate::enroll::line_prompt;
use std::{
    ffi::OsString,
    io,
    sync::atomic::{AtomicU8, Ordering},
};
use tokio_util::sync::CancellationToken;

static LANGUAGE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    English,
    Korean,
}
impl Language {
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Korean => "ko",
        }
    }
    fn parse(value: &str) -> io::Result<Self> {
        match value {
            "en" => Ok(Self::English),
            "ko" => Ok(Self::Korean),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--lang and HMUX_LANG must be en or ko",
            )),
        }
    }
}
pub fn current() -> Language {
    if LANGUAGE.load(Ordering::Relaxed) == 1 {
        Language::Korean
    } else {
        Language::English
    }
}
pub fn korean() -> bool {
    current() == Language::Korean
}
fn set(value: Language) {
    LANGUAGE.store(u8::from(value == Language::Korean), Ordering::Relaxed);
}

/// Remove only the installer language switch; all other arguments keep their order.
pub fn configure(args: &[OsString]) -> io::Result<(Vec<OsString>, bool)> {
    let mut cleaned = Vec::with_capacity(args.len());
    let mut chosen = None;
    let mut index = 0;
    while index < args.len() {
        let Some(raw) = args[index].to_str() else {
            // Installation paths are Unix OsStrings. Only the language switch
            // and its value require UTF-8; preserve all other arguments exactly.
            cleaned.push(args[index].clone());
            index += 1;
            continue;
        };
        if raw == "--lang" || raw.starts_with("--lang=") {
            if chosen.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--lang specified more than once",
                ));
            }
            let value = if let Some(value) = raw.strip_prefix("--lang=") {
                value
            } else {
                index += 1;
                args.get(index).and_then(|a| a.to_str()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--lang requires en or ko")
                })?
            };
            chosen = Some(Language::parse(value)?);
        } else {
            cleaned.push(args[index].clone());
        }
        index += 1;
    }
    let env = if chosen.is_some() {
        None
    } else {
        std::env::var_os("HMUX_LANG")
            .map(|value| Language::parse(value.to_str().unwrap_or("")))
            .transpose()?
    };
    set(chosen.or(env).unwrap_or(Language::English));
    Ok((cleaned, chosen.is_some() || env.is_some()))
}

pub fn choose(stop: &CancellationToken) -> io::Result<()> {
    loop {
        match line_prompt(stop, "Language / 언어 [1 English (default) / 2 한국어]: ")?.as_str()
        {
            "" | "1" | "en" => {
                set(Language::English);
                return Ok(());
            }
            "2" | "ko" => {
                set(Language::Korean);
                return Ok(());
            }
            _ => println!("Choose 1 or 2. / 1 또는 2를 선택하세요."),
        }
    }
}

pub fn tr(english: &'static str) -> &'static str {
    if !korean() {
        return english;
    }
    match english {
        "Low memory, web terminal for AI agents." => "AI 에이전트를 위한 저메모리 웹 터미널입니다.",
        "Install" => "설치",
        "Let's Encrypt subscriber terms:" => "Let's Encrypt 이용 약관:",
        "Update native executables. Keep configuration and services as they are." => "네이티브 실행 파일을 업데이트합니다. 설정과 서비스는 그대로 유지합니다.",
        "Your agents run here. Your browser connects from anywhere." => "에이전트는 여기에서 실행됩니다. 브라우저에서는 어디서든 연결할 수 있습니다.",
        "Enter a DNS hostname without https://, a path or a port." => "https://, 경로, 포트가 없는 DNS 호스트 이름을 입력하세요.",
        "Enter a valid email address." => "유효한 이메일 주소를 입력하세요.",
        "The domain must point here, with ports 80 and 443 reachable. Existing unrelated Nginx sites are retained." => "도메인이 이 서버를 가리켜야 하고 80/443 포트에 접속할 수 있어야 합니다. 관련 없는 기존 Nginx 사이트는 유지합니다.",
        "Your existing HTTPS proxy must forward HTTP and WebSocket upgrades to 127.0.0.1:8088 on this server. HMux will not change that proxy." => "기존 HTTPS 프록시는 HTTP 및 WebSocket 연결을 이 서버의 127.0.0.1:8088로 전달해야 합니다. HMux는 프록시를 변경하지 않습니다.",
        "Gateway installation creates a dedicated service account, private credentials and a systemd service." => "Gateway 설치는 전용 서비스 계정, 비공개 인증 정보, systemd 서비스를 만듭니다.",
        "Home setup follows under your current user, with the Gateway connection filled in automatically." => "이후 현재 사용자로 Home을 설정하며 Gateway 연결 정보가 자동으로 입력됩니다.",
        "Use an SSH host alias or user@hostname. Configure ports and keys in SSH config." => "SSH 호스트 별칭 또는 user@hostname을 입력하세요. 포트와 키는 SSH 설정에서 구성하세요.",
        "Installation cancelled. No Gateway or Home configuration was changed." => "설치를 취소했습니다. Gateway 또는 Home 설정을 변경하지 않았습니다.",
        "Private Home connection file:" => "비공개 Home 연결 파일:",
        "This file grants access to the Home connector. Transfer it only over a trusted private channel; never publish it." => "이 파일은 Home 연결 권한을 부여합니다. 신뢰할 수 있는 비공개 채널로만 전송하고 공개하지 마세요.",
        "On your Home machine, run the bundle's hmux-web install --role home --connection-file /PRIVATE/home-connection.json" => "Home 장치에서 번들의 hmux-web install --role home --connection-file /PRIVATE/home-connection.json 명령을 실행하세요.",
        "Private connection file retained at" => "비공개 연결 파일 보관 위치:",
        "Gateway setup finished; Home setup did not finish. The Gateway and private connection file were retained. Resume with install --role home --connection-file using that file." => "Gateway 설정은 완료됐지만 Home 설정은 완료되지 않았습니다. Gateway와 비공개 연결 파일은 유지했습니다. 해당 파일로 install --role home --connection-file을 실행해 재개하세요.",
        "Keep your private connection file. Pass --connection-file to a later guided installation to connect without retyping its settings." => "비공개 연결 파일을 보관하세요. 나중에 안내 설치에서 --connection-file을 지정하면 설정을 다시 입력하지 않고 연결할 수 있습니다.",
        "unchanged; restart separately to use this version" => "변경 없음; 이 버전을 사용하려면 별도로 다시 시작하세요",
        "Checking SSH target. Its host key must already be verified in known_hosts." => "SSH 대상을 확인합니다. 호스트 키가 known_hosts에서 이미 검증되어 있어야 합니다.",
        "Remote platform:" => "원격 플랫폼:",
        "Local path to the extracted" => "압축을 푼",
        "bundle:" => "번들의 로컬 경로:",
        "Transferring verified installation files…" => "검증한 설치 파일을 전송합니다…",
        "Private Home connection file saved locally:" => "비공개 Home 연결 파일의 로컬 저장 위치:",
        "Use it with install --role home --connection-file FILE on the Home host. It grants connector access; do not publish it." => "Home 호스트에서 install --role home --connection-file FILE과 함께 사용하세요. 연결 권한을 부여하므로 공개하지 마세요.",
        "Provision the Linux Gateway service. Use --lang en|ko to select installer messages." => "Linux Gateway 서비스를 설치합니다. 설치 메시지 언어는 --lang en|ko로 선택하세요.",
        "gateway installation failed and rollback is incomplete" => "Gateway 설치에 실패했고 롤백이 완료되지 않았습니다",
        "Gateway service active on 127.0.0.1:8088. Configured public URL:" => "Gateway 서비스가 127.0.0.1:8088에서 실행 중입니다. 공개 URL:",
        "Configure the existing HTTPS reverse proxy to forward HTTP and WebSocket upgrades to 127.0.0.1:8088; forward Host, X-Real-IP and X-Forwarded-Proto=https; expose /connect over WSS. Keep the upstream loopback-only." => "기존 HTTPS 역방향 프록시가 HTTP와 WebSocket 업그레이드를 127.0.0.1:8088로 전달하도록 설정하세요. Host, X-Real-IP, X-Forwarded-Proto=https를 전달하고 /connect를 WSS로 노출하세요. 업스트림은 루프백에만 바인딩하세요.",
        "Private Home connection file created at" => "비공개 Home 연결 파일 생성 위치:",
        "Installer cleanup exceeded its deadline. Check the target installation before retrying." => "설치 정리 작업 시간이 초과됐습니다. 다시 시도하기 전에 대상 설치를 확인하세요.",
        "Prepare private first-login setup. Create the account and configure TOTP in the browser. The one-time setup token is stored at <credentials-file>.bootstrap; existing accounts are never replaced." => "첫 로그인용 비공개 설정을 준비합니다. 브라우저에서 계정을 만들고 TOTP를 설정하세요. 일회용 설정 토큰은 <credentials-file>.bootstrap에 저장되며 기존 계정은 교체하지 않습니다.",
        "Private web setup prepared. Start the Gateway and open its HTTPS site to create the first account." => "비공개 웹 설정을 준비했습니다. Gateway를 시작하고 HTTPS 사이트에서 첫 계정을 만드세요.",
        "Enter y or n." => "y 또는 n을 입력하세요.",
        "Choose 1 or 2." => "1 또는 2를 선택하세요.",
        "Choose 1, 2 or 3." => "1, 2, 3 중 하나를 선택하세요.",
        "Installation role [1/2/3]: " => "설치 역할 [1/2/3]: ",
        "Installation target [1/2]: " => "설치 대상 [1/2]: ",
        "SSH host alias or user@hostname: " => "SSH 호스트 별칭 또는 user@hostname: ",
        "Gateway connection file [Enter to enter connection details later]: " => "Gateway 연결 파일 [나중에 연결하려면 Enter]: ",
        "Gateway domain (for example hmux.example.com): " => "Gateway 도메인 (예: hmux.example.com): ",
        "HTTPS mode [1/2]: " => "HTTPS 방식 [1/2]: ",
        "Email for certificate renewal notices: " => "인증서 갱신 알림 이메일: ",
        "Accept the certificate subscriber terms? [y/N]: " => "인증서 이용 약관에 동의하시나요? [y/N]: ",
        "Install missing Nginx/Certbot packages using apt? [y/N]: " => "apt로 누락된 Nginx/Certbot 패키지를 설치할까요? [y/N]: ",
        "Install Gateway with these settings? [y/N]: " => "이 설정으로 Gateway를 설치할까요? [y/N]: ",
        "New-session base directory [~/.hmux]: " => "새 세션 기본 디렉터리 [~/.hmux]: ",
        "Set up automatic startup now? [y/N]: " => "지금 자동 시작을 설정할까요? [y/N]: ",
        "Gateway address (https://...): " => "Gateway 주소 (https://...): ",
        "Use an HTTPS site address or wss://host/connect, without a query or fragment." => "쿼리나 조각 없이 HTTPS 사이트 주소 또는 wss://host/connect를 입력하세요.",
        "Use the private token file copied from your Gateway. Do not paste the token here." => "Gateway에서 복사한 비공개 토큰 파일을 사용하세요. 토큰 값은 여기에 붙여 넣지 마세요.",
        "Token file unavailable. Use an absolute path or ~/ and a private, valid token file." => "토큰 파일을 사용할 수 없습니다. 절대 경로나 ~/의 유효한 비공개 토큰 파일을 사용하세요.",
        "Connector token file" => "연결 토큰 파일",
        "Home installed" => "Home 설치 완료",
        "Check installation files" => "설치 파일 확인",
        "Choose your workspace" => "작업 공간 선택",
        "Connect your Home" => "Home 연결",
        "Install Home" => "Home 설치",
        "Binaries" => "실행 파일",
        "Configuration" => "설정",
        "Running Home" => "실행 중인 Home",
        "Automatic start" => "자동 시작",
        "found" => "발견됨",
        "missing; install before connecting" => "없음; 연결 전에 설치하세요",
        "optional; not found" => "선택 사항; 없음",
        "unchanged" => "변경 없음",
        "requested" => "요청됨",
        "not changed" => "변경하지 않음",
        "Provider login stays with your existing CLI account." => "제공자 로그인은 기존 CLI 계정에서 유지됩니다.",
        "Check your Home process:" => "Home 프로세스를 확인하세요:",
        "Then open your Gateway's HTTPS address in a browser." => "브라우저에서 Gateway의 HTTPS 주소를 여세요.",
        "Service registration alone does not confirm a Gateway connection." => "서비스 등록만으로 Gateway 연결이 확인되지는 않습니다.",
        "Connect when your Gateway and private token file are ready:" => "Gateway와 비공개 토큰 파일이 준비되면 연결하세요:",
        "A Gateway with HTTPS and a private connector token is needed for remote access." => "원격 접속에는 HTTPS Gateway와 비공개 연결 토큰이 필요합니다.",
        "You can finish the local installation now and connect later." => "로컬 설치를 마치고 나중에 연결할 수 있습니다.",
        "  Automatic startup requested by --enable-service." => "  --enable-service로 자동 시작을 요청했습니다.",
        "  Adopting the sole running Home connector and its connection settings." => "  실행 중인 유일한 Home 연결 프로세스와 연결 설정을 사용합니다.",
        "  Using the address and private token from your Gateway connection file." => "  Gateway 연결 파일의 주소와 비공개 토큰을 사용합니다.",
        "Installed hmux-web and hmux-agent" => "hmux-web 및 hmux-agent 설치 완료",
        "Home configured; existing paths are preserved unless --workspace-dir is supplied." => "Home 설정 완료. --workspace-dir을 지정하지 않으면 기존 경로를 유지합니다.",
        "  Existing configuration found. Keeping your workspace paths." => "  기존 설정을 찾았습니다. 작업 공간 경로를 유지합니다.",
        "  Private Gateway connection file validated. Token contents stay hidden." => "  비공개 Gateway 연결 파일을 확인했습니다. 토큰 값은 표시하지 않습니다.",
        "installation cancelled" => "설치가 취소되었습니다",
        "installation command failed; see the message above" => "설치 명령이 실패했습니다. 위 메시지를 확인하세요",
        "installation command timed out" => "설치 명령 시간이 초과되었습니다",
        "installer child output exceeds limit" => "설치 하위 명령의 출력이 제한을 초과했습니다",
        "--lang and HMUX_LANG must be en or ko" => "--lang과 HMUX_LANG에는 en 또는 ko를 지정해야 합니다",
        "role must be all, gateway or home" => "역할은 all, gateway 또는 home이어야 합니다",
        "Gateway provisioning requires Linux with systemd; use Home on macOS and install Gateway on your Linux server" => "Gateway 설치에는 systemd가 있는 Linux가 필요합니다. macOS에서는 Home을 사용하고 Linux 서버에 Gateway를 설치하세요",
        "run Home or combined installation as your normal tmux/provider user, without sudo; Gateway elevation is handled separately" => "Home 또는 통합 설치는 sudo 없이 일반 tmux/제공자 사용자로 실행하세요. Gateway 권한 상승은 별도로 처리합니다",
        "installer arguments exceed limit" => "설치 인수가 제한을 초과했습니다",
        "installation arguments exceed limit" => "설치 인수가 제한을 초과했습니다",
        "duplicate or invalid --local" => "중복되거나 잘못된 --local 옵션입니다",
        "invalid installer option" => "잘못된 설치 옵션입니다",
        "--local and --remote cannot be combined" => "--local과 --remote는 함께 사용할 수 없습니다",
        "--connection-output requires a local Gateway role" => "--connection-output에는 로컬 Gateway 역할이 필요합니다",
        "--connection-file is for Home-only installation" => "--connection-file은 Home 전용 설치에만 사용합니다",
        "--workspace-dir is for Home or combined installation" => "--workspace-dir은 Home 또는 통합 설치에만 사용합니다",
        "workspace configuration requires a Home role" => "작업 공간 설정에는 Home 역할이 필요합니다",
        "install requires an interactive terminal; use install-home or install-gateway for automation" => "install 명령에는 대화형 터미널이 필요합니다. 자동화에는 install-home 또는 install-gateway를 사용하세요",
        "run Home installation as the tmux/provider user without sudo" => "Home 설치는 sudo 없이 tmux/제공자 사용자로 실행하세요",
        "--guided requires an interactive terminal; use explicit flags for automation" => "--guided에는 대화형 터미널이 필요합니다. 자동화에는 명시적 옵션을 사용하세요",
        "candidate binary must be an owner-controlled executable" => "설치 후보 바이너리는 소유자가 관리하는 실행 파일이어야 합니다",
        "installation switches do not accept a value" => "설치 스위치에는 값을 지정할 수 없습니다",
        "--binaries-only cannot be combined with --enable-service" => "--binaries-only와 --enable-service는 함께 사용할 수 없습니다",
        "--guided cannot be combined with --binaries-only" => "--guided와 --binaries-only는 함께 사용할 수 없습니다",
        "--url and --token-file must be supplied together" => "--url과 --token-file은 함께 지정해야 합니다",
        "--url and --token-file require --enable-service" => "--url과 --token-file에는 --enable-service가 필요합니다",
        "--connection-file requires --guided or --enable-service and cannot combine with --binaries-only or explicit connection options" => "--connection-file에는 --guided 또는 --enable-service가 필요하며 --binaries-only나 명시적 연결 옵션과 함께 사용할 수 없습니다",
        "Gateway installation requires Linux" => "Gateway 설치에는 Linux가 필요합니다",
        "Gateway installation requires root; use the installer sudo wrapper" => "Gateway 설치에는 root 권한이 필요합니다. 설치 프로그램의 sudo 경로를 사용하세요",
        "a running systemd system manager is required for Gateway installation" => "Gateway 설치에는 실행 중인 systemd 시스템 관리자가 필요합니다",
        "trusted /usr/bin/sudo is required for Gateway provisioning" => "Gateway 설치에는 신뢰할 수 있는 /usr/bin/sudo가 필요합니다",
        "--domain must be a DNS hostname" => "--domain에는 DNS 호스트 이름을 지정해야 합니다",
        "--email must be a plain email address" => "--email에는 일반 이메일 주소를 지정해야 합니다",
        "--https must be managed or external" => "--https에는 managed 또는 external을 지정해야 합니다",
        "--domain is required" => "--domain은 필수입니다",
        "--https is required" => "--https는 필수입니다",
        "--email is required for managed HTTPS" => "managed HTTPS에는 --email이 필요합니다",
        "--accept-acme-terms is required" => "--accept-acme-terms가 필요합니다",
        "ACME/package options require managed HTTPS" => "ACME/패키지 옵션에는 managed HTTPS가 필요합니다",
        "a trusted system SSH client is required" => "신뢰할 수 있는 시스템 SSH 클라이언트가 필요합니다",
        "use a valid SSH host alias or user@hostname; configure ports/keys in SSH config" => "유효한 SSH 호스트 별칭 또는 user@hostname을 사용하세요. 포트와 키는 SSH 설정에서 구성하세요",
        "macOS remote hosts support Home only; Gateway requires Linux/systemd" => "macOS 원격 호스트는 Home만 지원합니다. Gateway에는 Linux/systemd가 필요합니다",
        "bundle platform does not match the remote host" => "번들 플랫폼이 원격 호스트와 일치하지 않습니다",
        "installation bundle checksum mismatch" => "설치 번들의 체크섬이 일치하지 않습니다",
        "invalid connection file" => "잘못된 연결 파일입니다",
        "connection file must be private, owner-controlled and valid; credentials were not changed" => "연결 파일은 비공개이며 소유자가 관리하는 유효한 파일이어야 합니다. 인증 정보는 변경하지 않았습니다",
        "Home already has a different or unsafe connector token; use a separate --config-dir or resolve the existing connection first" => "Home에 다른 연결 토큰 또는 안전하지 않은 토큰이 있습니다. 별도의 --config-dir을 사용하거나 기존 연결을 먼저 해결하세요",
        "--credentials and --token-file required" => "--credentials와 --token-file이 필요합니다",
        "initialization cancelled" => "초기화가 취소되었습니다",
        _ => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    #[test]
    fn language_flag_is_removed_without_changing_other_arguments() {
        let args = [
            "--role".into(),
            "home".into(),
            "--lang=ko".into(),
            "--local".into(),
        ];
        let (cleaned, explicit) = configure(&args).unwrap();
        assert!(explicit);
        assert_eq!(cleaned, ["--role", "home", "--local"]);
        assert_eq!(current(), Language::Korean);
        assert!(configure(&["--lang=fr".into()]).is_err());
        let path = OsString::from_vec(b"/synthetic/work-\xff".to_vec());
        let (cleaned, _) =
            configure(&["--lang=en".into(), "--workspace-dir".into(), path.clone()]).unwrap();
        assert_eq!(cleaned, [OsString::from("--workspace-dir"), path]);
        set(Language::English);
    }
}

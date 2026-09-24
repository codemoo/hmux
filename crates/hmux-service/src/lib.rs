#![forbid(unsafe_code)]
//! Bounded, private Home service building blocks. Lifecycle actions are separate.

pub use hmux_core::log;
mod files;
mod lifecycle;
pub mod process;
pub use lifecycle::{run, run_with_bundle, run_with_executable};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const LABEL: &str = "io.github.codemoo.hmux.home";
pub const UNIT: &str = "hmux-home.service";
pub const USAGE: &str = "usage: hmux-web service <install|status|start|stop|restart|uninstall> [--from-running | --url wss://host/connect --token-file /private/token] [--config /private/home.toml]";
const ENV_KEYS: &[&str] = &[
    "PATH",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMUX_TMPDIR",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub binary: String,
    pub home: String,
    pub endpoint: String,
    pub token_file: String,
    pub config_file: String,
    pub log_file: String,
    pub environment: BTreeMap<String, String>,
}

impl Spec {
    pub fn arguments(&self) -> Vec<String> {
        let mut args = vec!["/usr/bin/env".into(), "-i".into()];
        args.extend(
            self.environment
                .iter()
                .map(|(key, value)| format!("{key}={value}")),
        );
        args.extend([
            self.binary.clone(),
            "connect".into(),
            "--url".into(),
            self.endpoint.clone(),
            "--token-file".into(),
            self.token_file.clone(),
            "--log-file".into(),
            self.log_file.clone(),
        ]);
        if !self.config_file.is_empty() {
            args.extend(["--config".into(), self.config_file.clone()]);
        }
        args
    }
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

pub fn launch_agent(spec: &Spec) -> Vec<u8> {
    let mut out = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>\n");
    for arg in spec.arguments() {
        out.push_str(&format!("<string>{}</string>\n", xml_text(&arg)));
    }
    out.push_str(&format!(
        "</array>\n<key>WorkingDirectory</key><string>{}</string>\n",
        xml_text(&spec.home)
    ));
    out.push_str("<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>10</integer>\n<key>ExitTimeOut</key><integer>20</integer>\n<key>AbandonProcessGroup</key><true/>\n<key>ProcessType</key><string>Background</string>\n<key>Umask</key><integer>63</integer>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n");
    out.into_bytes()
}

pub fn unit_quote(value: &str, command: bool) -> String {
    let mut escaped = value.replace('%', "%%");
    if command {
        escaped = escaped.replace('$', "$$");
    }
    escaped = escaped.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub fn systemd_unit(spec: &Spec) -> Vec<u8> {
    let args = spec
        .arguments()
        .iter()
        .map(|arg| unit_quote(arg, true))
        .collect::<Vec<_>>()
        .join(" ");
    format!("[Unit]\nDescription=HMux Home connector\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart={args}\nWorkingDirectory={}\nRestart=always\nRestartSec=10\nTimeoutStopSec=20\nKillMode=process\nUMask=0077\nStandardOutput=null\nStandardError=null\n\n[Install]\nWantedBy=default.target\n", spec.home.replace('%', "%%")).into_bytes()
}

fn clean_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 32768 && !value.chars().any(char::is_control)
}

/// Build the exact service environment from a login environment or an adopted process.
/// `user` is the account name resolved by the lifecycle owner, never inherited from source.
pub fn service_environment_from(
    home: &Path,
    user: &str,
    source: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if !home.is_absolute() || !clean_value(&home.to_string_lossy()) || !clean_value(user) {
        return Err("invalid service account".into());
    }
    if source
        .get("HOME")
        .is_some_and(|value| !value.is_empty() && Path::new(value) != home)
    {
        return Err("connector HOME differs from the installation account; use the original account/environment".into());
    }
    let mut env = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("USER".into(), user.into()),
        ("LOGNAME".into(), user.into()),
    ]);
    for key in ENV_KEYS {
        if let Some(value) = source.get(*key).filter(|value| !value.is_empty()) {
            if !clean_value(value) {
                return Err(format!("invalid service environment value for {key}"));
            }
            if *key != "PATH"
                && *key != "LANG"
                && !key.starts_with("LC_")
                && !Path::new(value).is_absolute()
            {
                return Err(format!("{key} must be an absolute path"));
            }
            env.insert((*key).into(), value.clone());
        }
    }
    let path = env
        .get("PATH")
        .ok_or("PATH is required; install from the terminal where tmux and provider CLIs work")?;
    if path
        .split(':')
        .any(|part| !Path::new(part).is_absolute() || !clean_value(part))
    {
        return Err("service PATH must contain only absolute directories".into());
    }
    Ok(env)
}

pub fn executable_in_path(name: &str, path: &str) -> bool {
    if name.is_empty() || name.contains('/') {
        return false;
    }
    path.split(':').any(|dir| {
        let candidate = Path::new(dir).join(name);
        use std::os::unix::fs::PermissionsExt;
        candidate
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Status,
    Start,
    Stop,
    Restart,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub action: Action,
    pub endpoint: String,
    pub token_file: String,
    pub config_file: String,
    pub from_running: bool,
    pub binary: String,
}

/// Parse the service subcommand tail; all management options are install-only.
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    let Some(action) = args.first().map(String::as_str) else {
        return Err(USAGE.into());
    };
    let action = match action {
        "install" => Action::Install,
        "status" => Action::Status,
        "start" => Action::Start,
        "stop" => Action::Stop,
        "restart" => Action::Restart,
        "uninstall" => Action::Uninstall,
        _ => return Err(USAGE.into()),
    };
    let mut cmd = Command {
        action,
        endpoint: String::new(),
        token_file: String::new(),
        config_file: String::new(),
        from_running: false,
        binary: String::new(),
    };
    if args.len() > 128 || args.iter().map(String::len).sum::<usize>() > 65536 {
        return Err("service arguments exceed limit".into());
    }
    let mut index = 1;
    while index < args.len() {
        let raw = args[index]
            .strip_prefix("--")
            .or_else(|| args[index].strip_prefix('-'))
            .ok_or(USAGE)?;
        let (key, inline) = raw
            .split_once('=')
            .map_or((raw, None), |(k, v)| (k, Some(v)));
        if key == "from-running" {
            cmd.from_running = match inline.unwrap_or("true") {
                "true" | "1" | "t" | "TRUE" | "True" | "T" => true,
                "false" | "0" | "f" | "FALSE" | "False" | "F" => false,
                _ => return Err("invalid --from-running value".into()),
            };
        } else {
            let value = match inline {
                Some(value) => value,
                None => {
                    index += 1;
                    args.get(index).ok_or(USAGE)?.as_str()
                }
            };
            if !value.is_empty() && !clean_value(value) {
                return Err(USAGE.into());
            }
            match key {
                "url" => cmd.endpoint = value.into(),
                "token-file" => cmd.token_file = value.into(),
                "config" => cmd.config_file = value.into(),
                "binary" => cmd.binary = value.into(),
                _ => return Err(USAGE.into()),
            }
        }
        index += 1;
    }
    if action != Action::Install
        && (!cmd.endpoint.is_empty()
            || !cmd.token_file.is_empty()
            || !cmd.config_file.is_empty()
            || cmd.from_running
            || !cmd.binary.is_empty())
    {
        return Err("connection options are only accepted for service install".into());
    }
    if cmd.from_running
        && (!cmd.endpoint.is_empty() || !cmd.token_file.is_empty() || !cmd.config_file.is_empty())
    {
        return Err("--from-running cannot be combined with connection options".into());
    }
    Ok(cmd)
}

pub fn default_binary(home: &Path) -> PathBuf {
    home.join(".local/bin/hmux-web")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> Spec {
        Spec {
            binary: "/Users/test user/bin/hmux-web".into(),
            home: "/Users/test user".into(),
            endpoint: "wss://hmux.example/connect".into(),
            token_file: "/Users/test user/a&b<quoted>\"/token".into(),
            config_file: "/Users/test user/client.toml".into(),
            log_file: "/Users/test user/service.log".into(),
            environment: BTreeMap::from([
                ("HOME".into(), "/Users/test user".into()),
                ("PATH".into(), "/bin:/usr/bin".into()),
            ]),
        }
    }
    #[test]
    fn renders_native_process_lifetime_and_escaping() {
        let s = spec();
        let plist = String::from_utf8(launch_agent(&s)).unwrap();
        assert!(plist.contains("a&amp;b&lt;quoted&gt;&#34;"));
        assert!(plist.contains("<key>AbandonProcessGroup</key><true/>"));
        let unit = String::from_utf8(systemd_unit(&s)).unwrap();
        for fragment in [
            "ExecStart=\"/usr/bin/env\" \"-i\"",
            "Restart=always",
            "KillMode=process",
            "UMask=0077",
            "WantedBy=default.target",
        ] {
            assert!(unit.contains(fragment), "{fragment}");
        }
        assert_eq!(
            unit_quote("/a b/%n/$HOME/\"quote\"/back\\slash", true),
            "\"/a b/%%n/$$HOME/\\\"quote\\\"/back\\\\slash\""
        );
        assert_eq!(unit_quote("/a/%h/$HOME", false), "\"/a/%%h/$HOME\"");
    }
    #[test]
    fn environment_rejects_relative_paths_and_secrets() {
        let mut source = BTreeMap::from([
            ("PATH".into(), "/bin:/usr/bin".into()),
            ("CODEX_HOME".into(), "/private/codex".into()),
            ("OPENAI_API_KEY".into(), "secret".into()),
        ]);
        let env = service_environment_from(Path::new("/Users/test"), "test", &source).unwrap();
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert_eq!(env["CODEX_HOME"], "/private/codex");
        source.insert("PATH".into(), "relative:/bin".into());
        assert!(service_environment_from(Path::new("/Users/test"), "test", &source).is_err());
        source.insert("PATH".into(), "/bin".into());
        source.insert("CODEX_HOME".into(), "relative".into());
        assert!(service_environment_from(Path::new("/Users/test"), "test", &source).is_err());
    }
    #[test]
    fn parser_accepts_only_install_options() {
        let args = [
            "install",
            "--url",
            "wss://example/connect",
            "--token-file",
            "/private/token",
        ]
        .map(str::to_owned);
        assert_eq!(parse_command(&args).unwrap().action, Action::Install);
        assert!(parse_command(&["start".into(), "--url".into(), "x".into()]).is_err());
        assert!(parse_command(&[
            "install".into(),
            "--from-running".into(),
            "--url".into(),
            "x".into()
        ])
        .is_err());
        assert_eq!(
            parse_command(&[
                "install".into(),
                "-url=x".into(),
                "--url".into(),
                "y".into(),
                "--from-running=false".into()
            ])
            .unwrap()
            .endpoint,
            "y"
        );
    }
}

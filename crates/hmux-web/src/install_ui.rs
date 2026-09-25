//! Small installer presentation and opt-in connection guide; no resident UI or dependencies.
use crate::{enroll::line_prompt, locale};
use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub struct Display {
    color: bool,
}

impl Display {
    pub fn new() -> Self {
        Self {
            color: io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").is_ok_and(|s| s != "dumb"),
        }
    }

    fn title(&self, text: &str) {
        if self.color {
            println!("\x1b[1;34m{text}\x1b[0m");
        } else {
            println!("{text}");
        }
    }

    pub fn welcome(&self, binaries_only: bool) {
        println!();
        self.title("HMux / Home");
        println!("{}", locale::tr("Low memory, web terminal for AI agents."));
        if binaries_only {
            println!(
                "{}",
                locale::tr(
                    "Update native executables. Keep configuration and services as they are."
                )
            );
        } else {
            println!(
                "{}",
                locale::tr("Your agents run here. Your browser connects from anywhere.")
            );
        }
    }

    pub fn step(&self, number: usize, text: &'static str) {
        println!();
        self.title(&format!("{number:02}  {}", locale::tr(text)));
    }

    pub fn dependencies(&self) {
        let path = std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .unwrap_or_default();
        println!();
        for (label, name) in [("tmux", "tmux"), ("Codex", "codex"), ("Claude", "claude")] {
            let status = if !path.is_empty() && hmux_service::executable_in_path(name, &path) {
                locale::tr("found")
            } else if name == "tmux" {
                locale::tr("missing; install before connecting")
            } else {
                locale::tr("optional; not found")
            };
            println!("  {label:<8} {status}");
        }
        println!(
            "  {}",
            locale::tr("Provider login stays with your existing CLI account.")
        );
    }

    pub fn complete(&self, bin: &Path, config: &Path, service: bool, binaries_only: bool) {
        println!();
        self.title(locale::tr("Home installed"));
        println!(
            "  {:<16} {}",
            locale::tr("Binaries"),
            printable(&bin.to_string_lossy())
        );
        if binaries_only {
            println!(
                "  {:<16} {}",
                locale::tr("Configuration"),
                locale::tr("unchanged")
            );
            println!(
                "  {:<16} {}",
                locale::tr("Running Home"),
                locale::tr("unchanged; restart separately to use this version")
            );
            return;
        }
        println!(
            "  {:<16} {}",
            locale::tr("Configuration"),
            printable(&config.to_string_lossy())
        );
        println!(
            "  {:<16} {}",
            locale::tr("Automatic start"),
            locale::tr(if service { "requested" } else { "not changed" })
        );
        let web = shell_quote(&bin.join("hmux-web").to_string_lossy());
        println!();
        if service {
            println!("{}", locale::tr("Check your Home process:"));
            println!("  {web} service status");
            println!(
                "{}",
                locale::tr("Then open your Gateway's HTTPS address in a browser.")
            );
            println!(
                "{}",
                locale::tr("Service registration alone does not confirm a Gateway connection.")
            );
        } else {
            println!(
                "{}",
                locale::tr("Connect when your Gateway and private token file are ready:")
            );
            // Preserve an explicitly chosen install/config location in the copyable next step.
            let cfg = if config.join("home.toml").is_file() {
                config.join("home.toml")
            } else {
                config.join("client.toml")
            };
            println!(
                "  {web} service install --binary {web} --config {} \\",
                shell_quote(&cfg.to_string_lossy())
            );
            println!("    --url 'wss://YOUR_HOST/connect' --token-file '/PRIVATE/connector.token'");
        }
    }
}

pub struct Choice {
    pub enable_service: bool,
    pub connection: Option<(String, PathBuf)>,
}

pub fn connection(
    stop: &CancellationToken,
    requested: bool,
    explicit: bool,
    imported: bool,
    home: &Path,
    config: &Path,
) -> io::Result<Choice> {
    if requested {
        println!(
            "{}",
            locale::tr("  Automatic startup requested by --enable-service.")
        );
        if !explicit {
            println!(
                "{}",
                locale::tr(
                    "  Adopting the sole running Home connector and its connection settings."
                )
            );
        }
        return Ok(Choice {
            enable_service: true,
            connection: None,
        });
    }
    println!(
        "{}",
        locale::tr(
            "A Gateway with HTTPS and a private connector token is needed for remote access."
        )
    );
    println!(
        "{}",
        locale::tr("You can finish the local installation now and connect later.")
    );
    let enabled = loop {
        let answer = line_prompt(stop, locale::tr("Set up automatic startup now? [y/N]: "))?;
        match answer.to_ascii_lowercase().as_str() {
            "" | "n" | "no" => break false,
            "y" | "yes" => break true,
            _ => println!("{}", locale::tr("Enter y or n.")),
        }
    };
    if !enabled {
        return Ok(Choice {
            enable_service: false,
            connection: None,
        });
    }
    if imported {
        println!(
            "{}",
            locale::tr("  Using the address and private token from your Gateway connection file.")
        );
        return Ok(Choice {
            enable_service: true,
            connection: None,
        });
    }
    let endpoint = loop {
        let raw = line_prompt(stop, locale::tr("Gateway address (https://...): "))?;
        if let Some(endpoint) = endpoint(&raw) {
            break endpoint;
        }
        println!(
            "{}",
            locale::tr(
                "Use an HTTPS site address or wss://host/connect, without a query or fragment."
            )
        );
    };
    let default_token = config.join("web/connector.token");
    let prompt = format!(
        "{} [{}]: ",
        locale::tr("Connector token file"),
        printable(&default_token.to_string_lossy())
    );
    println!(
        "{}",
        locale::tr(
            "Use the private token file copied from your Gateway. Do not paste the token here."
        )
    );
    let token = loop {
        let raw = line_prompt(stop, &prompt)?;
        let path = if raw.is_empty() {
            default_token.clone()
        } else if let Some(tail) = raw.strip_prefix("~/") {
            home.join(tail)
        } else {
            PathBuf::from(raw)
        };
        if path.is_absolute() && hmux_core::token::load(&path).is_ok() {
            break path;
        }
        println!("{}", locale::tr("Token file unavailable. Use an absolute path or ~/ and a private, valid token file."));
    };
    Ok(Choice {
        enable_service: true,
        connection: Some((endpoint, token)),
    })
}

fn endpoint(raw: &str) -> Option<String> {
    let raw = raw.trim_end_matches('/');
    let value = if let Some(site) = raw.strip_prefix("https://") {
        format!(
            "wss://{}/connect",
            site.strip_suffix("/connect").unwrap_or(site)
        )
    } else {
        raw.into()
    };
    hmux_home::dial::Endpoint::parse(&value).ok().map(|_| value)
}

fn printable(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", printable(value).replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn website_input_uses_the_existing_strict_endpoint_contract() {
        for raw in [
            "https://hmux.example",
            "https://hmux.example/",
            "https://hmux.example/connect",
            "wss://hmux.example/connect",
        ] {
            assert_eq!(endpoint(raw).as_deref(), Some("wss://hmux.example/connect"));
        }
        for raw in [
            "http://hmux.example",
            "https://user:pass@hmux.example",
            "https://hmux.example/path",
            "https://hmux.example?token=secret",
            "https://hmux.example#fragment",
        ] {
            assert!(endpoint(raw).is_none());
        }
    }
    #[test]
    fn copyable_commands_quote_paths_and_escape_terminal_controls() {
        assert_eq!(
            shell_quote("/tmp/a'b $(literal)"),
            "'/tmp/a'\\''b $(literal)'"
        );
        assert_eq!(printable("a\x1b[2J\nb"), "a\\u{1b}[2J\\nb");
    }
}

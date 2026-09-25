//! Interactive enrollment; no secrets in argv, logs, or child processes.
use crate::args::{absolute, invalid, Options};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmux_core::PrivateDir;
use hmux_gateway::auth::{derive_password, Credentials};
use rustix::{
    event::{poll, PollFd, PollFlags, Timespec},
    termios::{self, LocalModes, OptionalActions, Termios},
};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::fd::AsFd,
    path::Path,
};
use tokio_util::sync::CancellationToken;

pub fn initialize(opts: Options, stop: CancellationToken) -> io::Result<()> {
    if opts.credentials.is_empty() || opts.token.is_empty() {
        return Err(invalid("--credentials and --token-file required"));
    }
    let credentials_path = absolute(&opts.credentials)?;
    let token_path = absolute(&opts.token)?;
    if credentials_path == token_path {
        return Err(invalid("secret file paths must differ"));
    }
    for path in [&credentials_path, &token_path] {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(invalid("refusing to overwrite existing secret files")),
        }
    }
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(invalid(
            "init requires an interactive terminal; secrets are not accepted in argv",
        ));
    }
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    write!(output, "Username: ")?;
    output.flush()?;
    let name = read_line(&mut input, &stop, 256)?;
    let name = name.trim();
    let mut password = secret(
        &mut input,
        &mut output,
        &stop,
        "Password (at least 8 bytes): ",
    )?;
    let mut confirm = secret(&mut input, &mut output, &stop, "Confirm password: ")?;
    if password != confirm {
        return Err(invalid("passwords do not match"));
    }
    let generated = credentials(name, &password);
    // Minimize the plaintext lifetime; no allocation is retained by an owner.
    password.clear();
    confirm.clear();
    let mut credentials = generated?;
    writeln!(
        output,
        "Add this secret to Google Authenticator (time-based). Keep it private:"
    )?;
    writeln!(output, "{}", credentials.totp_secret)?;
    writeln!(output, "Enrollment URI: {}", enrollment_uri(&credentials))?;
    write!(output, "Current 6-digit code: ")?;
    output.flush()?;
    let code = read_line(&mut input, &stop, 32)?;
    credentials.last_step = credentials
        .match_code(code.trim(), chrono::Utc::now().timestamp())
        .ok_or_else(|| invalid("authenticator code did not match"))?;
    if stop.is_cancelled() {
        return Err(interrupted());
    }
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).map_err(|_| io::Error::other("secure randomness unavailable"))?;
    let token = format!("{}\n", URL_SAFE_NO_PAD.encode(token));
    write_new(&credentials_path, credentials.go_json().as_bytes())?;
    write_new(&token_path, token.as_bytes())?;
    writeln!(
        output,
        "Created private credentials and connector token. Never commit or share these files."
    )?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = PrivateDir::open_or_create_trusted(
        path.parent()
            .ok_or_else(|| invalid("invalid secret path"))?,
    )?;
    dir.write_new_private(
        path.file_name()
            .ok_or_else(|| invalid("invalid secret path"))?,
        bytes,
    )
    .map_err(io::Error::other)
}
fn credentials(username: &str, password: &str) -> io::Result<Credentials> {
    if username.is_empty()
        || username.len() > 80
        || username.trim() != username
        || !(8..=128).contains(&password.len())
    {
        return Err(invalid("username required; password must be 8–128 bytes"));
    }
    let mut salt = [0_u8; 32];
    let mut secret = [0_u8; 20];
    getrandom::fill(&mut salt)
        .and_then(|()| getrandom::fill(&mut secret))
        .map_err(|_| io::Error::other("secure randomness unavailable"))?;
    Ok(Credentials {
        username: username.into(),
        hash: derive_password(password, &salt).to_vec(),
        salt: salt.to_vec(),
        totp_secret: data_encoding::BASE32_NOPAD.encode(&secret),
        ..Default::default()
    })
}
fn enrollment_uri(c: &Credentials) -> String {
    // Go url.PathEscape leaves these segment-safe characters unescaped.
    let mut label = String::new();
    for byte in format!("HMux:{}", c.username).bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~:@&=+$".contains(&byte) {
            label.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(&mut label, "%{byte:02X}").expect("write to string");
        }
    }
    format!(
        "otpauth://totp/{label}?algorithm=SHA1&digits=6&issuer=HMux&period=30&secret={}",
        c.totp_secret
    )
}
fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "enrollment cancelled")
}
fn read_line(input: &mut impl AsFd, stop: &CancellationToken, max: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        if stop.is_cancelled() {
            return Err(interrupted());
        }
        let mut pollfds = [PollFd::new(&*input, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        match poll(&mut pollfds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let mut byte = [0_u8];
        match rustix::io::read(&*input, &mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal input ended",
                ))
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => bytes.push(byte[0]),
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
        if bytes.len() > max {
            return Err(invalid("terminal input exceeds limit"));
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes).map_err(|_| invalid("terminal input must be UTF-8"))
}
struct EchoGuard<'a, T: AsFd> {
    input: &'a mut T,
    original: Termios,
}
impl<T: AsFd> Drop for EchoGuard<'_, T> {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(&*self.input, OptionalActions::Now, &self.original);
    }
}
fn secret(
    input: &mut impl AsFd,
    output: &mut impl Write,
    stop: &CancellationToken,
    prompt: &str,
) -> io::Result<String> {
    let original = termios::tcgetattr(&*input)?;
    let mut hidden = original.clone();
    hidden
        .local_modes
        .remove(LocalModes::ECHO | LocalModes::ECHONL);
    termios::tcsetattr(&*input, OptionalActions::Now, &hidden)?;
    let guard = EchoGuard { input, original };
    write!(output, "{prompt}")?;
    output.flush()?;
    let result = read_line(guard.input, stop, 128);
    drop(guard);
    writeln!(output)?;
    result
}

/// Optional new-install workspace prompt; existing installations never prompt.
pub fn workspace_prompt(stop: &CancellationToken) -> io::Result<String> {
    let line = line_prompt(
        stop,
        crate::locale::tr("New-session base directory [~/.hmux]: "),
    )?;
    Ok(if line.is_empty() {
        "~/.hmux".into()
    } else {
        line
    })
}

/// Bounded, cancellable input for non-secret installer choices.
pub fn line_prompt(stop: &CancellationToken, prompt: &str) -> io::Result<String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    print!("{prompt}");
    io::stdout().flush()?;
    let line = read_line(&mut input, stop, 4096)?;
    if line.chars().any(char::is_control) {
        return Err(invalid("input must not contain control characters"));
    }
    Ok(line.trim().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_credentials_match_go_codec_and_limits() {
        let c = credentials("synthetic", "example-password").unwrap();
        assert!(c.matches_password("example-password"));
        assert!(hmux_gateway::auth::parse_credentials(c.go_json().as_bytes()).is_ok());
        assert!(credentials(" name", "example-password").is_err());
        assert!(credentials("name", "short").is_err());
        assert!(credentials("name", &"x".repeat(129)).is_err());
    }
    #[test]
    fn uri_escapes_path_segment() {
        let c = Credentials {
            username: "a/b #한".into(),
            totp_secret: "SYNTHETIC".into(),
            ..Default::default()
        };
        assert_eq!(enrollment_uri(&c), "otpauth://totp/HMux:a%2Fb%20%23%ED%95%9C?algorithm=SHA1&digits=6&issuer=HMux&period=30&secret=SYNTHETIC");
    }
}

use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
};

pub const USAGE: &str =
    "usage: hmux-web <install|init|init-web|serve|connect|service|install-home|install-gateway>";

#[derive(Default)]
pub struct Options {
    pub credentials: OsString,
    pub token: OsString,
    pub origin: String,
    pub listen: String,
    pub assets: OsString,
    pub endpoint: String,
    pub config: OsString,
    pub log: OsString,
}

pub fn parse(args: &[OsString]) -> io::Result<Options> {
    let mut opts = Options {
        listen: "127.0.0.1:8088".into(),
        assets: "web/dist".into(),
        ..Default::default()
    };
    if args.len() > 256 {
        return Err(invalid("too many arguments"));
    }
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let text = arg.to_str().ok_or_else(|| invalid("invalid option"))?;
        if text == "--" {
            if args.next().is_some() {
                return Err(invalid("unexpected arguments"));
            }
            break;
        }
        if !text.starts_with('-') {
            return Err(invalid("unexpected arguments"));
        }
        let flag = text.strip_prefix("--").unwrap_or(&text[1..]);
        let (flag, inline) = flag
            .split_once('=')
            .map_or((flag, None), |(k, v)| (k, Some(v)));
        if !matches!(
            flag,
            "credentials"
                | "token-file"
                | "origin"
                | "listen"
                | "assets"
                | "url"
                | "config"
                | "log-file"
        ) {
            return Err(invalid("unknown option"));
        }
        let value = match inline {
            Some(v) => OsString::from(v),
            None => args
                .next()
                .ok_or_else(|| invalid("missing option value"))?
                .clone(),
        };
        if value.len() > 4096 || value.as_encoded_bytes().contains(&0) {
            return Err(invalid("invalid option value"));
        }
        match flag {
            "credentials" => opts.credentials = value,
            "token-file" => opts.token = value,
            "assets" => opts.assets = value,
            "config" => opts.config = value,
            "log-file" => opts.log = value,
            "origin" => opts.origin = text_value(value)?,
            "listen" => opts.listen = text_value(value)?,
            "url" => opts.endpoint = text_value(value)?,
            _ => unreachable!(),
        }
    }
    Ok(opts)
}
fn text_value(value: OsString) -> io::Result<String> {
    value
        .into_string()
        .map_err(|_| invalid("invalid text option"))
}
pub fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Lexical normalization retains private-file symlink checks at the file owner.
pub fn absolute(path: &OsStr) -> io::Result<PathBuf> {
    if path.is_empty() {
        return Err(invalid("required path is empty"));
    }
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            _ => clean.push(part),
        }
    }
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }
    #[test]
    fn go_flags_defaults_equals_single_dash_and_last_value() {
        let opts = parse(&args(&[
            "-url=wss://hmux.example/connect",
            "--config",
            "home.toml",
            "--listen",
            "localhost:3",
            "--listen=127.0.0.1:4",
        ]))
        .unwrap();
        assert_eq!(opts.endpoint, "wss://hmux.example/connect");
        assert_eq!(opts.config, "home.toml");
        assert_eq!(opts.listen, "127.0.0.1:4");
        assert_eq!(opts.assets, "web/dist");
    }
    #[test]
    fn positional_unknown_missing_and_oversized_rejected() {
        for a in [
            args(&["unexpected"]),
            args(&["--token-file"]),
            args(&["--password=secret"]),
            args(&["--", "unexpected"]),
            vec!["--url".into(), "x".repeat(4097).into()],
        ] {
            assert!(parse(&a).is_err());
        }
    }
}

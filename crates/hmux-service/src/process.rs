//! Exact, bounded inspection of same-account Home connector processes.
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: i32,
    pub birth: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

const ALLOWED_ENV: &[&str] = &[
    "HOME",
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "process exited")
}

fn process_environment(raw: &[u8]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for entry in raw.split(|byte| *byte == 0) {
        let Some(at) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let Ok(key) = std::str::from_utf8(&entry[..at]) else {
            continue;
        };
        if !ALLOWED_ENV.contains(&key) || env.contains_key(key) {
            continue;
        }
        if let Ok(value) = std::str::from_utf8(&entry[at + 1..]) {
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    env
}

/// Extract only the recognized connect options from an exact `hmux-web connect` argv.
pub fn connector_options(process: &Process) -> Result<(String, String, String), String> {
    if process.args.len() < 2
        || Path::new(&process.args[0])
            .file_name()
            .is_none_or(|name| name != "hmux-web")
        || process.args[1] != "connect"
    {
        return Err("process is not a Home connector".into());
    }
    let mut url = None;
    let mut token = None;
    let mut config = None;
    let mut log = None;
    let mut args = process.args[2..].iter();
    while let Some(arg) = args.next() {
        let (key, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        let slot = match key {
            "--url" | "-url" => &mut url,
            "--token-file" | "-token-file" => &mut token,
            "--config" | "-config" => &mut config,
            "--log-file" | "-log-file" => &mut log,
            _ => return Err("cannot adopt connector with unrecognized arguments".into()),
        };
        let value = inline
            .or_else(|| args.next().map(String::as_str))
            .ok_or("cannot adopt connector with missing arguments")?;
        if value.starts_with('-') || value.contains('\0') {
            return Err("cannot adopt connector with invalid arguments".into());
        }
        *slot = Some(value.to_owned());
    }
    let token = token.unwrap_or_default();
    let config = config.unwrap_or_default();
    if !Path::new(&token).is_absolute() || (!config.is_empty() && !Path::new(&config).is_absolute())
    {
        return Err("adoption requires absolute token/config paths; stop the old connector and install with explicit options".into());
    }
    Ok((url.unwrap_or_default(), token, config))
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{gone, invalid, process_environment, Process};
    use std::fs::{self, File};
    use std::io::{self, Read};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    fn bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let mut raw = Vec::new();
        File::open(path)?.take(limit + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 > limit {
            return Err(invalid("process metadata exceeds limit"));
        }
        Ok(raw)
    }
    fn birth(stat: &[u8]) -> io::Result<String> {
        let at = stat
            .iter()
            .rposition(|byte| *byte == b')')
            .ok_or_else(|| invalid("invalid process stat"))?;
        let tail =
            std::str::from_utf8(&stat[at + 1..]).map_err(|_| invalid("invalid process stat"))?;
        let fields: Vec<_> = tail.split_whitespace().take(20).collect();
        if fields.len() < 20 {
            return Err(invalid("invalid process stat"));
        }
        if fields[0] == "Z" || fields[0] == "X" {
            return Err(gone());
        }
        Ok(fields[19].to_owned())
    }
    fn owned(base: &Path) -> io::Result<()> {
        if fs::metadata(base)?.uid() != rustix::process::getuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connector belongs to a different user",
            ));
        }
        Ok(())
    }
    pub fn read_process(pid: i32) -> io::Result<Process> {
        if pid <= 1 {
            return Err(gone());
        }
        let base = Path::new("/proc").join(pid.to_string());
        owned(&base)?;
        let first = birth(&bounded(&base.join("stat"), 65_536)?)?;
        let raw = bounded(&base.join("cmdline"), 65_536)?;
        if raw.is_empty() || raw.last() != Some(&0) {
            return Err(invalid("invalid process arguments"));
        }
        let args = raw[..raw.len() - 1]
            .split(|byte| *byte == 0)
            .map(|arg| {
                String::from_utf8(arg.to_vec()).map_err(|_| invalid("invalid process arguments"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        if args.is_empty() || args.len() > 128 || args[0].is_empty() {
            return Err(invalid("invalid process arguments"));
        }
        let env = process_environment(&bounded(&base.join("environ"), 2 << 20)?);
        owned(&base)?;
        if birth(&bounded(&base.join("stat"), 65_536)?)? != first {
            return Err(gone());
        }
        Ok(Process {
            pid,
            birth: first,
            args,
            environment: env,
        })
    }
    pub fn connectors() -> io::Result<Vec<Process>> {
        let mut found = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if pid <= 1 || pid == std::process::id() as i32 {
                continue;
            }
            let base = entry.path();
            let Ok(meta) = fs::metadata(&base) else {
                continue;
            };
            if meta.uid() != rustix::process::getuid().as_raw() {
                continue;
            }
            let comm = match bounded(&base.join("comm"), 64) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if comm != b"hmux-web\n" {
                continue;
            }
            match read_process(pid) {
                Ok(process) if process.args.get(1).is_some_and(|arg| arg == "connect") => {
                    found.push(process)
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            if found.len() > 128 {
                return Err(invalid("too many Home connectors"));
            }
        }
        Ok(found)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{gone, invalid, process_environment, Process};
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid::pidinfo;
    use libproc::processes::{pids_by_type, ProcFilter};
    use std::io;

    fn info(pid: i32) -> io::Result<BSDInfo> {
        let data = pidinfo::<BSDInfo>(pid, 0).map_err(|error| {
            if let Some(pid) = rustix::process::Pid::from_raw(pid) {
                if rustix::process::test_kill_process(pid)
                    .is_err_and(|check| check == rustix::io::Errno::SRCH)
                {
                    return gone();
                }
            }
            io::Error::other(error)
        })?;
        if data.pbi_pid == 0 || data.pbi_status == 5 {
            return Err(gone());
        }
        let uid = rustix::process::getuid().as_raw();
        if data.pbi_ruid != uid || data.pbi_uid != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connector belongs to a different user",
            ));
        }
        Ok(data)
    }
    fn identity(data: &BSDInfo) -> String {
        format!("{}:{}", data.pbi_start_tvsec, data.pbi_start_tvusec)
    }
    fn comm(data: &BSDInfo) -> Vec<u8> {
        data.pbi_comm
            .iter()
            .map(|byte| *byte as u8)
            .take_while(|byte| *byte != 0)
            .collect()
    }
    fn parse_data(
        raw: &[u8],
    ) -> io::Result<(Vec<String>, std::collections::BTreeMap<String, String>)> {
        if raw.len() < 4 {
            return Err(invalid("invalid process arguments"));
        }
        let count = i32::from_ne_bytes(
            raw[..4]
                .try_into()
                .map_err(|_| invalid("invalid process arguments"))?,
        );
        if !(1..=128).contains(&count) {
            return Err(invalid("invalid process argument count"));
        }
        let mut at = 4;
        let end = raw[at..]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("missing executable path"))?
            + at;
        at = end;
        while raw.get(at) == Some(&0) {
            at += 1;
        }
        let mut args = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let end = raw[at..]
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| invalid("truncated process arguments"))?
                + at;
            if args.is_empty() && end == at {
                return Err(invalid("empty process argument"));
            }
            args.push(
                String::from_utf8(raw[at..end].to_vec())
                    .map_err(|_| invalid("invalid process argument encoding"))?,
            );
            at = end + 1;
        }
        Ok((args, process_environment(&raw[at..])))
    }
    pub fn read_process(pid: i32) -> io::Result<Process> {
        if pid <= 1 {
            return Err(gone());
        }
        let first = info(pid)?;
        let (args, environment) = parse_data(&hmux_platform::macos::process_arguments(pid)?)?;
        let second = info(pid)?;
        if identity(&first) != identity(&second) || comm(&first) != comm(&second) {
            return Err(gone());
        }
        Ok(Process {
            pid,
            birth: identity(&first),
            args,
            environment,
        })
    }
    pub fn connectors() -> io::Result<Vec<Process>> {
        let uid = rustix::process::getuid().as_raw();
        hmux_platform::macos::max_processes_per_user()?;
        // Validate the kernel allocation cap before asking libproc for the list.
        let pids = pids_by_type(ProcFilter::ByRealUID { ruid: uid })?;
        if pids.len() > 65_536 {
            return Err(invalid("too many processes to inspect"));
        }
        let mut found = Vec::new();
        for pid in pids {
            let Ok(pid) = i32::try_from(pid) else {
                continue;
            };
            if pid <= 1 || pid == std::process::id() as i32 {
                continue;
            }
            let first = match info(pid) {
                Ok(data) => data,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if comm(&first) != b"hmux-web" {
                continue;
            }
            match read_process(pid) {
                Ok(process) if process.args.get(1).is_some_and(|arg| arg == "connect") => {
                    found.push(process)
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            if found.len() > 128 {
                return Err(invalid("too many Home connectors"));
            }
        }
        Ok(found)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn parses_kernel_arguments_with_padding_and_filtered_environment() {
            let mut raw = 3_i32.to_ne_bytes().to_vec();
            raw.extend(b"/bin/hmux-web\0\0\0hmux-web\0connect\0--token-file\0HOME=/tmp/home\0OPENAI_API_KEY=secret\0PATH=/bin\0");
            let (args, env) = parse_data(&raw).unwrap();
            assert_eq!(args, ["hmux-web", "connect", "--token-file"]);
            assert_eq!(env.get("HOME").map(String::as_str), Some("/tmp/home"));
            assert!(!env.contains_key("OPENAI_API_KEY"));
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::Process;
    use std::io;
    pub fn read_process(_pid: i32) -> io::Result<Process> {
        Err(io::Error::other("Home services require macOS or Linux"))
    }
    pub fn connectors() -> io::Result<Vec<Process>> {
        Err(io::Error::other("Home services require macOS or Linux"))
    }
}

pub use platform::{connectors, read_process};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires native process metadata permission; inspects only this test and its child"]
    fn reads_only_self_process_metadata() {
        let pid = std::process::id() as i32;
        let process = read_process(pid).unwrap();
        assert_eq!(process.pid, pid);
        assert!(!process.birth.is_empty());
        assert!(!process.args.is_empty());
        assert!(process
            .environment
            .keys()
            .all(|key| ALLOWED_ENV.contains(&key.as_str())));
        let again = read_process(pid).unwrap();
        assert_eq!(process.birth, again.birth);
        assert_eq!(process.args, again.args);

        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let executable = std::env::current_exe().unwrap();
        let arguments = [
            "--exact",
            "process::tests::metadata_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
            "",
            "space and ;$ literals",
        ];
        let mut child = Command::new(&executable)
            .args(arguments)
            .env_clear()
            .env("HMUX_PROCESS_TEST_CHILD", "1")
            .env("HOME", "/hmux-e2e-synthetic")
            .env("PATH", "/usr/bin:/bin")
            .env("OPENAI_API_KEY", "hmux-e2e-private-sentinel")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let child_pid = child.id() as i32;
        let mut ready = String::new();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        for _ in 0..16 {
            ready.clear();
            output.read_line(&mut ready).unwrap();
            if ready.ends_with("hmux-e2e-ready\n") {
                break;
            }
        }
        let inspected = read_process(child_pid);
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready.ends_with("hmux-e2e-ready\n"));
        let inspected = inspected.unwrap();
        let expected = std::iter::once(executable.to_str().unwrap())
            .chain(arguments)
            .collect::<Vec<_>>();
        assert_eq!(inspected.args, expected);
        assert_eq!(
            inspected.environment.get("HOME").map(String::as_str),
            Some("/hmux-e2e-synthetic")
        );
        assert!(!inspected.environment.contains_key("OPENAI_API_KEY"));
    }
    // Executed only by the parent metadata test, never a production process.
    #[test]
    #[ignore = "fixture child for reads_only_self_process_metadata"]
    fn metadata_child() {
        if std::env::var("HMUX_PROCESS_TEST_CHILD").as_deref() != Ok("1") {
            return;
        }
        use std::io::Write;
        println!("hmux-e2e-ready");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
    }
    fn process(args: &[&str]) -> Process {
        Process {
            pid: 42,
            birth: "1:2".into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            environment: BTreeMap::new(),
        }
    }
    #[test]
    fn only_allowlisted_environment_leaves_reader() {
        let env = process_environment(
            b"HOME=/home/test\0OPENAI_API_KEY=secret\0PATH=/usr/bin\0PATH=/tmp\0",
        );
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
    }
    #[test]
    fn adoption_rejects_unknown_or_relative_arguments() {
        let base = [
            "/bin/hmux-web",
            "connect",
            "--url",
            "wss://example/connect",
            "--token-file",
            "/private/token",
            "--config",
            "/private/home.toml",
        ];
        let parsed = connector_options(&process(&base)).unwrap();
        assert_eq!(parsed.0, "wss://example/connect");
        assert_eq!(parsed.1, "/private/token");
        assert_eq!(parsed.2, "/private/home.toml");
        for args in [
            vec!["/bin/sh", "connect", "--token-file", "/private/token"],
            vec!["/bin/hmux-web", "serve", "--token-file", "/private/token"],
            vec!["/bin/hmux-web", "connect", "--token-file", "relative"],
            vec![
                "/bin/hmux-web",
                "connect",
                "--token-file",
                "/private/token",
                "--unsafe",
            ],
        ] {
            assert!(connector_options(&process(&args)).is_err());
        }
    }
}

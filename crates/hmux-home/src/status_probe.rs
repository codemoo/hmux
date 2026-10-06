//! User-authorized, bounded `/status` repair of missing Codex reader bindings.
//! No interrupt/clear keys, terminal attachment, directory guessing or private DB.
use crate::{
    binding::{Binding, Provider, Status},
    catalog::TmuxCatalogReader,
    conversation_link::{self, Link},
    input_gate,
    inspection::{self, Error, Inspector, ScanPurpose},
    observation::{self, Reason},
    records,
    view::Target,
};
use hmux_model::SessionIdentity;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
#[path = "status_probe_size.rs"]
mod size;
const FORMAT: &str = "#{session_id}|#{session_created}|#{pane_id}|#{pane_pid}|#{cursor_x}|#{cursor_y}|#{pane_width}|#{pane_height}|#{pane_in_mode}|#{pane_dead}|#{alternate_on}|#{pane_current_command}";
static PROBE: Mutex<()> = Mutex::new(());
static SENT: OnceLock<Mutex<HashMap<SessionIdentity, Instant>>> = OnceLock::new();
const COOLDOWN: Duration = Duration::from_secs(30);
#[derive(Clone, PartialEq, Eq)]
struct Pane {
    fields: Vec<String>,
    id: String,
    y: usize,
}
impl Pane {
    fn parse(raw: &[u8], identity: &SessionIdentity, pane_pid: i32) -> Result<Self, Error> {
        let text = std::str::from_utf8(raw)
            .map_err(|_| Error::Unavailable)?
            .trim();
        let f: Vec<_> = text.split('|').map(str::to_owned).collect();
        if f.len() != 12
            || f[0] != identity.id
            || f[1] != identity.created_at.to_string()
            || f[3] != pane_pid.to_string()
            || f[8] != "0"
            || f[9] != "0"
            || !["0", "1"].contains(&f[10].as_str())
        {
            return Err(Error::Unavailable);
        }
        if !f[2]
            .strip_prefix('%')
            .is_some_and(|s| !s.is_empty() && s.len() < 20 && s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(Error::Unavailable);
        }
        let x = f[4].parse::<usize>().map_err(|_| Error::Unavailable)?;
        let y = f[5].parse::<usize>().map_err(|_| Error::Unavailable)?;
        let width = f[6].parse::<usize>().map_err(|_| Error::Unavailable)?;
        let height = f[7].parse::<usize>().map_err(|_| Error::Unavailable)?;
        if !(48..=500).contains(&width)
            || !(12..=300).contains(&height)
            || x >= width
            || y >= height
            || f[11].is_empty()
            || !f[11]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
        {
            return Err(Error::Unavailable);
        }
        Ok(Self {
            id: f[2].clone(),
            fields: f,
            y,
        })
    }
    fn owner_guard(&self) -> String {
        [0, 1, 2, 3]
            .iter()
            .map(|&i| {
                let key = ["session_id", "session_created", "pane_id", "pane_pid"][i];
                format!("#{{==:#{{{key}}},{}}}", self.fields[i])
            })
            .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
            .expect("fields")
    }
    fn guard(&self) -> String {
        let names = [
            "session_id",
            "session_created",
            "pane_id",
            "pane_pid",
            "cursor_x",
            "cursor_y",
            "pane_width",
            "pane_height",
            "pane_in_mode",
            "pane_dead",
            "alternate_on",
            "pane_current_command",
        ];
        names
            .iter()
            .zip(&self.fields)
            .map(|(k, v)| format!("#{{==:#{{{k}}},{v}}}"))
            .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
            .expect("fields")
    }
}
fn empty_prompt(screen: &str, pane: &Pane) -> bool {
    if pane.fields[4] != "2" {
        return false;
    }
    let lines: Vec<_> = screen.lines().collect();
    let prompt = lines.get(pane.y).map(|s| s.trim_end());
    if !matches!(
        prompt,
        Some("» Ask Codex to do anything" | "› Ask Codex to do anything" | "»" | "›")
    ) {
        return false;
    }
    if lines.iter().any(|s| {
        s.trim_start().starts_with("• Working (")
            || s.contains("Queued follow-up inputs")
            || s.contains("esc to interrupt")
            || s.contains("Would you like to")
            || s.contains("Approval required")
    }) {
        return false;
    }
    lines.get(pane.y + 1).is_some_and(|s| s.trim().is_empty())
}
fn owned_prompt(screen: &str, pane: &Pane) -> bool {
    let lines: Vec<_> = screen.lines().collect();
    pane.fields[4] == "9"
        && lines.get(pane.y + 1).is_some_and(|s| s.trim().is_empty())
        && screen
            .lines()
            .nth(pane.y)
            .is_some_and(|s| matches!(s.trim_end(), "» /status" | "› /status"))
}
fn same_input_owner(before: &Pane, typed: &Pane) -> bool {
    [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11]
        .iter()
        .all(|&i| before.fields[i] == typed.fields[i])
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn session_id(screen: &str) -> Result<Option<String>, Error> {
    let mut result = None;
    let lines: Vec<_> = screen.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let stripped = line.trim().trim_matches(['│', '|', ' ']);
        let Some(raw) = stripped
            .strip_prefix("Session:")
            .or_else(|| stripped.strip_prefix("Session ID:"))
        else {
            continue;
        };
        let mut id = raw.trim().trim_matches(['│', '|', ' ']).to_owned();
        if id.len() < 36 {
            if let Some(next) = lines.get(i + 1) {
                id.push_str(next.trim().trim_matches(['│', '|', ' ']));
            }
        }
        if !uuid(&id) {
            return Err(Error::Unavailable);
        }
        if result.as_ref().is_some_and(|old| old != &id) {
            return Err(Error::Unavailable);
        }
        result = Some(id);
    }
    Ok(result)
}
fn status_panel(screen: &str) -> bool {
    let label = |name: &str| {
        screen
            .lines()
            .any(|line| line.trim().trim_matches(['│', '|', ' ']).starts_with(name))
    };
    label("Model:") && label("Directory:")
}
#[derive(Clone, Copy)]
pub(crate) struct Probe<'a> {
    pub reporter: Option<&'a observation::Reporter>,
    pub identity: &'a SessionIdentity,
    pub pane: i32,
    pub base: &'a Binding,
    pub reader: &'a TmuxCatalogReader,
    pub inspector: &'a Inspector,
    pub state_dir: &'a std::path::Path,
    pub stop: &'a CancellationToken,
    pub deadline: Instant,
    pub runtime: &'a tokio::runtime::Handle,
}
impl Probe<'_> {
    fn command(&self, target: &Target, args: Vec<String>) -> Result<Vec<u8>, Error> {
        inspection::check(self.stop, self.deadline)?;
        let raw = self
            .runtime
            .block_on(target.command_bounded(
                inspection::commands(),
                args,
                65536,
                self.stop,
                self.deadline.into(),
            ))
            .map_err(|_| Error::Unavailable)?;
        if raw.len() > 65536 {
            return Err(Error::Unavailable);
        }
        Ok(raw)
    }
    fn metadata(&self, target: &Target, selector: &str) -> Result<Pane, Error> {
        Pane::parse(
            &self.command(
                target,
                vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    selector.into(),
                    FORMAT.into(),
                ],
            )?,
            self.identity,
            self.pane,
        )
    }
    fn screen(&self, target: &Target, pane: &Pane) -> Result<String, Error> {
        String::from_utf8(self.command(
            target,
            vec![
                "capture-pane".into(),
                "-p".into(),
                "-t".into(),
                pane.id.clone(),
            ],
        )?)
        .map_err(|_| Error::Unavailable)
    }
    fn current(&self, stamp: &str) -> Result<(), Error> {
        let current = self
            .runtime
            .block_on(self.reader.read_basic_cancelable(
                inspection::commands(),
                self.stop,
                self.deadline.into(),
            ))
            .map_err(|_| Error::Unavailable)?;
        if !current
            .sessions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|s| {
                s.id == self.identity.id
                    && s.created_at == self.identity.created_at
                    && s.pane_pid == i64::from(self.pane)
            })
        {
            return Err(Error::Unavailable);
        }
        let scan = self.inspector.scan(
            &[self.pane],
            ScanPurpose::Conversation,
            self.stop,
            self.deadline,
            self.runtime,
        )?;
        if scan.statuses.get(&self.pane) != Some(&Status::Unavailable)
            || !scan.bindings.get(&self.pane).is_some_and(|b| {
                b.provider == Provider::Codex && b.provider_pid == self.base.provider_pid
            })
            || self.inspector.process_stamp(
                self.base.provider_pid,
                self.stop,
                self.deadline,
                self.runtime,
            )? != stamp
            || !self.inspector.foreground(
                self.base.provider_pid,
                self.stop,
                self.deadline,
                self.runtime,
            )?
        {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub fn run(&self) -> Result<(), Error> {
        let started = Instant::now();
        let mut reason = Reason::ProbeProcess;
        let result = self.run_inner(&mut reason);
        if let Some(report) = self.reporter {
            report(observation::Event::new(
                observation::Stage::StatusProbe,
                Some(hmux_protocol::protobuf::types::Operation::Conversation),
                if result.is_ok() {
                    Reason::Recovered
                } else {
                    reason
                },
                started,
            ));
        }
        result
    }
    fn run_inner(&self, reason: &mut Reason) -> Result<(), Error> {
        if self.base.provider != Provider::Codex || self.base.provider_pid < 1 {
            return Err(Error::Unavailable);
        }
        *reason = Reason::Busy;
        let _one = PROBE.try_lock().map_err(|_| Error::Busy)?;
        *reason = Reason::ProbeCooldown;
        if SENT.get().is_some_and(|sent| {
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(self.identity)
                .is_some_and(|at| at.elapsed() < COOLDOWN)
        }) {
            return Err(Error::Busy);
        }
        *reason = Reason::ProbeInput;
        let input = input_gate::state(self.identity);
        if !input.quiet() {
            return Err(Error::Unavailable);
        }
        let epoch = input.epoch.load(Ordering::SeqCst);
        *reason = Reason::ProbeSave;
        let previous = conversation_link::snapshot(self.state_dir, self.identity)?;
        let target = self
            .reader
            .terminal_target()
            .map_err(|_| Error::Unavailable)?;
        *reason = Reason::ProbePane;
        let pane = self.metadata(&target, &self.identity.id)?;
        let before = self.screen(&target, &pane)?;
        *reason = Reason::ProbePrompt;
        if !empty_prompt(&before, &pane) {
            return Err(Error::Unavailable);
        }
        *reason = Reason::ProbeStale;
        if session_id(&before)?.is_some() {
            return Err(Error::Unavailable);
        }
        *reason = Reason::ProbeProcess;
        let stamp = self.inspector.process_stamp(
            self.base.provider_pid,
            self.stop,
            self.deadline,
            self.runtime,
        )?;
        self.current(&stamp)?;
        // Expensive process discovery stays outside the writer gate. Only the
        // final fresh-screen checks and fixed guarded send pause HMux input.
        *reason = Reason::ProbeInput;
        let critical = Self {
            deadline: self
                .deadline
                .min(Instant::now() + Duration::from_millis(350)),
            ..*self
        };
        // Declare cleanup first so every early return releases input before
        // the cancellation-independent size restoration runs.
        let width;
        let gate=critical.runtime.block_on(async {tokio::select! {biased;_ = critical.stop.cancelled()=>Err(Error::Cancelled),_ = tokio::time::sleep_until(critical.deadline.into())=>Err(Error::Cancelled),guard = input.gate.lock()=>Ok(guard)}})?;
        *reason = Reason::ProbeChanged;
        if critical.metadata(&target, &self.identity.id)? != pane
            || critical.screen(&target, &pane)? != before
            || !input.quiet()
            || input.epoch.load(Ordering::SeqCst) != epoch
        {
            return Err(Error::Unavailable);
        }
        // Narrow status boxes clip the UUID rather than wrapping it. Widen only
        // this exact single-pane window and restore its size/policy on every exit.
        *reason = Reason::ProbeWidth;
        width = if pane.fields[6]
            .parse::<usize>()
            .map_err(|_| Error::Unavailable)?
            < 80
        {
            Some(size::Width::widen(critical, &target, &pane)?)
        } else {
            None
        };
        let (pane, before) = if width.is_some() {
            let until = critical.deadline;
            loop {
                let current = critical.metadata(&target, &self.identity.id)?;
                let screen = critical.screen(&target, &current)?;
                if current.id == pane.id && empty_prompt(&screen, &current) {
                    // Resize reflow can reveal an older status panel. Establish
                    // a new baseline and reject it before sending anything.
                    *reason = Reason::ProbeStale;
                    if session_id(&screen)?.is_some() {
                        return Err(Error::Unavailable);
                    }
                    break (current, screen);
                }
                inspection::check(self.stop, until)?;
                self.runtime
                    .block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
            }
        } else {
            (pane, before)
        };
        if !input.quiet() || input.epoch.load(Ordering::SeqCst) != epoch {
            return Err(Error::Unavailable);
        }
        *reason = Reason::ProbeCooldown;
        {
            let now = Instant::now();
            let mut sent = SENT
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            sent.retain(|_, at| now.duration_since(*at) < COOLDOWN);
            if sent.contains_key(self.identity) || sent.len() >= 128 {
                return Err(Error::Busy);
            }
            sent.insert(self.identity.clone(), now);
        }
        // Codex treats text + CR in one burst as multiline input. Send only the
        // fixed literal command, let its paste burst settle, then submit a named
        // Enter only if the entire composer still contains exactly our command.
        let send = format!("send-keys -t {} -H 2f 73 74 61 74 75 73", pane.id);
        *reason = Reason::ProbeSend;
        critical.command(
            &target,
            vec![
                "if-shell".into(),
                "-F".into(),
                "-t".into(),
                self.identity.id.clone(),
                pane.guard(),
                send,
            ],
        )?;
        critical.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = critical.stop.cancelled() => Err(Error::Cancelled),
                _ = tokio::time::sleep_until(critical.deadline.into()) => Err(Error::Cancelled),
                _ = tokio::time::sleep(Duration::from_millis(120)) => Ok(())
            }
        })?;
        let typed = critical.metadata(&target, &self.identity.id)?;
        let typed_screen = critical.screen(&target, &typed)?;
        if !same_input_owner(&pane, &typed)
            || !owned_prompt(&typed_screen, &typed)
            || !input.quiet()
            || input.epoch.load(Ordering::SeqCst) != epoch
        {
            return Err(Error::Unavailable);
        }
        if critical.inspector.process_stamp(
            self.base.provider_pid,
            self.stop,
            critical.deadline,
            self.runtime,
        )? != stamp
            || !critical.inspector.foreground(
                self.base.provider_pid,
                self.stop,
                critical.deadline,
                self.runtime,
            )?
        {
            return Err(Error::Unavailable);
        }
        critical.command(
            &target,
            vec![
                "if-shell".into(),
                "-F".into(),
                "-t".into(),
                self.identity.id.clone(),
                typed.guard(),
                format!("send-keys -t {} Enter", typed.id),
            ],
        )?;
        drop(gate);
        let unchanged_input = || input.quiet() && input.epoch.load(Ordering::SeqCst) == epoch;
        let until = (Instant::now() + Duration::from_secs(2)).min(self.deadline);
        let polling = Self {
            deadline: until,
            ..*self
        };
        *reason = Reason::ProbeStatus;
        let id = loop {
            inspection::check(self.stop, until)?;
            let after = polling.screen(&target, &pane)?;
            if !unchanged_input() {
                return Err(Error::Unavailable);
            }
            if after != before && status_panel(&after) {
                if let Ok(Some(id)) = session_id(&after) {
                    break id;
                }
            }
            self.runtime.block_on(async {tokio::select!{biased;_=self.stop.cancelled()=>Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(80))=>Ok(())}})?;
        };
        drop(width);
        *reason = Reason::ProbeProcess;
        self.current(&stamp)?;
        let after = self.metadata(&target, &self.identity.id)?;
        if after.id != pane.id {
            return Err(Error::Unavailable);
        }
        *reason = Reason::ProbeRecord;
        let path = records::find_codex_thread(
            &self.inspector.sessions_root(),
            &id,
            self.stop,
            self.deadline,
        )
        .map_err(|_| Error::Unavailable)?;
        let link = Link::new(
            self.identity.clone(),
            self.pane,
            self.base,
            stamp.clone(),
            (id, path),
            self.stop,
            self.deadline,
        )?;
        *reason = Reason::ProbeProcess;
        self.current(&stamp)?;
        *reason = Reason::ProbeInput;
        if !unchanged_input() {
            return Err(Error::Unavailable);
        }
        inspection::check(self.stop, self.deadline)?;
        *reason = Reason::ProbeSave;
        conversation_link::save_recovered(self.state_dir, self.identity, &link, previous)
    }
}

#[cfg(test)]
#[path = "status_probe_tests.rs"]
mod tests;

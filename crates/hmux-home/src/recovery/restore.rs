use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};

const CREATED_SESSION: &str = "#{session_id}|:hmux-recovery-v1:|#{window_id}|:hmux-recovery-v1:|#{pane_id}|:hmux-recovery-v1:|#{session_created}|:hmux-recovery-v1:|#{window_index}";
const CREATED_WINDOW: &str =
    "#{window_id}|:hmux-recovery-v1:|#{pane_id}|:hmux-recovery-v1:|#{window_index}";
const GATE_SCRIPT: &str = "while [ ! -f \"$1\" ]; do /bin/sleep 1; done; shift; exec \"$@\"";
const PROVIDER_SCRIPT: &str =
    "shell=$1; shift\ntrap ':' INT QUIT\n(trap - INT QUIT; exec \"$@\")\nexec \"$shell\" -i";

impl Store {
    pub(super) async fn restore_locked(
        &self,
        dir: &PrivateDir,
        boot: &str,
        state: &mut DiskState,
    ) -> Result<(), Error> {
        self.check_cancel()?;
        if state.pending.as_ref().is_none_or(|p| p.boot_id != boot) {
            state.pending = Some(Pending {
                boot_id: boot.into(),
                snapshot: state.checkpoint.clone(),
                completed: BTreeMap::new(),
                intents: BTreeMap::new(),
            });
            self.check_cancel()?;
            Self::write(dir, state)?;
        }
        if state.boot_id != boot
            && state
                .pending
                .as_ref()
                .is_some_and(|p| p.snapshot.sessions.is_empty())
        {
            self.start_empty_server().await?;
        }
        self.restore_snapshot(dir, state).await?;
        let mut live = self.capture().await?;
        let pending = state.pending.as_ref().ok_or(Error::Invalid)?;
        if !pending.snapshot.sessions.is_empty() && live.sessions.is_empty() {
            return Err(Error::Changed);
        }
        for mapping in pending.completed.values() {
            self.verify_panes(mapping).await?;
        }
        let mut rebased = pending.snapshot.clone();
        rebased.sessions.retain_mut(|s| {
            if let Some(m) = pending.completed.get(&key(&s.identity)) {
                s.identity = m.to.clone();
                true
            } else {
                false
            }
        });
        merge_resume(&mut live, &rebased);
        let added: Vec<_> = pending.completed.values().cloned().collect();
        self.rebase_workspace(&state.mappings, &added, &live)?;
        let mut mappings = Vec::new();
        let (mut from, mut to) = (BTreeSet::new(), BTreeSet::new());
        for m in added.iter().chain(&state.mappings) {
            if live
                .sessions
                .iter()
                .any(|s| s.identity == m.to && s.name == m.name)
                && from.insert(key(&m.from))
                && to.insert(key(&m.to))
            {
                mappings.push(m.clone());
            }
        }
        self.check_cancel()?;
        state.mappings = mappings;
        state.checkpoint = live;
        state.boot_id = boot.into();
        state.pending = None;
        Self::write(dir, state)
    }
    async fn restore_snapshot(&self, dir: &PrivateDir, state: &mut DiskState) -> Result<(), Error> {
        let live = self.basic().await?.sessions.unwrap_or_default();
        let mut by_id: BTreeMap<_, _> = live
            .iter()
            .map(|s| (format!("{}/{}", s.id, s.created_at), s.clone()))
            .collect();
        let mut by_name: BTreeSet<String> = live.iter().map(|s| s.name.clone()).collect();
        let saved = state
            .pending
            .as_ref()
            .ok_or(Error::Invalid)?
            .snapshot
            .sessions
            .clone();
        for session in saved {
            self.check_cancel()?;
            let from = key(&session.identity);
            if by_id.contains_key(&from) {
                continue;
            }
            if let Some(done) = state
                .pending
                .as_ref()
                .and_then(|p| p.completed.get(&from))
                .cloned()
            {
                let Some(current) = by_id.get(&key(&done.to)) else {
                    return Err(Error::Changed);
                };
                if current.name != done.name && current.name != temporary_name(&done.gate) {
                    return Err(Error::Changed);
                }
                self.launch_providers(&done).await?;
                continue;
            }
            if by_name.contains(&session.name) {
                continue;
            }
            let gate = if let Some(gate) = state.pending.as_ref().and_then(|p| p.intents.get(&from))
            {
                gate.clone()
            } else {
                let gate = self.new_gate(dir)?;
                state
                    .pending
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .intents
                    .insert(from.clone(), gate.clone());
                self.check_cancel()?;
                Self::write(dir, state)?;
                gate
            };
            self.validate_gate(&gate)?;
            let temp = temporary_name(&gate);
            if let Some(orphan) = live.iter().find(|s| s.name == temp) {
                let start = self
                    .command(
                        vec![
                            "display-message".into(),
                            "-p".into(),
                            "-t".into(),
                            orphan.id.clone(),
                            "#{pane_start_command}".into(),
                        ],
                        4096,
                    )
                    .await?;
                if !String::from_utf8_lossy(&start).contains(&gate) {
                    return Err(Error::Changed);
                }
                self.terminate_expected(&SessionIdentity {
                    id: orphan.id.clone(),
                    created_at: orphan.created_at,
                })
                .await?;
            }
            let mapping = self.create_session(&session, &gate).await?;
            state
                .pending
                .as_mut()
                .ok_or(Error::Invalid)?
                .completed
                .insert(from.clone(), mapping.clone());
            if let Err(e) = Self::write(dir, state) {
                let _ = self.terminate_expected(&mapping.to).await;
                state
                    .pending
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .completed
                    .remove(&from);
                return Err(e);
            }
            self.launch_providers(&mapping).await?;
            by_name.insert(session.name.clone());
            by_id.insert(
                key(&mapping.to),
                Session {
                    id: mapping.to.id.clone(),
                    name: mapping.name.clone(),
                    created_at: mapping.to.created_at,
                    ..Session::default()
                },
            );
        }
        Ok(())
    }
    fn new_gate(&self, dir: &PrivateDir) -> Result<String, Error> {
        for _ in 0..16 {
            let mut bytes = [0u8; 12];
            getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
            let name = format!("launch-{}", hex(&bytes));
            if let Some(gate) = self.reserve_gate_name(dir, &name)? {
                return Ok(gate);
            }
        }
        Err(Error::Unavailable)
    }
    // mkdir is exclusive: an old ready file must never release a newly
    // restored provider. The recovery root is private and held by state.lock.
    fn reserve_gate_name(&self, dir: &PrivateDir, name: &str) -> Result<Option<String>, Error> {
        if !name.starts_with("launch-")
            || name.len() != 31
            || !name[7..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid);
        }
        let root = self.state_dir.join("recovery");
        let path = root.join(name);
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
            Err(_) => return Err(Error::Unavailable),
        }
        dir.create_private_child(OsStr::new(name))
            .map_err(|_| Error::Unavailable)?;
        fs::File::open(&root)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Unavailable)?;
        Ok(Some(path.join("ready").to_string_lossy().into_owned()))
    }
    fn validate_gate(&self, gate: &str) -> Result<PrivateDir, Error> {
        let path = Path::new(gate);
        if path.parent().and_then(Path::parent) != Some(self.state_dir.join("recovery").as_path())
            || path.file_name() != Some(OsStr::new("ready"))
            || !path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|s| s.to_string_lossy().starts_with("launch-"))
        {
            return Err(Error::Invalid);
        }
        PrivateDir::open(path.parent().ok_or(Error::Invalid)?).map_err(|_| Error::Unavailable)
    }
    async fn start_empty_server(&self) -> Result<(), Error> {
        if !self.basic().await?.sessions.unwrap_or_default().is_empty() {
            return Ok(());
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let shell = shell();
        let result = self
            .command(
                vec![
                    "new-session".into(),
                    "-d".into(),
                    "-s".into(),
                    "hmux".into(),
                    "-c".into(),
                    home,
                    shell,
                    "-l".into(),
                ],
                4096,
            )
            .await;
        if result.is_err() && self.basic().await?.sessions.unwrap_or_default().is_empty() {
            return Err(Error::Command);
        }
        Ok(())
    }
    async fn create_session(&self, saved: &SavedSession, gate: &str) -> Result<Mapping, Error> {
        let executables = provider_executables(saved)?;
        let first = &saved.windows[0];
        let pane = &first.panes[0];
        let mut args = vec![
            "new-session".into(),
            "-d".into(),
            "-P".into(),
            "-F".into(),
            CREATED_SESSION.into(),
            "-s".into(),
            temporary_name(gate),
            "-n".into(),
            first.name.clone(),
            "-c".into(),
            pane.cwd.clone(),
        ];
        gated(&mut args, pane, &executables, gate);
        let raw = self.command(args, 4096).await?;
        let f = fields(&raw, 5)?;
        if !valid_tmux(&f[0], b'$') || !valid_tmux(&f[1], b'@') || !valid_tmux(&f[2], b'%') {
            return Err(Error::Invalid);
        }
        let created = f[3].parse::<i64>().map_err(|_| Error::Invalid)?;
        let initial = f[4].parse::<u32>().map_err(|_| Error::Invalid)?;
        if created < 1 {
            return Err(Error::Invalid);
        }
        let id = SessionIdentity {
            id: f[0].clone(),
            created_at: created,
        };
        let result = self
            .build_session(saved, gate, &executables, (&f[1], &f[2], initial), &id)
            .await;
        if result.is_err() {
            let _ = self.terminate_expected(&id).await;
        }
        result
    }
    async fn build_session(
        &self,
        saved: &SavedSession,
        gate: &str,
        executables: &BTreeMap<String, String>,
        first: (&str, &str, u32),
        id: &SessionIdentity,
    ) -> Result<Mapping, Error> {
        let (first_window, first_pane, initial) = first;
        let mut windows = vec![(first_window.to_owned(), vec![first_pane.to_owned()])];
        if initial != saved.windows[0].index {
            self.command(
                vec![
                    "move-window".into(),
                    "-s".into(),
                    first_window.into(),
                    "-t".into(),
                    format!("{}:{}", id.id, saved.windows[0].index),
                ],
                4096,
            )
            .await?;
        }
        for window in saved.windows.iter().skip(1) {
            let mut args = vec![
                "new-window".into(),
                "-d".into(),
                "-P".into(),
                "-F".into(),
                CREATED_WINDOW.into(),
                "-t".into(),
                format!("{}:{}", id.id, window.index),
                "-n".into(),
                window.name.clone(),
                "-c".into(),
                window.panes[0].cwd.clone(),
            ];
            gated(&mut args, &window.panes[0], executables, gate);
            let f = fields(&self.command(args, 4096).await?, 3)?;
            if !valid_tmux(&f[0], b'@')
                || !valid_tmux(&f[1], b'%')
                || f[2] != window.index.to_string()
            {
                return Err(Error::Invalid);
            }
            windows.push((f[0].clone(), vec![f[1].clone()]));
        }
        for (wi, (window_id, pane_ids)) in windows.iter_mut().enumerate() {
            let window = &saved.windows[wi];
            for pane in window.panes.iter().skip(1) {
                let mut args = vec![
                    "split-window".into(),
                    "-d".into(),
                    "-P".into(),
                    "-F".into(),
                    "#{pane_id}".into(),
                    "-t".into(),
                    pane_ids.last().ok_or(Error::Invalid)?.clone(),
                    "-c".into(),
                    pane.cwd.clone(),
                ];
                gated(&mut args, pane, executables, gate);
                let pane_id = String::from_utf8(self.command(args, 4096).await?)
                    .map_err(|_| Error::Invalid)?
                    .trim()
                    .to_owned();
                if !valid_tmux(&pane_id, b'%') {
                    return Err(Error::Invalid);
                }
                pane_ids.push(pane_id);
                self.command(
                    vec![
                        "select-layout".into(),
                        "-t".into(),
                        window_id.clone(),
                        "tiled".into(),
                    ],
                    4096,
                )
                .await?;
            }
            self.command(
                vec![
                    "select-layout".into(),
                    "-t".into(),
                    window_id.clone(),
                    window.layout.clone(),
                ],
                4096,
            )
            .await?;
            if let Some((pi, _)) = window.panes.iter().enumerate().find(|(_, p)| p.active) {
                self.command(
                    vec!["select-pane".into(), "-t".into(), pane_ids[pi].clone()],
                    4096,
                )
                .await?;
            }
        }
        if let Some((wi, _)) = saved.windows.iter().enumerate().find(|(_, w)| w.active) {
            self.command(
                vec!["select-window".into(), "-t".into(), windows[wi].0.clone()],
                4096,
            )
            .await?;
        }
        let f=fields(&self.command(vec!["display-message".into(),"-p".into(),"-t".into(),id.id.clone(),"#{session_id}|:hmux-recovery-v1:|#{session_name}|:hmux-recovery-v1:|#{session_created}".into()],4096).await?,3)?;
        if f[0] != id.id || f[1] != temporary_name(gate) || f[2] != id.created_at.to_string() {
            return Err(Error::Changed);
        }
        let mut restored = Session {
            id: id.id.clone(),
            created_at: id.created_at,
            name: saved.name.clone(),
            alias: saved.alias.clone(),
            profile: saved.profile.clone(),
            label: saved.label.clone(),
            tags: Some(saved.tags.clone()),
            ..Session::default()
        };
        let metadata = sessionstate::Store::new(self.state_dir.clone());
        metadata
            .import_legacy(
                &[restored.clone()],
                CancellationToken::new(),
                self.operation_deadline.unwrap_or(Instant::now() + OP),
            )
            .map_err(|_| Error::Unavailable)?;
        if saved.hidden {
            metadata
                .set_hidden_expected(
                    id,
                    true,
                    CancellationToken::new(),
                    self.operation_deadline.unwrap_or(Instant::now() + OP),
                    || Ok(restored.clone()),
                )
                .map_err(|_| Error::Unavailable)?;
        }
        restored.hidden = saved.hidden;
        let mut panes = BTreeMap::new();
        for (wi, w) in saved.windows.iter().enumerate() {
            for (pi, p) in w.panes.iter().enumerate() {
                panes.insert(
                    format!("{}/{}", w.index, p.index),
                    windows[wi].1[pi].clone(),
                );
            }
        }
        Ok(Mapping {
            from: saved.identity.clone(),
            to: id.clone(),
            name: saved.name.clone(),
            panes,
            gate: gate.into(),
        })
    }
    async fn launch_providers(&self, m: &Mapping) -> Result<(), Error> {
        if m.gate.is_empty() {
            return Ok(());
        }
        let gate_dir = self.validate_gate(&m.gate)?;
        self.verify_panes(m).await?;
        let name = String::from_utf8(
            self.command(
                vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    m.to.id.clone(),
                    "#{session_name}".into(),
                ],
                4096,
            )
            .await?,
        )
        .map_err(|_| Error::Invalid)?
        .trim()
        .to_owned();
        if name == temporary_name(&m.gate) {
            self.command(
                vec![
                    "rename-session".into(),
                    "-t".into(),
                    m.to.id.clone(),
                    m.name.clone(),
                ],
                4096,
            )
            .await?;
        } else if name != m.name {
            return Err(Error::Changed);
        }
        self.check_cancel()?;
        gate_dir
            .write_atomic_private(OsStr::new("ready"), b"ready\n")
            .map_err(|_| Error::Unavailable)
    }
    async fn verify_panes(&self, m: &Mapping) -> Result<(), Error> {
        let f = fields(
            &self
                .command(
                    vec![
                        "display-message".into(),
                        "-p".into(),
                        "-t".into(),
                        m.to.id.clone(),
                        "#{session_id}|:hmux-recovery-v1:|#{session_created}".into(),
                    ],
                    4096,
                )
                .await?,
            2,
        )?;
        if f[0] != m.to.id || f[1] != m.to.created_at.to_string() {
            return Err(Error::Changed);
        }
        let raw=self.command(vec!["list-panes".into(),"-s".into(),"-t".into(),m.to.id.clone(),"-F".into(),"#{pane_id}|:hmux-recovery-v1:|#{window_index}|:hmux-recovery-v1:|#{pane_index}".into()],MAX_STATE).await?;
        let value = std::str::from_utf8(&raw).map_err(|_| Error::Changed)?;
        let mut seen = BTreeSet::new();
        for row in value.lines() {
            let f: Vec<_> = row.split(SEP).collect();
            if f.len() != 3 {
                return Err(Error::Changed);
            }
            let position = format!("{}/{}", f[1], f[2]);
            if m.panes.get(&position).is_none_or(|id| id != f[0]) || !seen.insert(position) {
                return Err(Error::Changed);
            }
        }
        if seen.len() != m.panes.len() {
            return Err(Error::Changed);
        }
        Ok(())
    }
    async fn terminate_expected(&self, id: &SessionIdentity) -> Result<(), Error> {
        if !valid_id(id) {
            return Err(Error::Invalid);
        }
        let raw = self
            .command(
                vec![
                    "if-shell".into(),
                    "-F".into(),
                    "-t".into(),
                    id.id.clone(),
                    format!("#{{==:#{{session_created}},{}}}", id.created_at),
                    format!("kill-session -t {}", id.id),
                    "display-message -p hmux-session-changed".into(),
                ],
                4096,
            )
            .await?;
        if !raw.is_empty() {
            return Err(Error::Changed);
        }
        Ok(())
    }
    fn rebase_workspace(
        &self,
        previous: &[Mapping],
        added: &[Mapping],
        live: &Snapshot,
    ) -> Result<(), Error> {
        let path = self.state_dir.join("shared-workspace");
        let dir = match PrivateDir::open(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Error::Unavailable),
        };
        let _lock = dir
            .lock_for(OsStr::new("lock"), Duration::from_secs(3))
            .map_err(|_| Error::Busy)?;
        let raw = match dir.read_private(OsStr::new("workspace.json"), workspace::MAX_BYTES) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Error::Unavailable),
        };
        let current = workspace::Snapshot::decode(&raw).map_err(|_| Error::Invalid)?;
        let lineages = workspace_lineages(previous, added, live);
        let update = workspace::reconcile(current, None, &lineages).map_err(|_| Error::Invalid)?;
        if update.changed {
            let bytes = serde_json::to_vec(&update.state).map_err(|_| Error::Invalid)?;
            if bytes.len() > workspace::MAX_BYTES {
                return Err(Error::Invalid);
            }
            let verify = dir
                .read_private(OsStr::new("workspace.json"), workspace::MAX_BYTES)
                .map_err(|_| Error::Unavailable)?;
            if verify != raw {
                return Err(Error::Changed);
            }
            dir.write_atomic_private(OsStr::new("workspace.json"), &bytes)
                .map_err(|_| Error::Unavailable)?;
        }
        Ok(())
    }
}
fn fields(raw: &[u8], count: usize) -> Result<Vec<String>, Error> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::Invalid)?.trim();
    let f: Vec<_> = text.split(SEP).map(str::to_owned).collect();
    if f.len() != count {
        return Err(Error::Invalid);
    }
    Ok(f)
}
fn temporary_name(gate: &str) -> String {
    format!(
        "hmux-recovery-{}",
        Path::new(gate)
            .parent()
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .trim_start_matches("launch-")
    )
}
fn shell() -> String {
    for value in [
        std::env::var("SHELL").unwrap_or_default(),
        "/bin/zsh".into(),
        "/bin/bash".into(),
        "/bin/sh".into(),
    ] {
        let p = Path::new(&value);
        if p.is_absolute()
            && fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return value;
        }
    }
    "/bin/sh".into()
}
fn provider_executables(saved: &SavedSession) -> Result<BTreeMap<String, String>, Error> {
    let mut result = BTreeMap::new();
    for p in saved.windows.iter().flat_map(|w| &w.panes) {
        if let Some(r) = &p.resume {
            if !result.contains_key(&r.provider) {
                let path = find_executable(&r.provider)?;
                result.insert(r.provider.clone(), path);
            }
        }
    }
    Ok(result)
}
fn find_executable(provider: &str) -> Result<String, Error> {
    find_executable_in(
        provider,
        &std::env::var("PATH").unwrap_or_default(),
        std::env::var("HOME").ok().as_deref(),
    )
}
fn validate_executable(candidate: &Path) -> Option<(PathBuf, PathBuf)> {
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(candidate)
    };
    let resolved = absolute.canonicalize().ok()?;
    let meta = fs::metadata(&resolved).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some((absolute, resolved))
}
fn find_executable_in(provider: &str, path: &str, home: Option<&str>) -> Result<String, Error> {
    if !matches!(provider, "codex" | "claude") {
        return Err(Error::Invalid);
    }
    for entry in path.split(':') {
        if !Path::new(entry).is_absolute() {
            continue;
        }
        let candidate = Path::new(entry).join(provider);
        if let Some((original, _)) = validate_executable(&candidate) {
            return Ok(original.to_string_lossy().into_owned());
        }
    }
    let mut candidates = Vec::new();
    if let Some(home) = home.filter(|h| Path::new(h).is_absolute()) {
        let home = Path::new(home);
        candidates.push(home.join(".local/bin").join(provider));
        let versions = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(versions) {
            let mut count = 0;
            for entry in entries {
                let entry = entry.map_err(|_| Error::Unavailable)?;
                count += 1;
                if count > 128 {
                    return Err(Error::Invalid);
                }
                if entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                {
                    candidates.push(entry.path().join("bin").join(provider));
                }
            }
        }
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(provider));
    candidates.push(PathBuf::from("/usr/local/bin").join(provider));
    let mut found = BTreeMap::new();
    for candidate in candidates {
        if let Some((original, resolved)) = validate_executable(&candidate) {
            found.insert(resolved, original);
        }
    }
    if found.len() != 1 {
        return Err(Error::Unavailable);
    }
    Ok(found
        .into_values()
        .next()
        .ok_or(Error::Unavailable)?
        .to_string_lossy()
        .into_owned())
}
fn gated(
    args: &mut Vec<String>,
    pane: &SavedPane,
    executables: &BTreeMap<String, String>,
    gate: &str,
) {
    let mut launch = Vec::new();
    if let Some(r) = &pane.resume {
        let executable = &executables[&r.provider];
        let dir = Path::new(executable)
            .parent()
            .unwrap_or(Path::new("/usr/bin"));
        if dir
            .join("node")
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            args.extend([
                "-e".into(),
                format!(
                    "PATH={}:{}",
                    dir.display(),
                    "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin"
                ),
            ]);
        }
        args.extend([
            "-e".into(),
            format!(
                "{}={}",
                if r.provider == "codex" {
                    "CODEX_HOME"
                } else {
                    "CLAUDE_CONFIG_DIR"
                },
                r.config_dir
            ),
        ]);
        launch.extend([
            "/bin/sh".into(),
            "-c".into(),
            PROVIDER_SCRIPT.into(),
            "hmux-provider".into(),
            shell(),
            executable.clone(),
        ]);
        if r.provider == "codex" {
            launch.push("resume".into())
        } else {
            launch.push("--resume".into())
        }
        launch.push(r.session_id.clone());
    } else {
        launch.extend([shell(), "-il".into()]);
    }
    args.extend([
        "/bin/sh".into(),
        "-c".into(),
        GATE_SCRIPT.into(),
        "hmux-recovery".into(),
        gate.into(),
    ]);
    args.extend(launch)
}
pub(super) fn hex(raw: &[u8]) -> String {
    const H: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(raw.len() * 2);
    for b in raw {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}
fn merge_resume(current: &mut Snapshot, previous: &Snapshot) {
    let mut old = BTreeMap::new();
    for s in &previous.sessions {
        for w in &s.windows {
            for p in &w.panes {
                if let Some(r) = &p.resume {
                    old.insert((key(&s.identity), w.index, p.index), r.clone());
                }
            }
        }
    }
    for s in &mut current.sessions {
        for w in &mut s.windows {
            for p in &mut w.panes {
                if p.resume.is_none() {
                    p.resume = old.get(&(key(&s.identity), w.index, p.index)).cloned()
                }
            }
        }
    }
}
pub(super) fn workspace_lineages(
    previous: &[Mapping],
    added: &[Mapping],
    live: &Snapshot,
) -> Vec<SessionLineage> {
    let mut result: Vec<_> = live
        .sessions
        .iter()
        .map(|s| SessionLineage {
            id: s.identity.id.clone(),
            created_at: s.identity.created_at,
            restored_from: None,
        })
        .collect();
    let links: Vec<_> = added.iter().chain(previous).collect();
    for m in &links {
        let mut target = m.to.clone();
        let mut name = m.name.clone();
        let mut seen = BTreeSet::from([key(&m.from)]);
        for _ in 0..=links.len() {
            if !seen.insert(key(&target)) {
                break;
            }
            if live
                .sessions
                .iter()
                .any(|s| s.identity == target && s.name == name)
            {
                result.push(SessionLineage {
                    id: target.id.clone(),
                    created_at: target.created_at,
                    restored_from: Some(m.from.clone()),
                });
                break;
            }
            let next: Vec<_> = links.iter().filter(|l| l.from == target).collect();
            if next.len() != 1 {
                break;
            }
            target = next[0].to.clone();
            name = next[0].name.clone();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture() -> (Store, PathBuf) {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-gate-{}", hex(&random)));
        fs::create_dir(&root).unwrap();
        let store = Store::new(
            root.join("state"),
            PathBuf::from("/bin/sh"),
            None,
            CommandRunner::new(1).unwrap(),
            Arc::new(|_| Box::pin(async { Ok(BTreeMap::new()) })),
        )
        .unwrap();
        (store, root)
    }
    #[test]
    fn gate_reservation_never_reuses_ready_directory() {
        let (store, root) = fixture();
        let dir = store.dir().unwrap();
        let name = "launch-0123456789abcdef01234567";
        let old = store.state_dir.join("recovery").join(name);
        fs::create_dir(&old).unwrap();
        fs::write(old.join("ready"), "ready\n").unwrap();
        assert_eq!(store.reserve_gate_name(&dir, name).unwrap(), None);
        let new = store
            .reserve_gate_name(&dir, "launch-0123456789abcdef01234568")
            .unwrap()
            .unwrap();
        assert!(!Path::new(&new).exists());
        assert_eq!(
            fs::metadata(Path::new(&new).parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn provider_path_precedes_fallback() {
        let (_, root) = fixture();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let executable = bin.join("codex");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            find_executable_in("codex", bin.to_str().unwrap(), Some(root.to_str().unwrap()))
                .unwrap(),
            executable.to_string_lossy()
        );
        let _ = fs::remove_dir_all(root);
    }
}

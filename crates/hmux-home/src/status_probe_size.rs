//! Temporary single-pane width for Codex status, with guarded policy restoration.
use super::*;
const FORMAT: &str = "#{window_id}|#{window_panes}|#{window_width}|#{window_height}|#{window-size}";
pub(super) struct Width<'a, 't> {
    probe: Probe<'a>,
    target: &'t Target,
    original: Pane,
    window: String,
    width: usize,
    height: usize,
    policy: String,
    local: bool,
    armed: bool,
    marker: String,
}
impl<'a, 't> Width<'a, 't> {
    pub fn widen(probe: Probe<'a>, target: &'t Target, pane: &Pane) -> Result<Self, Error> {
        let raw = probe.command(
            target,
            vec![
                "display-message".into(),
                "-p".into(),
                "-t".into(),
                probe.identity.id.clone(),
                FORMAT.into(),
            ],
        )?;
        let text = std::str::from_utf8(&raw).map_err(|_| Error::Unavailable)?;
        let fields: Vec<_> = text.trim().split('|').collect();
        if fields.len() != 5
            || fields[1] != "1"
            || !fields[0].strip_prefix('@').is_some_and(|id| {
                !id.is_empty() && id.len() < 20 && id.bytes().all(|b| b.is_ascii_digit())
            })
            || !["latest", "largest", "smallest", "manual"].contains(&fields[4])
        {
            return Err(Error::Unavailable);
        }
        let width = fields[2].parse::<usize>().map_err(|_| Error::Unavailable)?;
        let height = fields[3].parse::<usize>().map_err(|_| Error::Unavailable)?;
        if width.to_string() != pane.fields[6] || height.to_string() != pane.fields[7] {
            return Err(Error::Unavailable);
        }
        let option = probe.command(
            target,
            vec![
                "show-options".into(),
                "-w".into(),
                "-t".into(),
                fields[0].into(),
                "window-size".into(),
            ],
        )?;
        let option = std::str::from_utf8(&option)
            .map_err(|_| Error::Unavailable)?
            .trim();
        if !option.is_empty() && option != format!("window-size {}", fields[4]) {
            return Err(Error::Unavailable);
        }
        let old_marker = probe.command(
            target,
            vec![
                "show-options".into(),
                "-wqv".into(),
                "-t".into(),
                fields[0].into(),
                "@hmux_status_probe".into(),
            ],
        )?;
        if !old_marker.is_empty() {
            return Err(Error::Unavailable);
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| Error::Unavailable)?;
        let marker: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let lease = Self {
            probe,
            target,
            original: pane.clone(),
            window: fields[0].into(),
            width,
            height,
            policy: fields[4].into(),
            local: !option.is_empty(),
            armed: true,
            marker,
        };
        let guard = format!(
            "#{{&&:{},#{{&&:#{{==:#{{window_id}},{}}},#{{&&:#{{==:#{{window-size}},{}}},#{{==:#{{@hmux_status_probe}},}}}}}}}}",
            pane.guard(), lease.window, lease.policy
        );
        probe.command(
            target,
            vec![
                "if-shell".into(),
                "-F".into(),
                "-t".into(),
                probe.identity.id.clone(),
                guard,
                format!(
                    "set-option -w -t {} @hmux_status_probe {} ; resize-window -t {} -x 80 -y {}",
                    lease.window, lease.marker, lease.window, height
                ),
            ],
        )?;
        let resized = probe.metadata(target, &probe.identity.id)?;
        if resized.fields[6] != "80" || resized.id != pane.id {
            return Err(Error::Unavailable);
        }
        // Disarm only via restore; errors after any attempted resize also unwind.
        Ok(lease)
    }
    fn restore(&mut self) -> Result<(), Error> {
        if !self.armed {
            return Ok(());
        }
        let guard = format!(
            "#{{&&:{},#{{&&:#{{==:#{{window_id}},{}}},#{{==:#{{@hmux_status_probe}},{}}}}}}}",
            self.original.owner_guard(),
            self.window,
            self.marker
        );
        let owned_size=format!("#{{&&:#{{==:#{{window_panes}},1}},#{{&&:#{{==:#{{window_width}},80}},#{{&&:#{{==:#{{window_height}},{}}},#{{==:#{{window-size}},manual}}}}}}}}",self.height);
        let policy = if self.local {
            format!(
                "set-option -w -t {} window-size {}",
                self.window, self.policy
            )
        } else {
            format!("set-option -wu -t {} window-size", self.window)
        };
        // Seed the old dimensions even with an automatic policy: a detached
        // window has no client from which tmux can recalculate after unsetting.
        // Restoring the policy afterward still lets current clients take priority.
        let restore = format!(
            "resize-window -t {} -x {} -y {} ; {}",
            self.window, self.width, self.height, policy
        );
        // A same-value external change cannot be distinguished from our resize;
        // the nonce and expected size/policy prevent other observable conflicts.
        let selector = format!(
            "{}:{}.{}",
            self.probe.identity.id, self.window, self.original.id
        );
        let command = format!(
            "if-shell -F -t {} '{}' '{}' ; set-option -wu -t {} @hmux_status_probe",
            selector, owned_size, restore, self.window
        );
        let stop = CancellationToken::new();
        self.probe
            .runtime
            .block_on(self.target.command(
                inspection::commands(),
                vec![
                    "if-shell".into(),
                    "-F".into(),
                    "-t".into(),
                    selector,
                    guard,
                    command,
                ],
                &stop,
                (Instant::now() + Duration::from_millis(250)).into(),
            ))
            .map_err(|_| Error::Unavailable)?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for Width<'_, '_> {
    fn drop(&mut self) {
        if self.restore().is_err() {
            if let Some(report) = self.probe.reporter {
                report(observation::Event::new(
                    observation::Stage::StatusProbe,
                    Some(hmux_protocol::protobuf::types::Operation::Conversation),
                    Reason::ProbeRestore,
                    Instant::now(),
                ));
            }
        }
    }
}

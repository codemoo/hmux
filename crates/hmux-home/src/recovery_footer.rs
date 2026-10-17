//! Current Codex footer identity, independent of retained transcript panels.
//! Reads the configured live UUID footer without sending terminal input.
use super::*;

fn footer_id(screen: &str, pane: &Pane) -> Result<String, Error> {
    if !empty_prompt(screen, pane) {
        return Err(Error::Unavailable);
    }
    let ids: Vec<_> = screen
        .lines()
        .skip(pane.y + 2)
        .filter_map(|line| {
            let s = line.trim();
            uuid(s).then_some(s)
        })
        .collect();
    match ids.as_slice() {
        [id] => Ok((*id).into()),
        _ => Err(Error::Unavailable),
    }
}
impl Probe<'_> {
    fn footer_owner(&self, stamp: &str) -> Result<(), Error> {
        if self.inspector.process_stamp(
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
    fn resolve_footer(
        &self,
        target: &Target,
        stamp: &str,
        input: &input_gate::State,
        epoch: u64,
    ) -> Result<Binding, Error> {
        let unchanged = || input.quiet() && input.epoch.load(Ordering::SeqCst) == epoch;
        if !unchanged() {
            return Err(Error::Unavailable);
        }
        self.footer_owner(stamp)?;
        let pane = self.metadata(target, self.selector())?;
        let screen = self.screen(target, &pane)?;
        let id = footer_id(&screen, &pane)?;
        let path = records::find_codex_thread(
            &self.inspector.sessions_root(),
            &id,
            self.stop,
            self.deadline,
        )
        .map_err(|_| Error::Unavailable)?;
        let binding = records::bind_codex(
            Binding::unavailable(Provider::Codex, self.base.provider_pid),
            0,
            &[path],
            false,
            self.stop,
            self.deadline,
        );
        if binding.status != Status::Ready || binding.record_id != id {
            return Err(Error::Unavailable);
        }
        self.footer_owner(stamp)?;
        let after = self.metadata(target, self.selector())?;
        if after.id != pane.id
            || footer_id(&self.screen(target, &after)?, &after)? != id
            || !unchanged()
        {
            return Err(Error::Unavailable);
        }
        Ok(binding)
    }
    /// Read-only: unsupported/missing footer configuration never sends input.
    /// Older CLIs can forward unknown slash arguments as an agent prompt.
    pub(crate) fn run_footer(&self) -> Result<Binding, Error> {
        if self.base.provider != Provider::Codex || self.base.provider_pid < 1 {
            return Err(Error::Unavailable);
        }
        let input = input_gate::state(self.identity);
        let epoch = input.epoch.load(Ordering::SeqCst);
        let target = self
            .reader
            .terminal_target()
            .map_err(|_| Error::Unavailable)?;
        let stamp = self.inspector.process_stamp(
            self.base.provider_pid,
            self.stop,
            self.deadline,
            self.runtime,
        )?;
        self.resolve_footer(&target, &stamp, &input, epoch)
    }
}

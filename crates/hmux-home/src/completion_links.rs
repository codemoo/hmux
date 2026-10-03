//! Opt-in pinned-thread completion sources, separate from automatic discovery.
use super::{Observation, Target};
use crate::{
    binding::Status,
    catalog::TmuxCatalogReader,
    conversation_link,
    inspection::{self, Error, Inspector, Scan, ScanPurpose},
};
use std::{path::Path, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

pub(super) struct Sources<'a> {
    pub state: &'a Path,
    pub reader: &'a TmuxCatalogReader,
    pub inspector: &'a Inspector,
    pub stop: &'a CancellationToken,
    pub deadline: Instant,
    pub runtime: &'a tokio::runtime::Handle,
}
impl Sources<'_> {
    pub fn observations(&self, targets: &[Target], scan: &Scan) -> Result<Vec<Observation>, Error> {
        let mut values: Vec<_> = targets
            .iter()
            .map(|t| {
                let binding = t.pane.and_then(|p| scan.bindings.get(&p).cloned());
                let ownership = binding
                    .as_ref()
                    .map(|b| format!("fd:{}:{}", b.provider_pid, b.file_pid))
                    .unwrap_or_default();
                Observation {
                    identity: t.identity.clone(),
                    binding,
                    ownership,
                }
            })
            .collect();
        let mut selected = Vec::new();
        for (i, target) in targets.iter().enumerate() {
            inspection::check(self.stop, self.deadline)?;
            let Some(pane) = target.pane else { continue };
            if scan.statuses.get(&pane) != Some(&Status::Unavailable) {
                continue;
            }
            let Some(base) = values[i].binding.as_ref() else {
                continue;
            };
            // Missing/corrupt links affect this optional source only.
            let Ok(Some(link)) = conversation_link::load(self.state, &target.identity) else {
                continue;
            };
            if link.notification_owner().is_some() && link.matches(&target.identity, pane, base) {
                selected.push((i, link));
                if selected.len() > 128 {
                    return Err(Error::Unavailable);
                }
            }
        }
        if selected.is_empty() {
            return Ok(values);
        }
        // Snapshot inboxes may be stale. Re-read exact tmux lifetimes only when
        // an opted-in source exists, never one extra command per browser/tab.
        let current = self
            .runtime
            .block_on(self.reader.read_basic_cancelable(
                inspection::commands(),
                self.stop,
                self.deadline.into(),
            ))
            .map_err(|_| Error::Unavailable)?;
        for (i, link) in selected {
            let target = &targets[i];
            let live = current
                .sessions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|s| {
                    s.id == target.identity.id
                        && s.created_at == target.identity.created_at
                        && i32::try_from(s.pane_pid).ok() == target.pane
                });
            if !live {
                continue;
            }
            let base = values[i].binding.as_ref().expect("selected binding");
            let resolved = self
                .inspector
                .process_stamp(base.provider_pid, self.stop, self.deadline, self.runtime)
                .and_then(|stamp| link.resolve(&stamp, self.stop, self.deadline));
            if let Ok(binding) = resolved {
                values[i].binding = Some(Arc::new(binding));
                values[i].ownership = link.notification_owner().expect("selected permission");
            }
        }
        Ok(values)
    }
    pub fn revalidate(
        &self,
        targets: &[Target],
        before: &[Observation],
    ) -> Result<Vec<Observation>, Error> {
        let panes: Vec<_> = targets.iter().filter_map(|t| t.pane).collect();
        let scan = self.inspector.scan(
            &panes,
            ScanPurpose::Completion,
            self.stop,
            self.deadline,
            self.runtime,
        )?;
        let after = self.observations(targets, &scan)?;
        Ok(before
            .iter()
            .filter(|old| after.iter().any(|new| same_source(old, new)))
            .cloned()
            .collect())
    }
}
fn same_source(a: &Observation, b: &Observation) -> bool {
    a.identity == b.identity
        && a.ownership == b.ownership
        && a.binding
            .as_ref()
            .zip(b.binding.as_ref())
            .is_some_and(|(a, b)| a.same_record(b))
}

#[cfg(test)]
#[path = "completion_links_tests.rs"]
mod tests;

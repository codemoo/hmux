//! Two-pass provider authority for recovery. Never select a latest transcript.
use crate::{
    binding::Status,
    inspection::{self, Inspector, ScanPurpose},
    recovery,
};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub(crate) fn resolver(inspector: Arc<Inspector>) -> recovery::Resolver {
    Arc::new(move |panes| {
        let inspector = inspector.clone();
        Box::pin(async move {
            let permit = inspection::admit_background().ok_or(recovery::Error::Busy)?;
            let stop = CancellationToken::new();
            let _guard = stop.clone().drop_guard();
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let deadline = Instant::now() + Duration::from_secs(10);
                // Conversation discovery includes both Codex and Claude, without
                // parsing conversation messages or model metadata.
                let first = inspector
                    .scan(&panes, ScanPurpose::Conversation, &stop, deadline, &runtime)
                    .map_err(|_| recovery::Error::Unavailable)?;
                let second = inspector
                    .scan(&panes, ScanPurpose::Conversation, &stop, deadline, &runtime)
                    .map_err(|_| recovery::Error::Unavailable)?;
                inspection::check(&stop, deadline).map_err(|_| recovery::Error::Unavailable)?;
                Ok(stable(first.bindings, &second.bindings))
            })
            .await
            .map_err(|_| recovery::Error::Unavailable)?
        })
    })
}

fn stable(
    first: BTreeMap<i32, Arc<crate::binding::Binding>>,
    second: &BTreeMap<i32, Arc<crate::binding::Binding>>,
) -> BTreeMap<i32, recovery::ResumeReference> {
    let mut refs = BTreeMap::new();
    for (pane, a) in first {
        let Some(b) = second.get(&pane) else { continue };
        if a.status != Status::Ready || b.status != Status::Ready || !a.same_record(b) {
            continue;
        }
        let Some(config_dir) = a.root.parent().and_then(|p| p.to_str()) else {
            continue;
        };
        refs.insert(
            pane,
            recovery::ResumeReference {
                provider: a.provider.as_str().into(),
                session_id: a.record_id.clone(),
                config_dir: config_dir.into(),
            },
        );
    }
    refs
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{Binding, Provider};
    #[test]
    fn only_stable_authoritative_records_can_be_resumed() {
        let a = Binding {
            provider: Provider::Claude,
            provider_pid: 20,
            file_pid: 20,
            path: "/synthetic/.claude/projects/a.jsonl".into(),
            root: "/synthetic/.claude/projects".into(),
            record_id: "synthetic-record".into(),
            model: String::new(),
            state: String::new(),
            working_since: 0,
            status: Status::Ready,
        };
        let first = BTreeMap::from([(10, Arc::new(a.clone()))]);
        let second = first.clone();
        let refs = stable(first.clone(), &second);
        assert_eq!(refs[&10].config_dir, "/synthetic/.claude");
        for replacement in [
            Binding {
                provider_pid: 21,
                ..a.clone()
            },
            Binding {
                record_id: "new-record".into(),
                ..a.clone()
            },
            Binding {
                status: Status::Ambiguous,
                ..a.clone()
            },
            Binding {
                path: "/synthetic/other".into(),
                ..a.clone()
            },
        ] {
            assert!(stable(
                first.clone(),
                &BTreeMap::from([(10, Arc::new(replacement))])
            )
            .is_empty());
        }
    }
}

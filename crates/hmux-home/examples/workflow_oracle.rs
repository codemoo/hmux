//! Synthetic test bridge only, not a production helper entrypoint.
use hmux_home::workflow::{Binding, HookEvent, Report, Store};
use hmux_model::{Catalog, Session};
use serde::Deserialize;
use std::io::Read;
use tokio_util::sync::CancellationToken;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: std::path::PathBuf,
    now: chrono::DateTime<chrono::Utc>,
    operation: String,
    binding: Binding,
    #[serde(default)]
    event: Option<HookEvent>,
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    render: bool,
}
fn main() {
    if run().is_err() {
        eprintln!("synthetic workflow operation failed");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(256 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 256 * 1024 {
        return Err("input limit".into());
    }
    let q: Request = serde_json::from_slice(&raw)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = Store::new(q.root);
    let cancel = CancellationToken::new();
    let result = runtime.block_on(async {
        match q.operation.as_str() {
            "hook" => {
                store
                    .record_hook(
                        q.binding.clone(),
                        q.event.ok_or(hmux_home::workflow::Error::Invalid)?,
                        q.now,
                        &cancel,
                    )
                    .await
            }
            "report" => {
                store
                    .record_report(
                        q.binding.clone(),
                        Report {
                            task_id: q.task_id,
                            status: q.status,
                        },
                        q.now,
                        &cancel,
                    )
                    .await
            }
            "read" => Ok(()),
            _ => Err(hmux_home::workflow::Error::Invalid),
        }
    });
    result.map_err(|_| "workflow write failed")?;
    let mut catalog = Catalog {
        sessions: Some(vec![Session {
            id: q.binding.id,
            created_at: q.binding.created_at,
            ..Default::default()
        }]),
        ..Default::default()
    };
    store
        .apply(&mut catalog, q.now, &cancel)
        .map_err(|_| "workflow read failed")?;
    if q.render {
        let views =
            hmux_home::workflow::views(catalog.sessions.as_deref().unwrap_or_default(), "")?;
        let mut text = Vec::new();
        hmux_home::workflow::write_views(&views, &mut text)?;
        serde_json::to_writer(
            std::io::stdout(),
            &serde_json::json!({"views": views, "text": String::from_utf8(text)?}),
        )?;
    } else {
        serde_json::to_writer(std::io::stdout(), &catalog.sessions)?;
    }
    Ok(())
}

//! Synthetic workload for the production JSONL reader, not a whole-Home benchmark.
//! Only a newly created private temporary tree is read; no provider data is used.
use chrono::{DateTime, Utc};
use hmux_home::usage_activity::{Diagnostics, Options, Reader, Sample};
use serde_json::{json, Value};
use std::{
    error::Error,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const LINES: usize = 32;
const BURST_FILES: usize = 8;
const BURST_LINES: usize = 64;
const QUIET_POLLS: usize = 10;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().canonicalize()?.join(format!(
            "hmux-e2e-activity-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        DirBuilder::new().mode(0o700).create(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // This owner only exists after successful exclusive directory creation.
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("synthetic activity fixture cleanup failed: {error}");
        }
    }
}

fn path(root: &Path, provider: &str, index: usize) -> PathBuf {
    root.join(provider)
        .join(format!("project-{:04}", index / 16))
        .join(format!("session-{index:04}.jsonl"))
}
fn line(provider: &str, index: usize, tokens: i64, now: DateTime<Utc>) -> Vec<u8> {
    let value = if provider == "claude" {
        json!({"type":"assistant", "timestamp":now.to_rfc3339(),
            "sessionId":format!("synthetic-{index}"), "message":{"model":"synthetic",
            "usage":{"input_tokens":tokens,"output_tokens":0}}})
    } else {
        json!({"type":"event_msg", "timestamp":now.to_rfc3339(),
            "payload":{"type":"token_count","info":{"last_token_usage":{
            "input_tokens":tokens,"cached_input_tokens":0,"output_tokens":0}}}})
    };
    let mut bytes = serde_json::to_vec(&value).expect("synthetic JSON");
    bytes.push(b'\n');
    bytes
}
fn append(path: &Path, data: &[u8], lines: usize, new: bool) -> Result<usize> {
    let mut file = OpenOptions::new()
        .write(true)
        .append(!new)
        .create_new(new)
        .mode(0o600)
        .open(path)?;
    for _ in 0..lines {
        file.write_all(data)?;
    }
    Ok(data.len() * lines)
}
fn diagnostics(d: Diagnostics) -> Value {
    json!({"bytes_read":d.bytes_read, "scanned_files":d.scanned_files,
        "rejected_files":d.rejected_files, "incomplete":d.incomplete,
        "issue":d.issue.map(|issue|format!("{issue:?}"))})
}
fn check(sample: &Sample, files: usize, expected: i64, bytes: [usize; 2]) -> Result<()> {
    for ((snapshot, diagnostic), expected_bytes) in [
        (&sample.claude, sample.claude_diagnostics),
        (&sample.codex, sample.codex_diagnostics),
    ]
    .into_iter()
    .zip(bytes)
    {
        if diagnostic.incomplete
            || diagnostic.issue.is_some()
            || diagnostic.rejected_files != 0
            || diagnostic.scanned_files != files
            || diagnostic.bytes_read != expected_bytes
            || snapshot.today_total_tokens != expected
            || snapshot.today_sessions_count != files.min(1024)
            || snapshot.sessions_capped != (files > 1024)
            || snapshot.total_saturated
        {
            return Err(format!("activity workload mismatch: {snapshot:?}; {diagnostic:?}; expected tokens {expected}, bytes {expected_bytes}").into());
        }
    }
    Ok(())
}
async fn sample(
    reader: &Reader,
    now: DateTime<Utc>,
    stage: &str,
    files: usize,
    expected: i64,
    bytes: [usize; 2],
) -> Result<Value> {
    let started = Instant::now();
    let value = reader
        .sample(now, &CancellationToken::new())
        .await
        .map_err(|error| format!("{stage}: {error:?}"))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    check(&value, files, expected, bytes)?;
    Ok(json!({"stage":stage,"elapsed_ms":elapsed_ms,
        "claude":diagnostics(value.claude_diagnostics),
        "codex":diagnostics(value.codex_diagnostics),
        "tokens_per_provider":expected, "sessions_per_provider":value.claude.today_sessions_count,
        "sessions_capped":value.claude.sessions_capped,
        "window_events_dropped":{"claude":value.claude.window_events_dropped,"codex":value.codex.window_events_dropped}}))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let files = if let [value] = args.as_slice() {
        value.parse::<usize>()?
    } else if args.is_empty() {
        512
    } else {
        return Err("usage: activity_bench [files-per-provider: 8..2048]".into());
    };
    if !(8..=2048).contains(&files) {
        return Err("files-per-provider must be 8..2048".into());
    }
    let fixture = Fixture::new()?;
    let now = Utc::now();
    let mut bootstrap_bytes = [0usize; 2];
    for (slot, provider) in ["claude", "codex"].into_iter().enumerate() {
        for index in 0..files {
            let file = path(&fixture.0, provider, index);
            fs::create_dir_all(file.parent().ok_or("missing fixture parent")?)?;
            bootstrap_bytes[slot] += append(&file, &line(provider, index, 1, now), LINES, true)?;
        }
    }
    let reader = Reader::new(
        Options {
            claude_projects_root: fixture.0.join("claude"),
            codex_sessions_root: fixture.0.join("codex"),
            claude_swap_sessions_root: None,
        },
        now,
    )
    .map_err(|error| format!("construct reader: {error:?}"))?;
    let mut expected = (files * LINES) as i64;
    let mut samples =
        vec![sample(&reader, now, "bootstrap", files, expected, bootstrap_bytes).await?];
    for index in 0..QUIET_POLLS {
        samples.push(
            sample(
                &reader,
                now,
                &format!("quiet-{index}"),
                files,
                expected,
                [0, 0],
            )
            .await?,
        );
    }
    let mut appended_bytes = [0usize; 2];
    for (slot, provider) in ["claude", "codex"].into_iter().enumerate() {
        for index in 0..BURST_FILES {
            appended_bytes[slot] += append(
                &path(&fixture.0, provider, index),
                &line(provider, index, 2, now),
                BURST_LINES,
                false,
            )?;
        }
    }
    expected += (BURST_FILES * BURST_LINES * 2) as i64;
    samples.push(
        sample(
            &reader,
            now,
            "append-burst",
            files,
            expected,
            appended_bytes,
        )
        .await?,
    );
    samples.push(sample(&reader, now, "post-burst-quiet", files, expected, [0, 0]).await?);
    let mut replaced_bytes = [0usize; 2];
    for (slot, provider) in ["claude", "codex"].into_iter().enumerate() {
        let current = path(&fixture.0, provider, 0);
        let replacement = current.with_extension("replacement");
        replaced_bytes[slot] = append(&replacement, &line(provider, 0, 3, now), 1, true)?;
        fs::rename(replacement, current)?;
    }
    expected += 3;
    samples.push(
        sample(
            &reader,
            now,
            "inode-replacement",
            files,
            expected,
            replaced_bytes,
        )
        .await?,
    );
    samples.push(
        sample(
            &reader,
            now,
            "post-replacement-quiet",
            files,
            expected,
            [0, 0],
        )
        .await?,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema":1, "status":"passed", "scenario":"synthetic-reader-incremental",
            "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "files_per_provider":files,"lines_per_file":LINES,"quiet_polls":QUIET_POLLS,
            "burst_files_per_provider":BURST_FILES,"burst_lines_per_file":BURST_LINES,
            "samples":samples,"limits":["production Reader library plus synthetic driver in one process",
            "wall time includes worker admission, filesystem metadata and parsing; fixture creation excluded",
            "immediate repeated polls, not production polling cadence or cold filesystem cache",
            "no gateway, browser, provider calls, whole-Home memory/CPU or Go comparison"]
        }))?
    );
    Ok(())
}

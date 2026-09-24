//! Runtime policy for native process entrypoints, not an application supervisor.
//!
//! The root future must stop admission and join application-owned sockets,
//! persistence and child-process cleanup before returning. Native resolver calls
//! cannot be cancelled. A finite runtime shutdown stops waiting for such calls;
//! they may continue on their threads until the process exits. Do not reuse this
//! helper to create repeated runtimes inside a long-lived library process.
use std::{
    future::Future,
    io,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    time::Duration,
};

const BLOCKING_THREADS: usize = 12;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Run a native process's root future, with finite runtime teardown on normal
/// return and unwinding panic. This does not impose a timeout on the application
/// future or pretend to cancel a blocking OS operation.
pub fn run_process<F: Future>(future: F) -> io::Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(BLOCKING_THREADS)
        .build()?;
    let result = catch_unwind(AssertUnwindSafe(|| runtime.block_on(future)));
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    match result {
        Ok(output) => Ok(output),
        Err(panic) => resume_unwind(panic),
    }
}

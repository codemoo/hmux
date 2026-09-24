//! Bounded login admission. No account decisions or persistence occur here.
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinHandle};

pub const PASSWORD_WORKERS: usize = 2;
pub const LOGIN_SOURCES: usize = 1024;
pub const LOGIN_ATTEMPTS: usize = 5;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

#[derive(Clone)]
pub struct PasswordAdmission(Arc<Semaphore>);
impl Default for PasswordAdmission {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(PASSWORD_WORKERS)))
    }
}
impl PasswordAdmission {
    /// The closure, not the HTTP request future, owns its permit. Dropping or
    /// aborting a running blocking job cannot admit another KDF until it exits.
    /// No unbounded semaphore waiters or per-request password task queue.
    pub fn try_spawn<F, T>(&self, work: F) -> Result<JoinHandle<T>, Busy>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self.0.clone().try_acquire_owned().map_err(|_| Busy)?;
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        }))
    }
    pub fn available(&self) -> usize {
        self.0.available_permits()
    }

    /// Semaphore fairness stops new admissions once this drain is queued. The
    /// permit stays with each actual worker, including detached/aborted callers.
    pub async fn shutdown(&self) {
        let _drained = self.0.acquire_many(PASSWORD_WORKERS as u32).await;
        self.0.close();
    }
}

#[derive(Default)]
pub struct LoginLimiter {
    attempts: HashMap<String, Vec<Instant>>,
}
impl LoginLimiter {
    /// Caller serializes this state and supplies an already trusted/normalized
    /// client address, never arbitrary untrusted forwarding headers.
    pub fn admit(&mut self, source: &str, now: Instant) -> bool {
        let source = if source.len() > 80 { "unknown" } else { source };
        self.attempts.retain(|_, times| {
            times
                .last()
                .is_some_and(|t| now.saturating_duration_since(*t) < LOGIN_WINDOW)
        });
        if let Some(times) = self.attempts.get_mut(source) {
            times.retain(|t| now.saturating_duration_since(*t) < LOGIN_WINDOW);
            if times.len() >= LOGIN_ATTEMPTS {
                return false;
            }
            times.push(now);
        } else {
            if self.attempts.len() >= LOGIN_SOURCES {
                return false;
            }
            let mut times = Vec::with_capacity(LOGIN_ATTEMPTS);
            times.push(now);
            self.attempts.insert(source.to_owned(), times);
        }
        true
    }
    pub fn sources(&self) -> usize {
        self.attempts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropped_and_aborted_requests_hold_running_password_permits() {
        let pool = PasswordAdmission::default();
        let mut release = Vec::new();
        let mut completed = Vec::new();
        for index in 0..PASSWORD_WORKERS {
            let (start, started) = tokio::sync::oneshot::channel();
            let (finish, finished) = tokio::sync::oneshot::channel();
            let (unlock, lock) = std::sync::mpsc::channel();
            let handle = pool
                .try_spawn(move || {
                    let _ = start.send(());
                    lock.recv().unwrap();
                    let _ = finish.send(());
                })
                .unwrap();
            started.await.unwrap();
            if index == 1 {
                handle.abort();
            }
            drop(handle);
            release.push(unlock);
            completed.push(finished);
        }
        assert_eq!(pool.available(), 0);
        assert!(pool.try_spawn(|| ()).is_err());
        for unlock in release {
            unlock.send(()).unwrap();
        }
        for finished in completed {
            finished.await.unwrap();
        }
        // Finish notifications occur just before permit Drop in the closure.
        tokio::time::timeout(Duration::from_secs(2), async {
            while pool.available() != PASSWORD_WORKERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(pool.try_spawn(|| 42).unwrap().await.unwrap(), 42);
    }

    #[test]
    fn source_rotation_is_bounded_and_expiry_recovers_capacity() {
        let mut limiter = LoginLimiter::default();
        let start = Instant::now();
        for _ in 0..LOGIN_ATTEMPTS {
            assert!(limiter.admit("192.0.2.1", start));
        }
        assert!(!limiter.admit("192.0.2.1", start + LOGIN_WINDOW - Duration::from_nanos(1)));
        assert!(limiter.admit("192.0.2.1", start + LOGIN_WINDOW));
        for index in 1..LOGIN_SOURCES {
            assert!(limiter.admit(&format!("synthetic-{index}"), start + LOGIN_WINDOW));
        }
        assert_eq!(limiter.sources(), LOGIN_SOURCES);
        assert!(!limiter.admit("extra", start + LOGIN_WINDOW));
        assert!(limiter.admit("extra", start + LOGIN_WINDOW * 2));
        assert_eq!(limiter.sources(), 1);
    }
}

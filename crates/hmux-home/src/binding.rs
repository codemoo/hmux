//! Private Home provider associations. Neither transcript paths nor provider
//! record IDs are part of a browser-facing catalog or conversation response.
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Codex,
    Claude,
}
impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unavailable,
    Ready,
    Ambiguous,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Binding {
    pub provider: Provider,
    pub provider_pid: i32,
    pub file_pid: i32,
    pub path: PathBuf,
    pub root: PathBuf,
    pub record_id: String,
    pub model: String,
    pub state: String,
    pub working_since: i64,
    pub status: Status,
}
impl Binding {
    pub fn unavailable(provider: Provider, provider_pid: i32) -> Self {
        Self {
            provider,
            provider_pid,
            file_pid: 0,
            path: PathBuf::new(),
            root: PathBuf::new(),
            record_id: String::new(),
            model: String::new(),
            state: String::new(),
            working_since: 0,
            status: Status::Unavailable,
        }
    }
    pub fn same_record(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.provider_pid == other.provider_pid
            && self.file_pid == other.file_pid
            && self.path == other.path
            && self.root == other.root
            && self.record_id == other.record_id
    }
}
impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Binding([redacted])")
    }
}

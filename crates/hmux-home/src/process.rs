//! Bounded, pure process-table inspection. Provider identity must come from
//! `nearest_provider` and an exact record binding, never from `candidate`.

use crate::binding::{Provider, Status};
use std::collections::{BTreeMap, VecDeque};

pub const MAX_RAW_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ROWS: usize = 200_000;
pub const MAX_NODES: usize = 100_000;
pub const MAX_TREE_NODES: usize = 10_000;
pub const MAX_WRAPPER_DEPTH: usize = 16;
const MAX_COMMAND_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    RawTooLarge,
    TooManyRows,
    TooManyNodes,
    NoValidRows,
    DuplicatePid,
    CyclicParent,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid process snapshot: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub pid: i32,
    pub ppid: i32,
    pub state: String,
    pub cpu: f64,
    pub process: String,
    pub provider: Option<Provider>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub nodes: BTreeMap<i32, Node>,
    pub children: BTreeMap<i32, Vec<i32>>,
}

impl Snapshot {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_RAW_BYTES {
            return Err(Error::RawTooLarge);
        }
        let mut nodes = BTreeMap::new();
        let table = raw.strip_suffix(b"\n").unwrap_or(raw);
        for (row_index, line) in table.split(|byte| *byte == b'\n').enumerate() {
            if row_index >= MAX_ROWS {
                return Err(Error::TooManyRows);
            }
            let Some(node) = parse_row(line) else {
                continue;
            };
            if nodes.insert(node.pid, node).is_some() {
                return Err(Error::DuplicatePid);
            }
            if nodes.len() > MAX_NODES {
                return Err(Error::TooManyNodes);
            }
        }
        if nodes.is_empty() && raw.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(Error::NoValidRows);
        }
        validate_acyclic(&nodes)?;
        let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for node in nodes.values() {
            children.entry(node.ppid).or_default().push(node.pid);
        }
        // BTreeMap iteration above is PID ordered, so each child list is sorted.
        Ok(Self { nodes, children })
    }

    /// Finds the first provider on each branch. Nested providers are excluded.
    /// A unique foreground provider wins over background peers.
    pub fn nearest_provider(&self, pane: i32) -> (i32, Status) {
        if !self.nodes.contains_key(&pane) {
            return (0, Status::Unavailable);
        }
        let mut found = Vec::new();
        let mut foreground = Vec::new();
        let result = self.walk(pane, true, |node, _| {
            if node.provider.is_some() {
                found.push(node.pid);
                if node.state.contains('+') {
                    foreground.push(node.pid);
                }
                false
            } else {
                true
            }
        });
        if result.is_err() {
            return (0, Status::Ambiguous);
        }
        if foreground.len() == 1 {
            return (foreground[0], Status::Ready);
        }
        match found.as_slice() {
            [] => (0, Status::Unavailable),
            [pid] => (*pid, Status::Ready),
            _ => (0, Status::Ambiguous),
        }
    }

    /// Returns the unique non-provider descendant chain, at most 16 deep.
    pub fn wrapper_chain(&self, provider_pid: i32) -> (Vec<i32>, Status) {
        if self
            .nodes
            .get(&provider_pid)
            .is_none_or(|node| node.provider.is_none())
        {
            return (Vec::new(), Status::Unavailable);
        }
        let mut current = provider_pid;
        let mut chain = Vec::new();
        for _ in 0..MAX_WRAPPER_DEPTH {
            let descendants = self.children.get(&current).map_or(&[][..], Vec::as_slice);
            let [child_pid] = descendants else {
                return if descendants.is_empty() {
                    (chain, Status::Ready)
                } else {
                    (Vec::new(), Status::Ambiguous)
                };
            };
            let Some(child) = self.nodes.get(child_pid) else {
                return (Vec::new(), Status::Unavailable);
            };
            if child.provider.is_some() {
                return (Vec::new(), Status::Ambiguous);
            }
            chain.push(*child_pid);
            current = *child_pid;
        }
        if self
            .children
            .get(&current)
            .is_some_and(|children| !children.is_empty())
        {
            (Vec::new(), Status::Unavailable)
        } else {
            (chain, Status::Ready)
        }
    }

    /// Display fallback matching Go's depth/rank selection. It is not an
    /// authoritative provider association, even when this node is a provider.
    pub fn candidate(&self, pane: i32) -> Option<&Node> {
        let root = self.nodes.get(&pane)?;
        let mut best = (root, 0usize);
        let mut provider: Option<(&Node, usize)> = None;
        self.walk(pane, false, |node, depth| {
            if node.provider.is_some()
                && provider.is_none_or(|(old, old_depth)| {
                    depth < old_depth || (depth == old_depth && node.pid < old.pid)
                })
            {
                provider = Some((node, depth));
            }
            let rank = generic_rank(node, depth);
            let best_rank = generic_rank(best.0, best.1);
            if rank > best_rank
                || (rank == best_rank && depth > best.1)
                || (rank == best_rank && depth == best.1 && node.cpu > best.0.cpu)
            {
                best = (node, depth);
            }
            true
        })
        .ok()?;
        Some(provider.map_or(best.0, |(node, _)| node))
    }

    /// Visits each reachable node with a queue capped before child insertion.
    fn walk<'a>(
        &'a self,
        pane: i32,
        stop_at_provider: bool,
        mut visit: impl FnMut(&'a Node, usize) -> bool,
    ) -> Result<(), ()> {
        let mut queue = VecDeque::from([(pane, 0usize)]);
        let mut visited = std::collections::BTreeSet::new();
        while let Some((pid, depth)) = queue.pop_front() {
            if !visited.insert(pid) || visited.len() > MAX_TREE_NODES {
                return Err(());
            }
            let Some(node) = self.nodes.get(&pid) else {
                return Err(());
            };
            if !visit(node, depth) || (stop_at_provider && node.provider.is_some()) {
                continue;
            }
            if let Some(children) = self.children.get(&pid) {
                if children.len() > MAX_TREE_NODES.saturating_sub(visited.len() + queue.len()) {
                    return Err(());
                }
                queue.extend(children.iter().map(|child| (*child, depth + 1)));
            }
        }
        Ok(())
    }
}

pub fn infer_agent_state(node: &Node) -> &'static str {
    if node.state.starts_with('R') || node.state.starts_with('D') || node.cpu >= 0.5 {
        "working"
    } else {
        "idle"
    }
}

fn parse_row(raw: &[u8]) -> Option<Node> {
    let line = std::str::from_utf8(raw).ok()?;
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse::<i32>().ok()?;
    let ppid = fields.next()?.parse::<i32>().ok()?;
    let state = fields.next()?;
    let cpu = fields.next()?.parse::<f64>().ok()?;
    if pid < 1 || ppid < 0 || !cpu.is_finite() {
        return None;
    }
    let first = fields.next()?;
    if first.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let mut command = String::with_capacity(first.len().min(MAX_COMMAND_BYTES));
    command.push_str(first);
    for field in fields {
        if command.len().saturating_add(field.len() + 1) > MAX_COMMAND_BYTES {
            return None;
        }
        command.push(' ');
        command.push_str(field);
    }
    if command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let process = process_name(&command);
    if process.is_empty() {
        return None;
    }
    Some(Node {
        pid,
        ppid,
        state: safe_text(state, 16),
        cpu,
        process,
        provider: provider_name(&command),
    })
}

fn validate_acyclic(nodes: &BTreeMap<i32, Node>) -> Result<(), Error> {
    let mut marks = BTreeMap::<i32, u8>::new();
    let mut path = Vec::new();
    for &pid in nodes.keys() {
        if marks.contains_key(&pid) {
            continue;
        }
        path.clear();
        let mut current = pid;
        while nodes.contains_key(&current) {
            match marks.get(&current) {
                Some(1) => return Err(Error::CyclicParent),
                Some(2) => break,
                _ => {}
            }
            marks.insert(current, 1);
            path.push(current);
            current = nodes[&current].ppid;
        }
        for visited in path.drain(..) {
            marks.insert(visited, 2);
        }
    }
    Ok(())
}

fn process_name(command: &str) -> String {
    let base = command.trim().rsplit('/').next().unwrap_or("");
    safe_text(
        base.trim_start_matches('-')
            .split_whitespace()
            .next()
            .unwrap_or(""),
        128,
    )
}

fn provider_name(command: &str) -> Option<Provider> {
    let clean = command.trim();
    if clean.contains("/claude/versions/") {
        let base = clean.rsplit('/').next().unwrap_or("");
        if !base.is_empty()
            && base.len() <= 128
            && base.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'+' | b'-'))
            })
        {
            return Some(Provider::Claude);
        }
    }
    match process_name(clean).to_ascii_lowercase().as_str() {
        "codex" => Some(Provider::Codex),
        "claude" => Some(Provider::Claude),
        _ => None,
    }
}

fn safe_text(value: &str, max: usize) -> String {
    let mut result = String::new();
    for mut ch in value.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            ch = ' ';
        }
        if result.len() + ch.len_utf8() > max {
            break;
        }
        result.push(ch);
    }
    result.trim().to_owned()
}

fn generic_rank(node: &Node, depth: usize) -> i32 {
    let name = node.process.trim_start_matches('-');
    let shell = ["bash", "dash", "fish", "login", "sh", "tcsh", "zsh"]
        .iter()
        .any(|shell| name.eq_ignore_ascii_case(shell));
    i32::from(!shell) * 100
        + i32::from(node.state.contains('+')) * 30
        + i32::from(node.cpu >= 0.5) * 20
        + i32::from(depth > 0) * 10
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

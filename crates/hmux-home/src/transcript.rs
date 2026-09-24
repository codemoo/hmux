//! Pure, bounded decoding of the public portion of a bound provider transcript.
//! The owner opens and authenticates the file; this module never touches storage.
use crate::binding::Provider;
use hmux_model::ConversationMessage;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::fmt;
use tokio_util::sync::CancellationToken;

pub const TAIL_LIMIT: usize = 4 * 1024 * 1024;
const LINE_LIMIT: usize = 1024 * 1024;
const MESSAGE_LIMIT: usize = 256 * 1024;
const TEXT_LIMIT: usize = 512 * 1024;
const MESSAGE_MAX: usize = 200;
const CONTINUATION_PREFIX: &str = "Another language model started to solve this problem and produced a summary of its thinking process.";
const CONTINUATION_SUMMARY: &str = "Here is the summary produced by the other language model";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidTail,
    Cancelled,
}
#[derive(Debug, Default)]
pub struct Parsed {
    pub messages: Vec<ConversationMessage>,
    pub truncated: bool,
}

/// `data` is the exact, at-most-four-MiB file tail, and `base_offset` is its
/// absolute start. The caller must recheck its authoritative file binding.
pub fn parse_tail(
    data: &[u8],
    base_offset: u64,
    file_identity: [u8; 16],
    provider: Provider,
    stop: &CancellationToken,
) -> Result<Parsed, Error> {
    if stop.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if data.len() > TAIL_LIMIT {
        return Err(Error::InvalidTail);
    }
    let mut parsed = Parsed {
        messages: Vec::new(),
        truncated: base_offset > 0,
    };
    let mut start = 0;
    if base_offset > 0 {
        let Some(end) = data.iter().position(|byte| *byte == b'\n') else {
            return Ok(parsed);
        };
        start = end + 1;
    }
    let complete_end = if data.last() == Some(&b'\n') {
        data.len()
    } else {
        parsed.truncated = !data.is_empty() || parsed.truncated;
        data.iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(start, |end| end + 1)
    };
    if start >= complete_end {
        return Ok(parsed);
    }
    let complete = &data[start..complete_end];
    let handoffs = compacted_handoffs(complete, stop)?;
    let mut messages = VecDeque::<ConversationMessage>::with_capacity(MESSAGE_MAX);
    let mut text_bytes = 0;
    let mut position = start;
    while position < complete_end {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let end = position
            + data[position..complete_end]
                .iter()
                .position(|byte| *byte == b'\n')
                .expect("complete line");
        let line = &data[position..end];
        if line.len() > LINE_LIMIT {
            parsed.truncated = true;
        } else {
            let offset = base_offset
                .checked_add(position as u64)
                .ok_or(Error::InvalidTail)?;
            match parse_line(line, &file_identity, offset, provider) {
                Line::Message(message) => {
                    let hash: [u8; 32] = Sha256::digest(message.text.trim()).into();
                    let compacted = message.role == "assistant" && handoffs.contains(&hash);
                    if !compacted {
                        while messages.len() == MESSAGE_MAX
                            || text_bytes + message.text.len() > TEXT_LIMIT
                        {
                            text_bytes -= messages
                                .pop_front()
                                .expect("bounded message fits")
                                .text
                                .len();
                            parsed.truncated = true;
                        }
                        text_bytes += message.text.len();
                        messages.push_back(message);
                    }
                }
                Line::Omitted => parsed.truncated = true,
                Line::Ignored => {}
            }
        }
        position = end + 1;
    }
    parsed.messages = messages.into_iter().collect();
    Ok(parsed)
}

enum Line {
    Ignored,
    Omitted,
    Message(ConversationMessage),
}

#[derive(Deserialize)]
struct CodexEvent<'a> {
    #[serde(rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<CodexPayload<'a>>,
}
#[derive(Deserialize)]
struct CodexPayload<'a> {
    #[serde(rename = "type")]
    kind: Option<Cow<'a, str>>,
    role: Option<Cow<'a, str>>,
    recipient: Option<Cow<'a, str>>,
    channel: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a serde_json::value::RawValue>,
}
#[derive(Deserialize)]
struct ClaudeEvent<'a> {
    #[serde(rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    meta: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    summary: Option<bool>,
    #[serde(borrow)]
    message: Option<ClaudeMessage<'a>>,
}
#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a serde_json::value::RawValue>,
}
#[derive(Deserialize)]
struct Part<'a> {
    #[serde(rename = "type")]
    kind: Option<Cow<'a, str>>,
    text: Option<Cow<'a, str>>,
    recipient: Option<Cow<'a, str>>,
    channel: Option<Cow<'a, str>>,
}

struct Parts<'a> {
    role: &'a str,
    provider: Provider,
    text: String,
    canonical: Sha256,
    canonical_count: usize,
    count: usize,
    overflow: bool,
}
impl<'a> Parts<'a> {
    fn new(role: &'a str, provider: Provider, identity: &[u8; 16], offset: u64) -> Self {
        let mut canonical = Sha256::new();
        canonical.update(identity);
        canonical.update(offset.to_be_bytes());
        canonical.update(b"{\"payload\":{\"content\":[");
        Self {
            role,
            provider,
            text: String::new(),
            canonical,
            canonical_count: 0,
            count: 0,
            overflow: false,
        }
    }
    fn accept(&mut self, part: Part<'_>) {
        let wanted = if self.provider == Provider::Codex {
            if self.role == "user" {
                "input_text"
            } else {
                "output_text"
            }
        } else {
            "text"
        };
        if part.kind.as_deref() != Some(wanted)
            || (self.provider == Provider::Codex
                && (!public_recipient(part.recipient.as_deref())
                    || (self.role == "assistant" && !public_channel(part.channel.as_deref()))))
        {
            return;
        }
        let Some(text) = part.text.filter(|text| !text.is_empty()) else {
            return;
        };
        let text = text.as_ref();
        if self.provider == Provider::Claude {
            if text.trim().is_empty() || (self.role == "user" && claude_injected_text(text)) {
                return;
            }
            // Go hashes Claude's canonical conversion before the common injected
            // user-text filter. Stream that conversion instead of retaining it.
            if self.canonical_count != 0 {
                self.canonical.update(b",");
            }
            self.canonical.update(b"{\"text\":");
            go_json_string(&mut self.canonical, text);
            self.canonical.update(if self.role == "user" {
                b",\"type\":\"input_text\"}".as_slice()
            } else {
                b",\"type\":\"output_text\"}".as_slice()
            });
            self.canonical_count += 1;
        }
        if self.role == "user" && rejected_injected_user_text(text) {
            return;
        }
        let next = self
            .text
            .len()
            .saturating_add(text.len())
            .saturating_add(usize::from(self.count != 0));
        if next > MESSAGE_LIMIT {
            self.overflow = true;
            return;
        }
        if self.count != 0 {
            self.text.push('\n');
        }
        self.text.push_str(text);
        self.count += 1;
    }
}
struct PartsSeed<'a, 'b>(&'a mut Parts<'b>);
impl<'de, 'a, 'b> DeserializeSeed<'de> for PartsSeed<'a, 'b> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        struct PartsVisitor<'a, 'b>(&'a mut Parts<'b>);
        impl<'de, 'a, 'b> Visitor<'de> for PartsVisitor<'a, 'b> {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("content array")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
                while let Some(part) = seq.next_element::<Option<Part<'de>>>()? {
                    if let Some(part) = part {
                        self.0.accept(part);
                    }
                }
                Ok(())
            }
        }
        deserializer.deserialize_seq(PartsVisitor(self.0))
    }
}
fn collect_array(raw: &serde_json::value::RawValue, parts: &mut Parts<'_>) -> bool {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    PartsSeed(parts).deserialize(&mut deserializer).is_ok() && deserializer.end().is_ok()
}

fn parse_line(line: &[u8], identity: &[u8; 16], offset: u64, provider: Provider) -> Line {
    match provider {
        Provider::Codex => parse_codex(line, identity, offset),
        Provider::Claude => parse_claude(line, identity, offset),
    }
}
fn parse_codex(line: &[u8], identity: &[u8; 16], offset: u64) -> Line {
    let normalized = String::from_utf8_lossy(line);
    let Ok(event) = serde_json::from_str::<CodexEvent<'_>>(&normalized) else {
        return Line::Ignored;
    };
    if event.kind.as_deref() != Some("response_item") {
        return Line::Ignored;
    }
    let Some(payload) = event.payload else {
        return Line::Ignored;
    };
    if payload.kind.as_deref() != Some("message") || !public_recipient(payload.recipient.as_deref())
    {
        return Line::Ignored;
    }
    let Some(role @ ("user" | "assistant")) = payload.role.as_deref() else {
        return Line::Ignored;
    };
    if role == "assistant" && !public_channel(payload.channel.as_deref()) {
        return Line::Ignored;
    }
    let Some(content) = payload.content else {
        return Line::Ignored;
    };
    let mut parts = Parts::new(role, Provider::Codex, identity, offset);
    if !collect_array(content, &mut parts) {
        return Line::Ignored;
    }
    let mut hash = Sha256::new();
    hash.update(identity);
    hash.update(offset.to_be_bytes());
    hash.update(line);
    finish_parts(parts, hash)
}
fn parse_claude(line: &[u8], identity: &[u8; 16], offset: u64) -> Line {
    let normalized = String::from_utf8_lossy(line);
    let Ok(event) = serde_json::from_str::<ClaudeEvent<'_>>(&normalized) else {
        return Line::Ignored;
    };
    if event.sidechain == Some(true) || event.meta == Some(true) || event.summary == Some(true) {
        return Line::Ignored;
    }
    let Some(role @ ("user" | "assistant")) = event.kind.as_deref() else {
        return Line::Ignored;
    };
    let Some(message) = event.message else {
        return Line::Ignored;
    };
    if message.role.as_deref() != Some(role) {
        return Line::Ignored;
    }
    let Some(content) = message.content else {
        return Line::Ignored;
    };
    let mut parts = Parts::new(role, Provider::Claude, identity, offset);
    if let Ok(text) = serde_json::from_str::<Option<Cow<'_, str>>>(content.get()) {
        if let Some(text) = text {
            parts.accept(Part {
                kind: Some(Cow::Borrowed("text")),
                text: Some(text),
                recipient: None,
                channel: None,
            });
        }
    } else if !collect_array(content, &mut parts) {
        return Line::Ignored;
    }
    if parts.count == 0 && !parts.overflow {
        return Line::Ignored;
    }
    parts.canonical.update(if role == "user" {
        b"],\"role\":\"user\",\"type\":\"message\"},\"type\":\"response_item\"}".as_slice()
    } else {
        b"],\"role\":\"assistant\",\"type\":\"message\"},\"type\":\"response_item\"}".as_slice()
    });
    let hash = parts.canonical.clone();
    finish_parts(parts, hash)
}
fn finish_parts(parts: Parts<'_>, hash: Sha256) -> Line {
    if parts.count == 0 && !parts.overflow {
        return Line::Ignored;
    }
    if parts.overflow {
        return Line::Omitted;
    }
    if rejected_handoff(&parts.text, parts.role) {
        return Line::Ignored;
    }
    let digest = hash.finalize();
    let mut id = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(id, "{byte:02x}");
    }
    Line::Message(ConversationMessage {
        id,
        role: parts.role.to_owned(),
        text: parts.text,
    })
}
fn public_recipient(value: Option<&str>) -> bool {
    matches!(value, None | Some("" | "all"))
}
fn public_channel(value: Option<&str>) -> bool {
    matches!(value, None | Some("" | "commentary" | "final"))
}
fn rejected_handoff(text: &str, role: &str) -> bool {
    let text = text.trim();
    if text.starts_with(CONTINUATION_PREFIX) && text.contains(CONTINUATION_SUMMARY) {
        return true;
    }
    if role != "assistant" {
        return false;
    }
    text.split_once('\n').is_some_and(|(heading, rest)| {
        heading.trim() == "## Task and constraints" && rest.trim().starts_with("Workspace:")
    })
}
fn rejected_injected_user_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "# agents.md instructions",
        "<instructions",
        "</instructions>",
        "<environment_context",
        "</environment_context>",
        "<codex_internal_context",
        "</codex_internal_context>",
        "<recommended_plugins>",
        "</recommended_plugins>",
        "<available_deferred_tools>",
        "<tool_call",
        "<tool_result",
        "<function_call",
        "<function_result",
        "<user_shell_command",
        "</user_shell_command>",
        "<assistant recipient=",
        "<developer",
        "<system",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}
fn claude_injected_text(text: &str) -> bool {
    let text = text.trim();
    [
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<command-name>",
        "<system-reminder>",
        "<task-notification>",
        "<teammate-message>",
        "This session is being continued from a previous conversation that ran out of context.",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}
// Hash long ordinary spans together; only JSON escapes need separate updates.
fn go_json_string(out: &mut Sha256, value: &str) {
    out.update(b"\"");
    let mut start = 0;
    for (index, character) in value.char_indices() {
        let mut control = *b"\\u0000";
        let escaped = match character {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            '\u{0008}' => "\\b",
            '\u{000c}' => "\\f",
            '<' => "\\u003c",
            '>' => "\\u003e",
            '&' => "\\u0026",
            '\u{2028}' => "\\u2028",
            '\u{2029}' => "\\u2029",
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                control[4] = HEX[(c as usize) >> 4];
                control[5] = HEX[(c as usize) & 15];
                std::str::from_utf8(&control).expect("ASCII escape")
            }
            _ => continue,
        };
        out.update(&value.as_bytes()[start..index]);
        out.update(escaped);
        start = index + character.len_utf8();
    }
    out.update(&value.as_bytes()[start..]);
    out.update(b"\"");
}

#[derive(Deserialize)]
struct Compaction<'a> {
    #[serde(rename = "type")]
    kind: Option<Cow<'a, str>>,
    payload: Option<CompactionPayload<'a>>,
}
#[derive(Deserialize)]
struct CompactionPayload<'a> {
    message: Option<Cow<'a, str>>,
}
fn compacted_handoffs(data: &[u8], stop: &CancellationToken) -> Result<HashSet<[u8; 32]>, Error> {
    let mut hashes = HashSet::new();
    for line in data.split(|byte| *byte == b'\n') {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !line
            .windows(b"\"compacted\"".len())
            .any(|window| window == b"\"compacted\"")
        {
            continue;
        }
        let normalized = String::from_utf8_lossy(line);
        // A complete possible compaction record that cannot be decoded must
        // not expose an assistant handoff whose confirmation we could not read.
        let record =
            serde_json::from_str::<Compaction<'_>>(&normalized).map_err(|_| Error::InvalidTail)?;
        if record.kind.as_deref() != Some("compacted") {
            continue;
        }
        let Some(text) = record.payload.and_then(|payload| payload.message) else {
            continue;
        };
        let text = text.trim();
        if !text.starts_with(CONTINUATION_PREFIX) {
            continue;
        }
        let Some((_, rest)) = text.split_once(CONTINUATION_SUMMARY) else {
            continue;
        };
        let Some((_, summary)) = rest.split_once(':') else {
            continue;
        };
        let summary = summary.trim();
        if !summary.is_empty() {
            hashes.insert(Sha256::digest(summary).into());
        }
    }
    Ok(hashes)
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;

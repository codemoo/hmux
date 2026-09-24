//! Gateway/Home JSON envelope. Payload ownership stays with its operation.
use base64::{engine::general_purpose, Engine};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::{
    fmt,
    io::{self, Write},
};

pub const MAX_MESSAGE: usize = 4 << 20;
pub const MAX_TERMINALS: usize = 8;
pub const MAX_DATA: usize = 32 << 10;
pub const MAX_UPLOAD_CHUNK: usize = 256 << 10;
pub const HOME_OFFLINE: u16 = 4001;
pub const OUTPUT_FULL: u16 = 4002;
pub const VIEW_EXITED: u16 = 4003;

fn null_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
fn zero<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}
fn raw_payload<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    // Go RawMessage distinguishes an omitted field from explicit JSON null.
    Box::<RawValue>::deserialize(d).map(Some)
}

fn parse_object<T: DeserializeOwned, E: serde::de::Error>(raw: &RawValue) -> Result<T, E> {
    if !raw.get().trim_start().starts_with('{') {
        return Err(E::custom("object required"));
    }
    serde_json::from_str(raw.get()).map_err(|_| E::custom("invalid object"))
}
fn optional_object<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<Box<RawValue>>::deserialize(d)?
        .map(|raw| parse_object(&raw))
        .transpose()
}
fn object_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(optional_object(d)?.unwrap_or_default())
}
fn file_objects<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<FileHeader>>, D::Error> {
    let Some(raw) = Option::<Vec<Option<Box<RawValue>>>>::deserialize(d)? else {
        return Ok(None);
    };
    raw.into_iter()
        .map(|value| match value {
            Some(raw) => parse_object(&raw),
            None => Ok(FileHeader::default()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionIdentity {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub created_at: i64,
}
impl SessionIdentity {
    pub fn is_valid(&self) -> bool {
        let Some(digits) = self.id.strip_prefix('$') else {
            return false;
        };
        self.created_at > 0
            && (1..=12).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileHeader {
    #[serde(deserialize_with = "null_default")]
    pub index: i64,
    #[serde(deserialize_with = "null_default")]
    pub size: i64,
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub extension: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UploadHeader {
    #[serde(deserialize_with = "null_default")]
    pub protocol_version: i64,
    #[serde(deserialize_with = "null_default")]
    pub request_id: String,
    #[serde(deserialize_with = "object_default")]
    pub session: SessionIdentity,
    #[serde(deserialize_with = "null_default")]
    pub file_count: i64,
    #[serde(deserialize_with = "null_default")]
    pub total_bytes: i64,
    #[serde(deserialize_with = "file_objects")]
    pub files: Option<Vec<FileHeader>>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Message {
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub id: String,
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub operation: String,
    // Go omitempty does not omit a non-pointer struct, even when it is zero.
    #[serde(deserialize_with = "object_default")]
    pub session: SessionIdentity,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "raw_payload"
    )]
    pub payload: Option<Box<RawValue>>,
    #[serde(skip_serializing_if = "Vec::is_empty", with = "data_bytes")]
    pub data: Vec<u8>,
    #[serde(skip_serializing_if = "zero", deserialize_with = "null_default")]
    pub cols: u16,
    #[serde(skip_serializing_if = "zero", deserialize_with = "null_default")]
    pub rows: u16,
    #[serde(
        skip_serializing_if = "String::is_empty",
        deserialize_with = "null_default"
    )]
    pub error: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_object"
    )]
    pub header: Option<UploadHeader>,
    #[serde(skip_serializing_if = "zero", deserialize_with = "null_default")]
    pub received: i64,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_default"
    )]
    pub capabilities: Vec<String>,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Message")
            .field("redacted", &true)
            .field("data_bytes", &self.data.len())
            .field(
                "payload_bytes",
                &self.payload.as_ref().map(|p| p.get().len()),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    MessageLimit,
    InvalidJson,
    FieldLimit,
}
impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MessageLimit => "web frame exceeds 4 MiB",
            Self::InvalidJson => "invalid web frame",
            Self::FieldLimit => "frame field exceeds limit",
        })
    }
}
impl std::error::Error for CodecError {}

// serde_json may split a string or UTF-8 code point across writes. Keep only
// the final frame and the two bytes needed to recognize U+2028/U+2029.
struct BoundedJson {
    bytes: Vec<u8>,
    full: bool,
    quoted: bool,
    escaped: bool,
    unicode_prefix: u8,
}
impl BoundedJson {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            full: false,
            quoted: false,
            escaped: false,
            unicode_prefix: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_MESSAGE - self.bytes.len() {
            self.full = true;
            return Err(io::Error::other("message limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn emit(&mut self, byte: u8) -> io::Result<()> {
        match self.unicode_prefix {
            1 if byte == 0x80 => {
                self.unicode_prefix = 2;
                return Ok(());
            }
            1 => self.append(&[0xe2])?,
            2 if byte == 0xa8 => {
                self.unicode_prefix = 0;
                return self.append(br"\u2028");
            }
            2 if byte == 0xa9 => {
                self.unicode_prefix = 0;
                return self.append(br"\u2029");
            }
            2 => self.append(&[0xe2, 0x80])?,
            _ => {}
        }
        self.unicode_prefix = 0;
        match byte {
            0xe2 => self.unicode_prefix = 1,
            b'&' => self.append(br"\u0026")?,
            b'<' => self.append(br"\u003c")?,
            b'>' => self.append(br"\u003e")?,
            _ => self.append(&[byte])?,
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<Vec<u8>> {
        match self.unicode_prefix {
            1 => self.append(&[0xe2])?,
            2 => self.append(&[0xe2, 0x80])?,
            _ => {}
        }
        Ok(self.bytes)
    }
}
impl Write for BoundedJson {
    fn write(&mut self, raw: &[u8]) -> io::Result<usize> {
        // Compact insignificant whitespace without parsing opaque payloads:
        // large integers and their original key order must survive unchanged.
        for &byte in raw {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.quoted = false;
                }
            } else if byte == b'"' {
                self.quoted = true;
            } else if matches!(byte, b' ' | b'\n' | b'\r' | b'\t') {
                continue;
            }
            self.emit(byte)?;
        }
        Ok(raw.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Message {
    /// Enforces wire field limits. Operation-specific authorization and geometry
    /// validation remain with their owners; this codec never authorizes an action.
    pub fn validate_fields(&self) -> Result<(), CodecError> {
        let limit = if self.kind == "upload-data" {
            MAX_UPLOAD_CHUNK
        } else {
            MAX_DATA
        };
        if self.data.len() > limit
            || self.id.len() > 64
            || self.error.len() > 128
            || self.capabilities.len() > 16
            || self
                .capabilities
                .iter()
                .any(|c| c.is_empty() || c.len() > 64)
        {
            return Err(CodecError::FieldLimit);
        }
        Ok(())
    }
    pub fn decode(raw: &[u8]) -> Result<Self, CodecError> {
        if raw.len() > MAX_MESSAGE {
            return Err(CodecError::MessageLimit);
        }
        if raw
            .iter()
            .find(|b| !matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
            != Some(&b'{')
        {
            return Err(CodecError::InvalidJson);
        }
        let message: Self = serde_json::from_slice(raw).map_err(|_| CodecError::InvalidJson)?;
        message.validate_fields()?;
        Ok(message)
    }
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        self.validate_fields()?;
        // Preflight the largest opaque field before allocating a serialized copy.
        if self
            .payload
            .as_ref()
            .is_some_and(|p| p.get().len() > MAX_MESSAGE)
        {
            return Err(CodecError::MessageLimit);
        }
        let mut writer = BoundedJson::new();
        if serde_json::to_writer(&mut writer, self).is_err() {
            return Err(if writer.full {
                CodecError::MessageLimit
            } else {
                CodecError::InvalidJson
            });
        }
        writer.finish().map_err(|_| CodecError::MessageLimit)
    }
}

mod data_bytes {
    use super::*;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Text(String),
        Array(Vec<Option<u8>>),
    }
    pub fn serialize<S: Serializer>(value: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&general_purpose::STANDARD.encode(value))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        match Option::<Input>::deserialize(d)? {
            None => Ok(Vec::new()),
            Some(Input::Array(value)) => {
                Ok(value.into_iter().map(Option::unwrap_or_default).collect())
            }
            Some(Input::Text(value)) => {
                // Go StdEncoding permits CR/LF and nonzero unused trailing bits.
                let compact: String = value.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                let engine = general_purpose::GeneralPurpose::new(
                    &base64::alphabet::STANDARD,
                    general_purpose::GeneralPurposeConfig::new()
                        .with_decode_allow_trailing_bits(true),
                );
                engine
                    .decode(compact)
                    .map_err(|_| serde::de::Error::custom("invalid data encoding"))
            }
        }
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;

    #[test]
    fn compaction_and_html_escaping_survive_every_chunk_boundary() {
        let raw = format!(
            r#" {{ "z" : "a <&> {} {} \" q" , "n" : 9007199254740993 }} "#,
            '\u{2028}', '\u{2029}'
        );
        let expected = br#"{"z":"a \u003c\u0026\u003e \u2028 \u2029 \" q","n":9007199254740993}"#;
        for chunk_size in 1..=raw.len() {
            let mut writer = BoundedJson::new();
            for chunk in raw.as_bytes().chunks(chunk_size) {
                writer.write_all(chunk).unwrap();
            }
            assert_eq!(
                writer.finish().unwrap(),
                expected,
                "chunk size {chunk_size}"
            );
        }
    }
}

//! Bounded browser upload metadata and Home completion validation.
//! The Hub owns request IDs and transport; this module never touches the spool.

use hmux_protocol::protobuf::types as p;
use serde::{
    de::{value::MapAccessDeserializer, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;

const MAX_START: usize = 16 * 1024;
#[cfg(test)]
const MAX_RESPONSE: usize = 64 * 1024;
const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: i64 = 32 << 20;
const MAX_TOTAL_BYTES: i64 = 128 << 20;
const MAX_CHUNK: usize = 256 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContractError {
    Size,
    Invalid,
    Incomplete,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid upload contract")
    }
}
impl std::error::Error for ContractError {}

// A typed visitor rejects a seventeenth element before deserializing it into
// an allocated file or building an unbounded serde_json::Value tree.
#[derive(Default)]
struct Files<T>(Vec<T>);

// Derived Serde structs also accept positional JSON arrays. Wrapping each DTO
// forces an object while retaining derived duplicate/unknown-field rejection.
struct Object<T>(T);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Object)
            }
        }
        deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Files<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FilesVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for FilesVisitor<T> {
            type Value = Files<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("at most 16 upload files")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut files = Vec::with_capacity(MAX_FILES);
                while files.len() < MAX_FILES {
                    match seq.next_element()? {
                        Some(file) => files.push(file),
                        None => return Ok(Files(files)),
                    }
                }
                if seq.next_element::<IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom("too many upload files"));
                }
                Ok(Files(files))
            }
        }
        deserializer.deserialize_seq(FilesVisitor(std::marker::PhantomData))
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSession {
    id: Option<String>,
    created_at: Option<i64>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFile {
    size: Option<i64>,
    extension: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawStart {
    #[serde(rename = "type")]
    kind: Option<String>,
    csrf: Option<String>,
    session: Option<Object<RawSession>>,
    files: Option<Files<Object<RawFile>>>,
}

fn parse_bounded<'a, T: Deserialize<'a>>(raw: &'a [u8], limit: usize) -> Result<T, ContractError> {
    if raw.is_empty() || raw.len() > limit {
        return Err(ContractError::Size);
    }
    let mut decoder = serde_json::Deserializer::from_slice(raw);
    let value = T::deserialize(&mut decoder).map_err(|_| ContractError::Invalid)?;
    decoder.end().map_err(|_| ContractError::Invalid)?;
    Ok(value)
}

fn valid_session(session: &p::Session) -> bool {
    session.created_at > 0
        && session.id.strip_prefix('$').is_some_and(|digits| {
            (1..=31).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        })
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_header(header: &p::UploadHeader, require_request_id: bool) -> bool {
    header.protocol_version == 1
        && (if require_request_id {
            valid_id(&header.request_id)
        } else {
            header.request_id.is_empty() || valid_id(&header.request_id)
        })
        && header.session.as_ref().is_some_and(valid_session)
        && (1..=MAX_FILES as u32).contains(&header.file_count)
        && header.file_count as usize == header.files.len()
        && (1..=MAX_TOTAL_BYTES).contains(&header.total_bytes)
        && header.files.iter().enumerate().all(|(index, file)| {
            file.index as usize == index
                && (1..=MAX_FILE_BYTES).contains(&file.size)
                && file.extension.len() <= 16
                && file
                    .extension
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        && header
            .files
            .iter()
            .try_fold(0i64, |sum, file| sum.checked_add(file.size))
            == Some(header.total_bytes)
}

pub(crate) fn decode_start(
    raw: &[u8],
    expected_csrf: &str,
) -> Result<p::UploadHeader, ContractError> {
    let Object(start): Object<RawStart> = parse_bounded(raw, MAX_START)?;
    if expected_csrf.is_empty()
        || start.kind.as_deref() != Some("start")
        || !bool::from(
            start
                .csrf
                .as_deref()
                .unwrap_or("")
                .as_bytes()
                .ct_eq(expected_csrf.as_bytes()),
        )
    {
        return Err(ContractError::Invalid);
    }
    let Object(session) = start.session.ok_or(ContractError::Invalid)?;
    // Browser admission is narrower than the Home wire header: model.ValidateSessionID.
    if !session.id.as_deref().is_some_and(|id| {
        id.strip_prefix('$').is_some_and(|digits| {
            (1..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        })
    }) {
        return Err(ContractError::Invalid);
    }
    let files = start.files.ok_or(ContractError::Invalid)?.0;
    let files: Vec<_> = files
        .into_iter()
        .enumerate()
        .map(|(index, Object(file))| p::FileHeader {
            index: index as u32,
            size: file.size.unwrap_or_default(),
            extension: file.extension.unwrap_or_default(),
        })
        .collect();
    let header = p::UploadHeader {
        protocol_version: 1,
        request_id: String::new(),
        session: Some(p::Session {
            id: session.id.unwrap_or_default(),
            created_at: session.created_at.unwrap_or_default(),
        }),
        file_count: files.len() as u32,
        total_bytes: files
            .iter()
            .try_fold(0i64, |sum, file| sum.checked_add(file.size))
            .ok_or(ContractError::Invalid)?,
        files,
    };
    if !valid_header(&header, false) {
        return Err(ContractError::Invalid);
    }
    Ok(header)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFinish {
    #[serde(rename = "type")]
    kind: Option<String>,
}

pub(crate) fn decode_finish(raw: &[u8]) -> Result<(), ContractError> {
    let Object(finish): Object<RawFinish> = parse_bounded(raw, MAX_START)?;
    if finish.kind.as_deref() != Some("finish") {
        return Err(ContractError::Invalid);
    }
    Ok(())
}

pub(crate) struct UploadHashes {
    hashers: Vec<Sha256>,
    sizes: Vec<i64>,
    file_index: usize,
    remaining: i64,
    received: i64,
    total: i64,
}

impl UploadHashes {
    pub(crate) fn new(header: &p::UploadHeader) -> Result<Self, ContractError> {
        if !valid_header(header, false) {
            return Err(ContractError::Invalid);
        }
        let sizes: Vec<_> = header.files.iter().map(|file| file.size).collect();
        Ok(Self {
            hashers: (0..sizes.len()).map(|_| Sha256::new()).collect(),
            remaining: sizes[0],
            sizes,
            file_index: 0,
            received: 0,
            total: header.total_bytes,
        })
    }

    /// Returns the cumulative byte count to compare with Home's ACK.
    pub(crate) fn consume(&mut self, mut data: &[u8]) -> Result<i64, ContractError> {
        if data.is_empty()
            || data.len() > MAX_CHUNK
            || data.len() as i64 > self.total - self.received
        {
            return Err(ContractError::Size);
        }
        self.received += data.len() as i64;
        while !data.is_empty() {
            let take = data.len().min(self.remaining as usize);
            self.hashers[self.file_index].update(&data[..take]);
            self.remaining -= take as i64;
            data = &data[take..];
            if self.remaining == 0 && self.file_index + 1 < self.sizes.len() {
                self.file_index += 1;
                self.remaining = self.sizes[self.file_index];
            }
        }
        Ok(self.received)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.received == self.total
    }

    pub(crate) fn finish(self) -> Result<Vec<String>, ContractError> {
        if !self.is_complete() {
            return Err(ContractError::Incomplete);
        }
        Ok(self
            .hashers
            .into_iter()
            .map(|hasher| format!("{:x}", hasher.finalize()))
            .collect())
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StageSession {
    pub id: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StageFile {
    pub index: usize,
    pub path: String,
    pub size: i64,
    pub sha256: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StageResponse {
    pub protocol_version: i64,
    pub request_id: String,
    pub stage_id: String,
    pub session: StageSession,
    pub expires_at_unix: i64,
    pub files: Vec<StageFile>,
}

#[cfg(test)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawStageFile {
    index: Option<i64>,
    path: Option<String>,
    size: Option<i64>,
    sha256: Option<String>,
}

#[cfg(test)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawResponse {
    protocol_version: Option<i64>,
    request_id: Option<String>,
    stage_id: Option<String>,
    session: Option<Object<RawSession>>,
    expires_at_unix: Option<i64>,
    files: Option<Files<Object<RawStageFile>>>,
}

fn valid_path(path: &str, stage_dir: &str, index: usize, extension: &str) -> bool {
    if path.is_empty()
        || path.len() > 4096
        || !path.starts_with('/')
        || path
            .bytes()
            .any(|b| matches!(b, 0 | b'\r' | b'\n' | b'\t' | 0x1b))
    {
        return false;
    }
    for part in path[1..].split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return false;
        }
    }
    let mut suffix = path.rsplit('/');
    let file = suffix.next();
    let directory = suffix.next();
    let root = suffix.next();
    let mut name = format!("file-{:04}", index + 1);
    if !extension.is_empty() {
        name.push('.');
        name.push_str(extension);
    }
    file == Some(name.as_str()) && directory == Some(stage_dir) && root == Some("staged-files-v1")
}

#[cfg(test)]
fn decode_completion(
    raw: &[u8],
    header: &p::UploadHeader,
    hashes: &[String],
) -> Result<StageResponse, ContractError> {
    if !valid_header(header, true) || hashes.len() != header.files.len() {
        return Err(ContractError::Invalid);
    }
    let Object(response): Object<RawResponse> = parse_bounded(raw, MAX_RESPONSE)?;
    let Object(session) = response.session.ok_or(ContractError::Invalid)?;
    let session = StageSession {
        id: session.id.unwrap_or_default(),
        created_at: session.created_at.unwrap_or_default(),
    };
    let value = p::StageResult {
        protocol_version: response
            .protocol_version
            .unwrap_or_default()
            .try_into()
            .map_err(|_| ContractError::Invalid)?,
        request_id: response.request_id.unwrap_or_default(),
        stage_id: response.stage_id.unwrap_or_default(),
        session: Some(p::Session {
            id: session.id,
            created_at: session.created_at,
        }),
        expires_at_unix: response.expires_at_unix.unwrap_or_default(),
        files: response
            .files
            .ok_or(ContractError::Invalid)?
            .0
            .into_iter()
            .map(|Object(file)| {
                Ok(p::StageFile {
                    index: file
                        .index
                        .unwrap_or_default()
                        .try_into()
                        .map_err(|_| ContractError::Invalid)?,
                    path: file.path.unwrap_or_default(),
                    size: file.size.unwrap_or_default(),
                    sha256: file.sha256.unwrap_or_default(),
                })
            })
            .collect::<Result<_, ContractError>>()?,
    };
    validate_completion(&value, header, hashes)
}

pub(crate) fn validate_completion(
    response: &p::StageResult,
    header: &p::UploadHeader,
    hashes: &[String],
) -> Result<StageResponse, ContractError> {
    if !valid_header(header, true) || hashes.len() != header.files.len() {
        return Err(ContractError::Invalid);
    }
    let session = response.session.as_ref().ok_or(ContractError::Invalid)?;
    let expected = header.session.as_ref().ok_or(ContractError::Invalid)?;
    let request_id = &response.request_id;
    let stage_id = &response.stage_id;
    let expires = response.expires_at_unix;
    if response.protocol_version != 1
        || request_id != &header.request_id
        || session.id != expected.id
        || session.created_at != expected.created_at
        || !valid_id(stage_id)
        || expires < 1
    {
        return Err(ContractError::Invalid);
    }
    let raw_files = &response.files;
    if raw_files.len() != header.files.len() {
        return Err(ContractError::Invalid);
    }
    let stage_dir = format!("{expires}-{stage_id}");
    let mut files = Vec::with_capacity(raw_files.len());
    for (index, raw_file) in raw_files.iter().enumerate() {
        let metadata = &header.files[index];
        let path = &raw_file.path;
        let sha256 = &raw_file.sha256;
        if raw_file.index as usize != index
            || raw_file.size != metadata.size
            || sha256 != &hashes[index]
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_path(path, &stage_dir, index, &metadata.extension)
        {
            return Err(ContractError::Invalid);
        }
        files.push(StageFile {
            index,
            path: path.clone(),
            size: metadata.size,
            sha256: sha256.clone(),
        });
    }
    Ok(StageResponse {
        protocol_version: 1,
        request_id: request_id.clone(),
        stage_id: stage_id.clone(),
        session: StageSession {
            id: session.id.clone(),
            created_at: session.created_at,
        },
        expires_at_unix: expires,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        json: String,
        rust_valid: bool,
        #[serde(default)]
        header: serde_json::Value,
    }
    #[derive(Deserialize)]
    struct Fixture {
        csrf: String,
        header: serde_json::Value,
        hashes: Vec<String>,
        starts: Vec<Case>,
        responses: Vec<Case>,
    }

    fn oracle() -> Fixture {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/upload-v1/go-oracle.json"
        ))
        .unwrap()
    }

    #[test]
    fn go_browser_start_and_finish_contract() {
        let oracle = oracle();
        for case in &oracle.starts {
            let parsed = decode_start(case.json.as_bytes(), &oracle.csrf);
            assert_eq!(parsed.is_ok(), case.rust_valid, "{}", case.name);
            if let Ok(header) = parsed {
                assert_eq!(header.request_id, "");
                assert_eq!(
                    header.protocol_version as u64,
                    case.header["protocol_version"].as_u64().unwrap()
                );
                assert_eq!(
                    header.file_count as u64,
                    case.header["file_count"].as_u64().unwrap()
                );
                assert_eq!(
                    header.total_bytes,
                    case.header["total_bytes"].as_i64().unwrap()
                );
                assert_eq!(
                    header.session.as_ref().unwrap().id,
                    case.header["session"]["id"].as_str().unwrap()
                );
                assert_eq!(
                    header.session.as_ref().unwrap().created_at,
                    case.header["session"]["created_at"].as_i64().unwrap()
                );
                for (index, file) in header.files.iter().enumerate() {
                    assert_eq!(file.index as usize, index);
                    assert_eq!(
                        file.size,
                        case.header["files"][index]["size"].as_i64().unwrap()
                    );
                    assert_eq!(
                        file.extension,
                        case.header["files"][index]["extension"]
                            .as_str()
                            .unwrap_or("")
                    );
                }
            }
        }
        for bad in [
            r#"["start","synthetic-csrf",{"id":"$12","created_at":123},[{"size":2,"extension":"txt"},{"size":3}]]"#,
            r#"{"type":"start","csrf":"synthetic-csrf","session":["$12",123],"files":[{"size":2,"extension":"txt"},{"size":3}]}"#,
            r#"{"type":"start","csrf":"synthetic-csrf","session":{"id":"$12","created_at":123},"files":[[2,"txt"],{"size":3}]}"#,
        ] {
            assert!(decode_start(bad.as_bytes(), &oracle.csrf).is_err());
        }
        assert!(decode_start(&vec![b' '; MAX_START + 1], &oracle.csrf).is_err());
        assert!(decode_finish(br#"{"type":"finish"}"#).is_ok());
        for bad in [
            br#"["finish"]"#.as_slice(),
            br#"{"type":"finish","extra":1}"#,
            br#"{"type":"bad"}"#,
            br#"{"type":"finish"}{}"#,
        ] {
            assert!(decode_finish(bad).is_err());
        }
    }

    #[test]
    fn crossing_file_boundary_hashes_and_go_completion_contract() {
        let oracle = oracle();
        let valid_start = oracle
            .starts
            .iter()
            .find(|case| case.name == "valid")
            .unwrap();
        let mut header = decode_start(valid_start.json.as_bytes(), &oracle.csrf).unwrap();
        header.request_id = oracle.header["request_id"].as_str().unwrap().to_owned();
        let mut tracker = UploadHashes::new(&header).unwrap();
        assert_eq!(tracker.consume(b"a").unwrap(), 1);
        assert_eq!(tracker.consume(b"bcde").unwrap(), 5);
        assert!(tracker.is_complete());
        assert!(tracker.consume(b"x").is_err());
        let hashes = tracker.finish().unwrap();
        assert_eq!(hashes, oracle.hashes);
        for case in &oracle.responses {
            let parsed = decode_completion(case.json.as_bytes(), &header, &hashes);
            assert_eq!(parsed.is_ok(), case.rust_valid, "{}", case.name);
            if let Ok(stage) = parsed {
                assert_eq!(
                    serde_json::to_value(stage).unwrap(),
                    serde_json::from_str::<serde_json::Value>(&case.json).unwrap()
                );
            }
        }
        // The fixture proves object decoding; a nested array is rejected even
        // when it would otherwise carry the right positional values.
        let array_response = format!(
            r#"{{"protocol_version":1,"request_id":"{}","stage_id":"abcdef0123456789abcdef0123456789","session":["$12",123],"expires_at_unix":1790000000,"files":[]}}"#,
            header.request_id
        );
        assert!(decode_completion(array_response.as_bytes(), &header, &hashes).is_err());
        assert!(decode_completion(&vec![b' '; MAX_RESPONSE + 1], &header, &hashes).is_err());
        let mut tracker = UploadHashes::new(&header).unwrap();
        assert!(tracker.consume(&vec![b'x'; MAX_CHUNK + 1]).is_err());
        assert_eq!(tracker.finish().unwrap_err(), ContractError::Incomplete);
    }
}

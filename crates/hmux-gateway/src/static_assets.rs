//! Configured on-disk web assets with bounded, demand-driven streaming.
//! The administrator's root may traverse deployment symlinks. Reopen that root
//! for each request so an atomic `current` switch is visible without a restart;
//! all client-controlled descendants are then resolved through directory fds.
use crate::http_boundary::{self as boundary, Body, Reply};
use bytes::Bytes;
use http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use rustix::fs::{self, Mode, OFlags};
use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[path = "static_io.rs"]
mod file_io;
pub(crate) use file_io::FileBody;
use file_io::{FileLease, Part, Resources};
const MAX_RANGES: usize = 32;

pub struct Assets {
    root: PathBuf,
    protected: Arc<Vec<PathBuf>>,
    resources: Arc<Resources>,
}
struct Opened {
    file: File,
    size: u64,
    modified: Option<SystemTime>,
    path: String,
}
enum Found {
    File(Opened),
    Redirect(String),
}
impl Assets {
    /// Startup-only validation. Public release files may be owned by a different
    /// administrator; unlike private state they do not require service ownership.
    pub fn open(root: &Path) -> io::Result<Self> {
        Self::open_excluding(root, Vec::new())
    }

    /// Recheck private-state exclusions for each live release-root switch.
    /// Protected paths are canonical private files or normalized state paths
    /// beneath their canonical directory, including not-yet-created stores.
    pub fn open_excluding(root: &Path, protected: Vec<PathBuf>) -> io::Result<Self> {
        let root = std::path::absolute(root)?;
        if protected.iter().any(|path| {
            !path.is_absolute()
                || !path
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        }) {
            return Err(io::Error::other(
                "absolute private asset exclusions required",
            ));
        }
        match open_asset(&root, "", &protected)? {
            Found::File(_) => {}
            _ => return Err(io::Error::other("asset index missing")),
        }
        Ok(Self {
            root,
            protected: Arc::new(protected),
            resources: Resources::new(),
        })
    }
    pub async fn shutdown(&self) {
        self.resources.shutdown().await;
    }
    pub async fn serve<B>(&self, request: &Request<B>) -> Reply {
        let mut reply = self.serve_inner(request).await;
        // HEAD has exactly the same representation headers, never a body,
        // including error and conditional responses.
        if request.method() == Method::HEAD {
            *reply.body_mut() = Body::new(Bytes::new());
        }
        reply
    }
    async fn serve_inner<B>(&self, request: &Request<B>) -> Reply {
        if request.method() != Method::GET && request.method() != Method::HEAD {
            return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
        }
        let Some(path) = decode_path(request.uri().path()) else {
            return boundary::error(StatusCode::NOT_FOUND);
        };
        if path == "connect" || path.starts_with("api/") {
            return boundary::error(StatusCode::NOT_FOUND);
        }
        if path.ends_with("/index.html") || path == "index.html" {
            return redirect("./", request.uri().query());
        }
        let Ok(permit) = self.resources.streams.clone().try_acquire_owned() else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let root = self.root.clone();
        let protected = self.protected.clone();
        // The worker retains stream admission if the request is cancelled.
        let opened = self
            .resources
            .run(move || Ok((open_asset(&root, &path, &protected)?, permit)))
            .await;
        let (file, permit) = match opened {
            Ok((Found::File(file), permit)) => (file, permit),
            Ok((Found::Redirect(location), _permit)) => {
                return redirect(&location, request.uri().query())
            }
            Err(error) => {
                return boundary::error(match error.kind() {
                    io::ErrorKind::NotFound
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::NotADirectory => StatusCode::NOT_FOUND,
                    _ => StatusCode::SERVICE_UNAVAILABLE,
                })
            }
        };
        if let Some(status) = conditional_status(request.headers(), file.modified) {
            let mut reply = Response::new(Body::new(Bytes::new()));
            *reply.status_mut() = status;
            modified_header(&mut reply, file.modified);
            boundary::secure_headers(&mut reply);
            return reply;
        }
        let content_type = mime_type(&file.path);
        let mut ranges = None;
        if allow_range(request.headers(), file.modified) {
            if let Some(value) = request
                .headers()
                .get(header::RANGE)
                .filter(|v| !v.is_empty())
            {
                match value
                    .to_str()
                    .map_err(|_| RangeError::Invalid)
                    .and_then(|value| parse_ranges(value, file.size))
                {
                    Ok(parsed) => ranges = parsed,
                    Err(error) => {
                        let mut reply = boundary::error(StatusCode::RANGE_NOT_SATISFIABLE);
                        if error == RangeError::NoOverlap {
                            reply.headers_mut().insert(
                                header::CONTENT_RANGE,
                                format!("bytes */{}", file.size).parse().unwrap(),
                            );
                        }
                        return reply;
                    }
                }
            }
        }
        let mut parts = Vec::new();
        let mut status = StatusCode::OK;
        let mut mime = content_type.to_owned();
        let mut range_header = None;
        match ranges {
            None => parts.push(Part::Span {
                start: 0,
                len: file.size,
            }),
            Some(ranges) if ranges.len() == 1 => {
                status = StatusCode::PARTIAL_CONTENT;
                let (start, end) = ranges[0];
                range_header = Some(format!("bytes {start}-{}/{}", end as i64 - 1, file.size));
                parts.push(Part::Span {
                    start,
                    len: end - start,
                });
            }
            Some(ranges) => {
                status = StatusCode::PARTIAL_CONTENT;
                let mut nonce = [0_u8; 30];
                if getrandom::fill(&mut nonce).is_err() {
                    return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
                }
                let boundary = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
                mime = format!("multipart/byteranges; boundary={boundary}");
                for (start, end) in ranges {
                    // Header order and boundary length match Go's MIME writer.
                    parts.push(Part::Literal(Bytes::from(format!("--{boundary}\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Type: {content_type}\r\n\r\n", end as i64 - 1, file.size))));
                    parts.push(Part::Span {
                        start,
                        len: end - start,
                    });
                    parts.push(Part::Literal(Bytes::from_static(b"\r\n")));
                }
                parts.push(Part::Literal(Bytes::from(format!("--{boundary}--\r\n"))));
            }
        }
        let body = FileBody::new(
            FileLease {
                file: file.file,
                _permit: permit,
            },
            self.resources.clone(),
            parts,
        );
        let size = hyper::body::Body::size_hint(&body).exact().unwrap();
        let mut reply = response(status, &mime, Body::file(body), size);
        reply
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if let Some(value) = range_header {
            reply
                .headers_mut()
                .insert(header::CONTENT_RANGE, value.parse().unwrap());
        }
        modified_header(&mut reply, file.modified);
        reply
    }
}

fn open_asset(root: &Path, path: &str, protected: &[PathBuf]) -> io::Result<Found> {
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    // Only the trusted configured root can follow a link. No request component
    // is ever joined to this path before opening it.
    let mut file = if protected.is_empty() {
        File::from(fs::open(root, directory, Mode::empty())?)
    } else {
        let resolved = root.canonicalize()?;
        if protected
            .iter()
            .any(|private| private.starts_with(&resolved) || resolved.starts_with(private))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private asset root",
            ));
        }
        // Open exactly the checked resolution, refusing links introduced after
        // canonicalization. Subsequent request traversal stays relative to this fd.
        let mut current = File::from(fs::open("/", directory, Mode::empty())?);
        for part in resolved.components() {
            if let Component::Normal(name) = part {
                current = File::from(fs::openat(
                    &current,
                    name,
                    directory | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?);
            }
        }
        current
    };
    let path_without_slash = path.trim_end_matches('/');
    let mut components = path_without_slash
        .split('/')
        .filter(|p| !p.is_empty())
        .peekable();
    while let Some(name) = components.next() {
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        file = File::from(
            fs::openat(&file, name, flags, Mode::empty()).map_err(|error| {
                // Disallowed client links are missing resources, not server errors.
                if error == rustix::io::Errno::LOOP {
                    io::Error::new(io::ErrorKind::NotFound, "asset link")
                } else {
                    error.into()
                }
            })?,
        );
    }
    let mut meta = file.metadata()?;
    let mut path = path.to_owned();
    if meta.is_dir() {
        if !path.is_empty() && !path.ends_with('/') {
            return Ok(Found::Redirect(format!(
                "{}/",
                path.rsplit('/').next().unwrap()
            )));
        }
        // Deliberately no directory listing or private/dotfile publication.
        file = File::from(fs::openat(
            &file,
            "index.html",
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        meta = file.metadata()?;
        path.push_str("index.html");
    } else if path.ends_with('/') {
        return Ok(Found::Redirect(format!(
            "../{}",
            path_without_slash.rsplit('/').next().unwrap()
        )));
    }
    if !meta.is_file() || meta.len() > i64::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "not a regular asset",
        ));
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .filter(|d| d.as_secs() > 0 && d.as_secs() < 253402300800)
        .map(|d| UNIX_EPOCH + Duration::from_secs(d.as_secs()));
    Ok(Found::File(Opened {
        file,
        size: meta.len(),
        modified,
        path,
    }))
}
fn conditional_status(headers: &HeaderMap, modified: Option<SystemTime>) -> Option<StatusCode> {
    let header = |name| headers.get(name).filter(|v| !v.is_empty());
    if let Some(value) = header(header::IF_MATCH) {
        if !wildcard_etag(value.as_bytes()) {
            return Some(StatusCode::PRECONDITION_FAILED);
        }
    } else if modified
        .zip(date(headers, header::IF_UNMODIFIED_SINCE))
        .is_some_and(|(m, d)| m > d)
    {
        return Some(StatusCode::PRECONDITION_FAILED);
    }
    if let Some(value) = header(header::IF_NONE_MATCH) {
        if wildcard_etag(value.as_bytes()) {
            return Some(StatusCode::NOT_MODIFIED);
        }
    } else if modified
        .zip(date(headers, header::IF_MODIFIED_SINCE))
        .is_some_and(|(m, d)| m <= d)
    {
        return Some(StatusCode::NOT_MODIFIED);
    }
    None
}
fn date(headers: &HeaderMap, name: header::HeaderName) -> Option<SystemTime> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .and_then(|v| httpdate::parse_http_date(v).ok())
}
fn allow_range(headers: &HeaderMap, modified: Option<SystemTime>) -> bool {
    headers.get(header::IF_RANGE).is_none_or(|v| v.is_empty())
        || modified
            .zip(date(headers, header::IF_RANGE))
            .is_some_and(|(m, d)| m == d)
}
fn modified_header(reply: &mut Reply, modified: Option<SystemTime>) {
    if let Some(time) = modified {
        reply.headers_mut().insert(
            header::LAST_MODIFIED,
            httpdate::fmt_http_date(time).parse().unwrap(),
        );
    }
}
fn wildcard_etag(mut value: &[u8]) -> bool {
    loop {
        while value
            .first()
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b',')
        {
            value = &value[1..];
        }
        if value.first() == Some(&b'*') {
            return true;
        }
        if value.starts_with(b"W/") {
            value = &value[2..];
        }
        if value.first() != Some(&b'"') {
            return false;
        }
        value = &value[1..];
        let Some(end) = value.iter().position(|b| *b == b'"') else {
            return false;
        };
        if value[..end].iter().any(|b| *b < 0x21 || *b == 0x7f) {
            return false;
        }
        value = &value[end + 1..];
    }
}
fn response(status: StatusCode, content_type: &str, body: Body, size: u64) -> Reply {
    let mut reply = Response::new(body);
    *reply.status_mut() = status;
    reply
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    reply
        .headers_mut()
        .insert(header::CONTENT_LENGTH, size.to_string().parse().unwrap());
    boundary::secure_headers(&mut reply);
    reply
}
fn redirect(location: &str, query: Option<&str>) -> Reply {
    let location = match query {
        Some(q) => format!("{location}?{q}"),
        None => location.into(),
    };
    let mut reply = Response::new(Body::new(Bytes::new()));
    *reply.status_mut() = StatusCode::MOVED_PERMANENTLY;
    boundary::secure_headers(&mut reply);
    let Ok(location) = location.parse() else {
        return boundary::error(StatusCode::BAD_REQUEST);
    };
    reply.headers_mut().insert(header::LOCATION, location);
    reply
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 2048
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.starts_with('.'))
}
fn decode_path(raw: &str) -> Option<String> {
    if raw.len() > 6144 {
        return None;
    }
    let raw = raw.strip_prefix('/')?.as_bytes();
    let mut out = Vec::with_capacity(raw.len().min(2048));
    let mut i = 0;
    while i < raw.len() {
        let byte = if raw[i] == b'%' {
            let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            let b = hex(*raw.get(i + 1)?)? * 16 + hex(*raw.get(i + 2)?)?;
            i += 3;
            b
        } else {
            let b = raw[i];
            i += 1;
            b
        };
        if out.len() == 2048 {
            return None;
        }
        out.push(byte);
    }
    let path = String::from_utf8(out).ok()?;
    if !path.is_empty() && !safe_path(path.strip_suffix('/').unwrap_or(&path)) {
        return None;
    }
    Some(path)
}
fn mime_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}
#[derive(Debug, PartialEq, Eq)]
enum RangeError {
    Invalid,
    NoOverlap,
}
fn parse_ranges(header: &str, size: u64) -> Result<Option<Vec<(u64, u64)>>, RangeError> {
    let value = header.strip_prefix("bytes=").ok_or(RangeError::Invalid)?;
    let mut ranges = Vec::new();
    let mut total = 0_u64;
    let mut no_overlap = false;
    for (index, part) in value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        if index == MAX_RANGES {
            return Ok(None);
        }
        let (a, b) = part.split_once('-').ok_or(RangeError::Invalid)?;
        let (a, b) = (a.trim(), b.trim());
        let number = |v: &str| {
            v.parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .map(|v| v as u64)
                .ok_or(RangeError::Invalid)
        };
        let range = if a.is_empty() {
            // ParseInt accepts negative zero, but the Go HTTP suffix grammar
            // explicitly forbids a leading minus sign.
            if b.starts_with('-') {
                return Err(RangeError::Invalid);
            }
            let suffix = number(b)?;
            (size.saturating_sub(suffix), size)
        } else {
            let start = number(a)?;
            if start >= size {
                no_overlap = true;
                continue;
            }
            let end = if b.is_empty() { size - 1 } else { number(b)? };
            if start > end {
                return Err(RangeError::Invalid);
            }
            (start, end.min(size - 1) + 1)
        };
        total = total
            .checked_add(range.1 - range.0)
            .ok_or(RangeError::Invalid)?;
        ranges.push(range);
    }
    if ranges.is_empty() {
        return if no_overlap && size != 0 {
            Err(RangeError::NoOverlap)
        } else {
            Ok(None)
        };
    }
    if total > size {
        return Ok(None);
    }
    Ok(Some(ranges))
}

#[cfg(test)]
#[path = "static_assets_tests.rs"]
mod tests;

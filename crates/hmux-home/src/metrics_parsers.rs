//! Pure, bounded decoders for host metric command output.

use hmux_model::MAXIMUM_HOST_MEMORY_BYTES;
use quick_xml::events::Event;
use quick_xml::Reader;

const TEXT_MAX: usize = 64 * 1024;
const GPU_XML_MAX: usize = 2 * 1024 * 1024;
const XML_TOKEN_MAX: usize = 64 * 1024;
const XML_DEPTH_MAX: usize = 32;
const XML_FIELD_MAX: usize = 128;
const PLIST_DOCTYPE: &str = "plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"";
const DISK_MAX_BYTES: u64 = 1 << 53;

fn text(raw: &[u8]) -> Option<&str> {
    (raw.len() <= TEXT_MAX)
        .then(|| std::str::from_utf8(raw).ok())
        .flatten()
}

fn decimal(raw: &str) -> Option<u64> {
    (!raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

fn percentage(raw: &str) -> Option<f64> {
    if raw.is_empty()
        || !raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || raw.bytes().filter(|byte| *byte == b'.').count() > 1
    {
        return None;
    }
    let value: f64 = raw.parse().ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}

fn gpu_percentage(raw: &str) -> Option<f64> {
    let value: f64 = raw.parse().ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}

/// CPU-only `iostat -d -C -n 0 -c 2 -w 1`: discard the since-boot first row
/// and use the one-second interval row. No process enumeration is needed.
pub fn cpu_darwin(raw: &[u8]) -> Option<f64> {
    let mut fields = text(raw)?.split_ascii_whitespace();
    for header in ["cpu", "us", "sy", "id"] {
        if fields.next() != Some(header) {
            return None;
        }
    }
    let mut last_idle = 0.0;
    for _ in 0..2 {
        percentage(fields.next()?)?;
        percentage(fields.next()?)?;
        last_idle = percentage(fields.next()?)?;
    }
    fields.next().is_none().then_some(100.0 - last_idle)
}

fn page_size(header: &str) -> Option<u64> {
    let after = header.split_once("page size of ")?.1;
    let before = after.split_once(" bytes")?.0;
    decimal(before).filter(|size| *size > 0)
}

/// Activity Monitor-like resident use: anonymous - purgeable + wired + compressed.
pub fn memory_darwin(vm: &[u8], total: &[u8]) -> Option<(u64, u64)> {
    let vm = text(vm)?;
    let total = decimal(text(total)?.trim())?;
    if total == 0 || total > MAXIMUM_HOST_MEMORY_BYTES {
        return None;
    }
    let page_size = page_size(vm.lines().next()?)?;
    let mut values = [None; 4];
    for line in vm.lines() {
        let Some((name, raw)) = line.split_once(':') else {
            continue;
        };
        let index = match name {
            "Anonymous pages" => 0,
            "Pages wired down" => 1,
            "Pages purgeable" => 2,
            "Pages occupied by compressor" => 3,
            _ => continue,
        };
        let raw = raw.trim();
        values[index] = Some(decimal(raw.strip_suffix('.').unwrap_or(raw))?);
    }
    let [Some(anonymous), Some(wired), Some(purgeable), Some(compressed)] = values else {
        return None;
    };
    let pages = anonymous
        .saturating_sub(purgeable)
        .checked_add(wired)?
        .checked_add(compressed)?;
    let used = pages.checked_mul(page_size)?;
    (used <= total).then_some((used, total))
}

fn cpu_times(raw: &[u8]) -> Option<(u64, u64)> {
    let raw = text(raw)?;
    for line in raw.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some("cpu") {
            continue;
        }
        let mut total = 0u64;
        let mut idle = 0u64;
        let mut count = 0;
        // guest and guest_nice are already included in user and nice.
        for (index, field) in fields.take(8).enumerate() {
            let value = decimal(field)?;
            total = total.checked_add(value)?;
            if index == 3 || index == 4 {
                idle = idle.checked_add(value)?;
            }
            count += 1;
        }
        return (count >= 4 && total > 0).then_some((idle, total));
    }
    None
}

/// Percentage across two aggregate `/proc/stat` readings.
pub fn cpu_linux(first: &[u8], second: &[u8]) -> Option<f64> {
    let (idle_before, total_before) = cpu_times(first)?;
    let (idle_after, total_after) = cpu_times(second)?;
    let total_delta = total_after.checked_sub(total_before)?;
    let idle_delta = idle_after.checked_sub(idle_before)?;
    if total_delta == 0 || idle_delta > total_delta {
        return None;
    }
    Some((total_delta - idle_delta) as f64 / total_delta as f64 * 100.0)
}

/// `/proc/meminfo` reports used RAM as MemTotal minus MemAvailable.
pub fn memory_linux(raw: &[u8]) -> Option<(u64, u64)> {
    let raw = text(raw)?;
    let mut total = None;
    let mut available = None;
    for line in raw.lines() {
        let mut fields = line.split_ascii_whitespace();
        let target = match fields.next() {
            Some("MemTotal:") => &mut total,
            Some("MemAvailable:") => &mut available,
            _ => continue,
        };
        let number = decimal(fields.next()?)?;
        let unit = match fields.next() {
            Some("kB") => 1024,
            None => 1,
            _ => return None,
        };
        if fields.next().is_some() {
            return None;
        }
        *target = Some(number.checked_mul(unit)?);
    }
    let total = total?;
    let available = available?;
    if total == 0 || total > MAXIMUM_HOST_MEMORY_BYTES || available > total {
        return None;
    }
    Some((total - available, total))
}

/// Filesystem allocation, capped to the wire's exact integer range.
pub fn disk_bytes(blocks: u64, free: u64, block_size: u64) -> Option<(u64, u64)> {
    if blocks == 0 || block_size == 0 || free > blocks {
        return None;
    }
    let total = blocks.checked_mul(block_size)?;
    (total <= DISK_MAX_BYTES).then_some(((blocks - free) * block_size, total))
}

#[derive(Clone, Copy)]
enum GpuField {
    Canonical,
    Activity,
}

enum XmlCapture {
    Key,
    Value(GpuField),
}

/// Find the maximum canonical utilization across devices, falling back to activity.
/// The reader borrows each token from the bounded input and never builds an XML tree.
pub fn gpu_darwin(raw: &[u8]) -> Option<f64> {
    if raw.len() > GPU_XML_MAX || std::str::from_utf8(raw).is_err() {
        return None;
    }
    let mut reader = Reader::from_reader(raw);
    reader.config_mut().check_end_names = true;
    let mut depth = 0usize;
    let mut roots = 0usize;
    let mut doctype_seen = false;
    let mut capture: Option<(XmlCapture, usize, String)> = None;
    let mut pending: Option<(GpuField, usize)> = None;
    let mut canonical: Option<f64> = None;
    let mut activity: Option<f64> = None;

    loop {
        let event = reader.read_event().ok()?;
        match event {
            Event::Start(start) => {
                if start.len() > XML_TOKEN_MAX {
                    return None;
                }
                if depth == 0 {
                    roots += 1;
                    if roots != 1 || start.name().as_ref() != b"plist" {
                        return None;
                    }
                }
                depth = depth.checked_add(1)?;
                if depth > XML_DEPTH_MAX || capture.is_some() {
                    return None;
                }
                let name = start.name();
                let name = name.as_ref();
                let next_value = pending.take().and_then(|(kind, parent_depth)| {
                    (depth == parent_depth + 1 && matches!(name, b"integer" | b"real" | b"string"))
                        .then_some(kind)
                });
                if name == b"key" {
                    capture = Some((XmlCapture::Key, depth, String::new()));
                } else if let Some(kind) = next_value {
                    capture = Some((XmlCapture::Value(kind), depth, String::new()));
                }
            }
            Event::Empty(empty) => {
                if empty.len() > XML_TOKEN_MAX || depth == 0 || capture.is_some() {
                    return None;
                }
                pending = None;
            }
            Event::Text(value) => {
                if value.len() > XML_TOKEN_MAX {
                    return None;
                }
                if depth == 0 && !value.iter().all(u8::is_ascii_whitespace) {
                    return None;
                }
                if let Some((_, _, content)) = capture.as_mut() {
                    let piece = value.decode().ok()?;
                    if content.len().checked_add(piece.len())? > XML_FIELD_MAX {
                        return None;
                    }
                    content.push_str(&piece);
                }
            }
            Event::GeneralRef(reference) => {
                // XML's five predefined entities and numeric character refs
                // need no DTD or network access. They can also occur in an
                // unrelated registry string; do not discard valid utilization.
                if depth == 0 || reference.len() > XML_FIELD_MAX {
                    return None;
                }
                let mut encoded = [0u8; 4];
                let character = reference.resolve_char_ref().ok()?;
                let piece = match character {
                    Some(c) => c.encode_utf8(&mut encoded),
                    None => quick_xml::escape::resolve_xml_entity(&reference.decode().ok()?)?,
                };
                if let Some((_, _, content)) = capture.as_mut() {
                    if content.len().checked_add(piece.len())? > XML_FIELD_MAX {
                        return None;
                    }
                    content.push_str(piece);
                }
            }
            Event::End(end) => {
                if depth == 0 || end.len() > XML_TOKEN_MAX {
                    return None;
                }
                if let Some((kind, at_depth, content)) = capture.take() {
                    if at_depth != depth {
                        return None;
                    }
                    match kind {
                        XmlCapture::Key => {
                            let field = match content.as_str() {
                                "Device Utilization %" => Some(GpuField::Canonical),
                                "GPU Activity(%)" => Some(GpuField::Activity),
                                _ => None,
                            };
                            pending = field.map(|field| (field, depth - 1));
                        }
                        XmlCapture::Value(field) => {
                            if let Some(value) = gpu_percentage(content.trim()) {
                                let maximum = match field {
                                    GpuField::Canonical => &mut canonical,
                                    GpuField::Activity => &mut activity,
                                };
                                *maximum = Some(maximum.map_or(value, |old| old.max(value)));
                            }
                        }
                    }
                }
                depth -= 1;
                if pending.is_some_and(|(_, parent_depth)| depth < parent_depth) {
                    pending = None;
                }
            }
            Event::Decl(decl) => {
                if depth != 0 || roots != 0 || decl.len() > XML_TOKEN_MAX {
                    return None;
                }
            }
            Event::DocType(doctype) => {
                // ioreg emits Apple's standard plist declaration. quick-xml
                // never fetches it; only predefined/character references are
                // decoded. No internal subsets or other DTDs are needed.
                if depth != 0
                    || roots != 0
                    || doctype_seen
                    || doctype.len() > PLIST_DOCTYPE.len()
                    || doctype.decode().ok()?.as_ref() != PLIST_DOCTYPE
                {
                    return None;
                }
                doctype_seen = true;
            }
            Event::Comment(comment) => {
                if comment.len() > XML_TOKEN_MAX {
                    return None;
                }
            }
            Event::Eof => break,
            // Reject CDATA, processing instructions and
            // other constructs: ioreg's XML plist output does not need them.
            _ => return None,
        }
    }
    if roots != 1 || depth != 0 || capture.is_some() {
        return None;
    }
    canonical.or(activity)
}

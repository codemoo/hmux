//! Strict, bounded Go diagnostic DTOs. No free-form content is retained.
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{
    de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::{fmt, marker::PhantomData, sync::Arc};

pub(super) const LIMIT: usize = 2048;
pub(super) const ACCOUNT_LIMIT: usize = 256;
pub(super) const DISK_BYTES: usize = 2 << 20;
pub(super) const TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn null_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
fn short<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<String, D::Error> {
    struct Text<const N: usize>;
    impl<const N: usize> Visitor<'_> for Text<N> {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded text")
        }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<String, E> {
            if s.len() > N {
                Err(E::custom("text too long"))
            } else {
                Ok(s.to_owned())
            }
        }
    }
    d.deserialize_any(Text::<N>)
}
struct Object<T>(PhantomData<T>);
impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
    type Value = T;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object")
    }
    fn visit_map<M: MapAccess<'de>>(self, m: M) -> Result<T, M::Error> {
        T::deserialize(de::value::MapAccessDeserializer::new(m))
    }
}
impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for Object<T> {
    type Value = T;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<T, D::Error> {
        d.deserialize_map(self)
    }
}
fn label<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    struct Label<T>(PhantomData<T>);
    impl<T: DeserializeOwned + Default> Visitor<'_> for Label<T> {
        type Value = T;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a diagnostic label")
        }
        fn visit_unit<E: de::Error>(self) -> Result<T, E> {
            Ok(T::default())
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<T, E> {
            T::deserialize(de::value::StrDeserializer::new(s))
        }
    }
    d.deserialize_any(Label::<T>(PhantomData))
}
fn rows<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    struct Rows<T, const N: usize>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for Rows<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a bounded object array")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Vec<T>, E> {
            Ok(Vec::new())
        }
        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Vec<T>, S::Error> {
            let mut result = Vec::new();
            while result.len() < N {
                let Some(value) = seq.next_element_seed(Object::<T>(PhantomData))? else {
                    return Ok(result);
                };
                result.push(value);
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("too many rows"));
            }
            Ok(result)
        }
    }
    d.deserialize_any(Rows::<T, N>(PhantomData))
}
macro_rules! labels {
    ($name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        #[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Deserialize, Serialize)]
        enum $name { #[default] #[serde(rename="")] Empty, $(#[serde(rename=$text)] $variant,)* }
        impl $name { fn empty(&self)->bool{self.label().is_empty()} fn label(self)->&'static str{match self {Self::Empty=>"",$(Self::$variant=>$text,)*}} }
    }
}
labels!(Kind {TerminalFailed=>"terminal-failed",TerminalRecovered=>"terminal-recovered",ApiFailed=>"api-failed",Offline=>"offline",Resume=>"resume",RuntimeError=>"runtime-error",UnhandledRejection=>"unhandled-rejection"});
labels!(Reason {Network=>"network",Timeout=>"timeout",Limit=>"limit",Unavailable=>"unavailable",OutputOverflow=>"output-overflow",Protocol=>"protocol",Http=>"http",TypeError=>"TypeError",RangeError=>"RangeError",ReferenceError=>"ReferenceError",SyntaxError=>"SyntaxError",Error=>"Error",Unknown=>"unknown"});
labels!(Route {Session=>"session",State=>"state",Action=>"action",Sessions=>"sessions",Account=>"account",Push=>"push",Upload=>"upload",Other=>"other"});
fn zero(v: &i64) -> bool {
    *v == 0
}
macro_rules! event_struct {
    ($name:ident {$($extra:tt)*}) => {
        #[derive(Clone,Default,Deserialize,Serialize)]
        #[serde(default,deny_unknown_fields)]
        pub(super) struct $name {
            $($extra)*
            #[serde(deserialize_with="null_default")] sequence:i64,
            #[serde(deserialize_with="null_default")] at:i64,
            #[serde(deserialize_with="label")] kind:Kind,
            # [serde(deserialize_with="label",skip_serializing_if="Reason::empty")] reason:Reason,
            #[serde(deserialize_with="label",skip_serializing_if="Route::empty")] route:Route,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] code:i64,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] attempt:i64,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] retry_ms:i64,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] duration_ms:i64,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] line:i64,
            #[serde(deserialize_with="null_default",skip_serializing_if="zero")] column:i64,
            #[serde(deserialize_with="null_default")] online:bool,
            #[serde(deserialize_with="null_default")] visible:bool,
            #[serde(deserialize_with="null_default")] standalone:bool,
        }
    }
}
event_struct!(Event {});
event_struct!(RecordWire {
    #[serde(deserialize_with = "short::<_,80>")]
    account: String,
    #[serde(deserialize_with = "short::<_,64>")]
    profile: String,
    #[serde(deserialize_with = "short::<_,36>")]
    client: String,
    #[serde(deserialize_with = "short::<_,71>")]
    build: String,
    #[serde(deserialize_with = "short::<_,32>")]
    browser: String,
    #[serde(deserialize_with = "short::<_,40>")]
    received_at: String,
});
impl Event {
    fn valid(&self, now: DateTime<Utc>) -> bool {
        let now = now.timestamp_millis();
        (1..=i32::MAX as i64).contains(&self.sequence)
            && self.at >= now.saturating_sub(TTL_MS)
            && self.at <= now.saturating_add(300_000)
            && !self.kind.empty()
            && (0..=4999).contains(&self.code)
            && (0..=1_000_000).contains(&self.attempt)
            && (0..=60_000).contains(&self.retry_ms)
            && (0..=86_400_000).contains(&self.duration_ms)
            && (0..=10_000_000).contains(&self.line)
            && (0..=10_000_000).contains(&self.column)
    }
    pub(super) fn sequence(&self) -> i64 {
        self.sequence
    }
    pub(super) fn failure_key(&self) -> Option<String> {
        matches!(
            self.kind,
            Kind::TerminalFailed | Kind::ApiFailed | Kind::RuntimeError | Kind::UnhandledRejection
        )
        .then(|| format!("{}:{}", self.kind.label(), self.reason.label()))
    }
}
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Batch {
    #[serde(deserialize_with = "null_default")]
    version: i64,
    #[serde(deserialize_with = "short::<_,36>")]
    client: String,
    #[serde(deserialize_with = "short::<_,71>")]
    build: String,
    #[serde(deserialize_with = "rows::<_,Event,20>")]
    events: Vec<Event>,
}
impl Batch {
    pub fn decode(raw: &[u8]) -> Result<Self, super::Error> {
        if raw.len() > crate::http_boundary::MAX_JSON_BYTES
            || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
        {
            return Err(super::Error::Invalid);
        }
        serde_json::from_slice(raw).map_err(|_| super::Error::Invalid)
    }
    pub(super) fn valid(&self, now: DateTime<Utc>) -> bool {
        self.version == 1
            && valid_client(&self.client)
            && valid_build(&self.build)
            && !self.events.is_empty()
            && self.events.len() <= 20
            && self.events.iter().all(|e| e.valid(now))
    }
    pub(super) fn records(
        self,
        source: Source,
        now: DateTime<Utc>,
    ) -> impl Iterator<Item = Record> {
        let source = Arc::new(Source {
            client: self.client,
            build: self.build,
            ..source
        });
        self.events.into_iter().map(move |event| Record {
            source: source.clone(),
            received: now,
            event,
        })
    }
}
#[derive(Default, PartialEq, Eq)]
pub(super) struct Source {
    pub account: String,
    pub profile: String,
    pub client: String,
    pub build: String,
    pub browser: String,
}
#[derive(Clone)]
pub(super) struct Record {
    pub source: Arc<Source>,
    pub received: DateTime<Utc>,
    pub event: Event,
}
impl Record {
    pub fn same_owner(&self, account: &str, profile: &str) -> bool {
        self.source.account == account && self.source.profile == profile
    }
    fn valid(&self, now: DateTime<Utc>) -> bool {
        valid_owner(&self.source.account, &self.source.profile)
            && valid_client(&self.source.client)
            && valid_build(&self.source.build)
            && valid_browser(&self.source.browser)
            && self.received != DateTime::from_timestamp(-62135596800, 0).unwrap()
            && self.received.signed_duration_since(now) <= chrono::Duration::minutes(5)
            && self.event.valid(self.received)
    }
}
impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let row = Object::<RecordWire>(PhantomData).deserialize(d)?;
        let received = DateTime::parse_from_rfc3339(&row.received_at)
            .map_err(|_| de::Error::custom("invalid receipt time"))?
            .with_timezone(&Utc);
        let event = Event {
            sequence: row.sequence,
            at: row.at,
            kind: row.kind,
            reason: row.reason,
            route: row.route,
            code: row.code,
            attempt: row.attempt,
            retry_ms: row.retry_ms,
            duration_ms: row.duration_ms,
            line: row.line,
            column: row.column,
            online: row.online,
            visible: row.visible,
            standalone: row.standalone,
        };
        Ok(Self {
            source: Arc::new(Source {
                account: row.account,
                profile: row.profile,
                client: row.client,
                build: row.build,
                browser: row.browser,
            }),
            received,
            event,
        })
    }
}
#[derive(Serialize)]
struct RecordView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a str>,
    client: &'a str,
    build: &'a str,
    browser: &'a str,
    #[serde(serialize_with = "go_time")]
    received_at: DateTime<Utc>,
    #[serde(flatten)]
    event: &'a Event,
}
impl Record {
    fn view(&self, private: bool) -> RecordView<'_> {
        RecordView {
            account: private.then_some(self.source.account.as_str()),
            profile: (private && !self.source.profile.is_empty())
                .then_some(self.source.profile.as_str()),
            client: &self.source.client,
            build: &self.source.build,
            browser: &self.source.browser,
            received_at: self.received,
            event: &self.event,
        }
    }
}
impl Serialize for Record {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.view(true).serialize(s)
    }
}
pub(super) struct Export(pub Vec<Record>);
impl Serialize for Export {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|r| r.view(false)))
    }
}
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Disk {
    #[serde(deserialize_with = "null_default")]
    version: i64,
    #[serde(deserialize_with = "rows::<_,Record,LIMIT>")]
    records: Vec<Record>,
}
pub(super) fn decode_disk(raw: &[u8], now: DateTime<Utc>) -> Result<Vec<Record>, super::Error> {
    if raw.len() > DISK_BYTES || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(super::Error::Invalid);
    }
    let disk: Disk = serde_json::from_slice(raw).map_err(|_| super::Error::Invalid)?;
    if disk.version != 1 {
        return Err(super::Error::Invalid);
    }
    let mut records: Vec<Record> = Vec::with_capacity(disk.records.len());
    for mut record in disk.records {
        if !record.valid(now)
            || records
                .iter()
                .filter(|r| r.same_owner(&record.source.account, &record.source.profile))
                .count()
                >= ACCOUNT_LIMIT
        {
            return Err(super::Error::Invalid);
        }
        // Restart restores sharing too, without an unbounded interning cache.
        if let Some(previous) = records.iter().rev().find(|r| r.source == record.source) {
            record.source = previous.source.clone();
        }
        records.push(record);
    }
    Ok(records)
}
pub(super) fn valid_owner(account: &str, profile: &str) -> bool {
    !account.is_empty()
        && account.len() <= 80
        && account.trim() == account
        && (profile.is_empty() || profile.len() == 64 && lower_hex(profile.as_bytes()))
}
fn lower_hex(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}
fn valid_client(value: &str) -> bool {
    let v = value.as_bytes();
    v.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| v[i] == b'-')
        && v[14] == b'4'
        && b"89ab".contains(&v[19])
        && v.iter()
            .enumerate()
            .all(|(i, b)| [8, 13, 18, 23].contains(&i) || lower_hex(std::slice::from_ref(b)))
}
fn valid_build(value: &str) -> bool {
    matches!(value, "development" | "unknown")
        || value
            .strip_prefix("app-")
            .and_then(|v| v.strip_suffix(".js"))
            .is_some_and(|v| {
                !v.is_empty()
                    && v.len() <= 64
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
}
pub(super) fn valid_browser(value: &str) -> bool {
    let (browser, os) = value
        .split_once(" on ")
        .map_or((value, None), |(b, o)| (b, Some(o)));
    matches!(
        browser,
        "Unknown browser" | "Edge" | "Chrome" | "Firefox" | "Safari"
    ) && os.is_none_or(|o| {
        matches!(
            o,
            "Android" | "iOS" | "Windows" | "macOS" | "ChromeOS" | "Linux"
        )
    })
}
pub(super) fn go_time<S: Serializer>(
    value: &DateTime<Utc>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let raw = value.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let (prefix, fraction) = raw.trim_end_matches('Z').rsplit_once('.').unwrap();
    let fraction = fraction.trim_end_matches('0');
    serializer.serialize_str(&if fraction.is_empty() {
        format!("{prefix}Z")
    } else {
        format!("{prefix}.{fraction}Z")
    })
}

//! Browser controls are additive JSON, independent of the strict Home envelope.
//! Skip unused fields and stream capabilities instead of retaining their array.
use hmux_protocol::{flow, wire::SessionIdentity};
use serde::{
    de::{self, DeserializeOwned, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::value::RawValue;
use std::fmt;

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Control {
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    #[serde(deserialize_with = "session")]
    pub session: SessionIdentity,
    #[serde(deserialize_with = "null_default")]
    pub cols: u16,
    #[serde(deserialize_with = "null_default")]
    pub rows: u16,
    #[serde(deserialize_with = "null_default")]
    pub received: i64,
    #[serde(rename = "capabilities", deserialize_with = "capabilities")]
    pub output_flow: bool,
}
impl Control {
    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() > crate::browser_terminal::FRAME_BYTES
            || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
        {
            return None;
        }
        serde_json::from_slice(raw).ok()
    }
}
fn null_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
fn session<'de, D: Deserializer<'de>>(d: D) -> Result<SessionIdentity, D::Error> {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Identity {
        #[serde(deserialize_with = "null_default")]
        id: String,
        #[serde(deserialize_with = "null_default")]
        created_at: i64,
    }
    let Some(raw) = Option::<Box<RawValue>>::deserialize(d)? else {
        return Ok(SessionIdentity::default());
    };
    if !raw.get().trim_start().starts_with('{') {
        return Err(de::Error::custom("session object required"));
    }
    let identity: Identity =
        serde_json::from_str(raw.get()).map_err(|_| de::Error::custom("invalid session"))?;
    Ok(SessionIdentity {
        id: identity.id,
        created_at: identity.created_at,
    })
}
fn capabilities<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct Capabilities;
    impl<'de> Visitor<'de> for Capabilities {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("capability array or null")
        }
        fn visit_unit<E: de::Error>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
            let mut enabled = false;
            while let Some(capability) = seq.next_element::<Option<String>>()? {
                enabled |= capability.as_deref() == Some(flow::CAPABILITY);
            }
            Ok(enabled)
        }
    }
    d.deserialize_any(Capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_stream_null_unknown_and_escaped_values() {
        let message = Control::decode(br#"{"type":"open","capabilities":[null,"future","terminal-output-flow-v\u0031"],"extension":{"object":true}}"#).unwrap();
        assert!(message.output_flow);
        assert!(
            !Control::decode(br#"{"capabilities":null}"#)
                .unwrap()
                .output_flow
        );
        assert!(Control::decode(br#"{"capabilities":["terminal-output-flow-v1",42]}"#).is_none());
    }
    #[test]
    fn malformed_objects_ambiguous_fields_and_numbers_still_fail() {
        for raw in [
            r#"[]"#,
            r#"null"#,
            r#"{"session":["$1",42]}"#,
            r#"{"type":"open","type":"refresh"}"#,
            r#"{"session":{"id":"$1","id":"$2"}}"#,
            r#"{"cols":65536}"#,
            r#"{"received":9223372036854775808}"#,
        ] {
            assert!(Control::decode(raw.as_bytes()).is_none(), "accepted {raw}");
        }
        let message = Control::decode(
            br#"{"session":{"id":"$1","created_at":42,"future":true},"cols":null}"#,
        )
        .unwrap();
        assert!(message.session.is_valid());
        assert_eq!(message.cols, 0);
    }
}

//! Bounded shape walk without a Value tree, before the model's serde deserializer constructs a catalog.
//! The input frame is already bounded, and this pass counts retained Vec capacity and strings.
use super::{
    catalog_to_proto, Error, MAX_CATALOG_BYTES, MAX_CATALOG_SESSIONS, MAX_CATALOG_TAGS,
    MAX_CATALOG_TEXT, MAX_CATALOG_WINDOWS, MAX_DECODED_SNAPSHOT_BYTES, MAX_WORKFLOWS,
    MAX_WORKFLOW_NODES,
};
use crate::protobuf::types as p;
use hmux_model as m;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{cell::Cell, fmt, mem::size_of};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Catalog,
    Sessions,
    Session,
    Windows,
    Tags,
    Workflows,
    Workflow,
    Nodes,
    Node,
    Metrics,
    Generic,
}
impl Kind {
    fn key(self, key: &str) -> Self {
        match (self, key) {
            (Self::Catalog, "sessions") => Self::Sessions,
            (Self::Catalog, "host_metrics") => Self::Metrics,
            (Self::Sessions, _) => Self::Generic,
            (Self::Session, "window_names") => Self::Windows,
            (Self::Session, "tags") => Self::Tags,
            (Self::Session, "workflows") => Self::Workflows,
            (Self::Workflow, "nodes") => Self::Nodes,
            (_, _) => Self::Generic,
        }
    }
    fn element(self) -> Self {
        match self {
            Self::Sessions => Self::Session,
            Self::Workflows => Self::Workflow,
            Self::Nodes => Self::Node,
            _ => Self::Generic,
        }
    }
    fn count(self) -> usize {
        match self {
            Self::Sessions => MAX_CATALOG_SESSIONS,
            Self::Windows => MAX_CATALOG_WINDOWS,
            Self::Tags => MAX_CATALOG_TAGS,
            Self::Workflows => MAX_WORKFLOWS,
            Self::Nodes => MAX_WORKFLOW_NODES,
            _ => MAX_DECODED_SNAPSHOT_BYTES / 32,
        }
    }
    fn item_bytes(self) -> usize {
        match self {
            Self::Sessions => size_of::<m::Session>(),
            Self::Workflows => size_of::<m::Workflow>(),
            Self::Nodes => size_of::<m::WorkflowNode>(),
            Self::Windows | Self::Tags => size_of::<String>(),
            _ => size_of::<serde_json::Value>(),
        }
    }
}
#[derive(Clone, Copy)]
struct Seed<'a> {
    used: &'a Cell<usize>,
    kind: Kind,
    depth: u8,
}
impl Seed<'_> {
    fn charge<E: de::Error>(&self, bytes: usize) -> Result<(), E> {
        let Some(next) = self.used.get().checked_add(bytes) else {
            return Err(E::custom("catalog tree limit"));
        };
        if next > MAX_DECODED_SNAPSHOT_BYTES {
            return Err(E::custom("catalog tree limit"));
        }
        self.used.set(next);
        Ok(())
    }
    fn descend<E: de::Error>(&self, kind: Kind) -> Result<Self, E> {
        if self.depth >= 128 {
            return Err(E::custom("catalog nesting limit"));
        }
        Ok(Self {
            used: self.used,
            kind,
            depth: self.depth + 1,
        })
    }
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded catalog JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        self.deserialize(d)
    }
    fn visit_str<E: de::Error>(self, s: &str) -> Result<(), E> {
        let max = if matches!(self.kind, Kind::Metrics | Kind::Generic) {
            MAX_CATALOG_BYTES
        } else {
            MAX_CATALOG_TEXT
        };
        if s.len() > max {
            return Err(E::custom("catalog string limit"));
        }
        self.charge::<E>(s.len())
    }
    fn visit_borrowed_str<E: de::Error>(self, s: &'de str) -> Result<(), E> {
        self.visit_str(s)
    }
    fn visit_string<E: de::Error>(self, s: String) -> Result<(), E> {
        self.visit_str(&s)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut count = 0usize;
        while seq
            .next_element_seed(self.descend::<A::Error>(self.kind.element())?)?
            .is_some()
        {
            count += 1;
            if count > self.kind.count() {
                return Err(de::Error::custom("catalog list limit"));
            }
            self.charge::<A::Error>(self.kind.item_bytes().saturating_mul(2))?;
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut count = 0usize;
        while let Some(key) = map.next_key::<String>()? {
            let max = if matches!(self.kind, Kind::Metrics | Kind::Generic) {
                MAX_CATALOG_BYTES
            } else {
                MAX_CATALOG_TEXT
            };
            if key.len() > max {
                return Err(de::Error::custom("catalog key limit"));
            }
            count += 1;
            if count > MAX_DECODED_SNAPSHOT_BYTES / 64 {
                return Err(de::Error::custom("catalog map limit"));
            }
            self.charge::<A::Error>(key.len() + 64)?;
            map.next_value_seed(self.descend::<A::Error>(self.kind.key(&key))?)?;
        }
        Ok(())
    }
}

pub fn catalog_from_json(raw: &[u8]) -> Result<p::CatalogSnapshot, Error> {
    if raw.len() > MAX_CATALOG_BYTES {
        return Err(Error::Limit);
    }
    let used = Cell::new(size_of::<m::Catalog>());
    let mut decoder = serde_json::Deserializer::from_slice(raw);
    Seed {
        used: &used,
        kind: Kind::Catalog,
        depth: 0,
    }
    .deserialize(&mut decoder)
    .map_err(|e: serde_json::Error| {
        if e.classify() == serde_json::error::Category::Data {
            Error::Limit
        } else {
            Error::Invalid
        }
    })?;
    decoder.end().map_err(|_| Error::Invalid)?;
    let catalog: m::Catalog = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    catalog_to_proto(catalog)
}

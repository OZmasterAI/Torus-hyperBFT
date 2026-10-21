//! P0.0 phase 4: the genesis file digest a data dir is bound to.
//!
//! Every boot re-reads the genesis file and takes the chain config
//! (activations, timeouts, chain id, ...) from it, but the file is applied to
//! the DB only once. Without a binding, an edited file would silently change
//! consensus parameters on one node. The node stores a [`GenesisDigest`] at
//! the first boot of a data dir and refuses to start when a later boot's file
//! digests differently.
//!
//! **What is digested.** The file parsed as JSON (any valid JSON value, before
//! the typed [`crate::Genesis`] parse), written back in a canonical form:
//! compact (no whitespace), object keys sorted by their UTF-8 bytes, arrays in
//! file order, strings and numbers as `serde_json` writes them. So whitespace,
//! indentation and key order do not change the digest; any value change does,
//! including keys the typed parse ignores and an explicit value equal to a
//! default. The typed struct is not digested, so a newer binary with more
//! (defaulted) genesis fields computes the same digest for the same file.
//!
//! `digest = keccak256("torus-genesis-file-v1\0" || canonical bytes)`.
//!
//! **Field digests**, for the refusal message: one per top-level key, except
//! a top-level object with at most [`SPLIT_MAX_KEYS`] keys (`consensus`, `evm`,
//! `economics`), which gets one per key of it (`consensus.timeout_base_ms`).
//! Large maps (`evm_alloc`) stay one field.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, B256};
use serde_json::Value;

use crate::GenesisError;

/// Domain prefix of the whole-file digest.
const DIGEST_DOMAIN: &[u8] = b"torus-genesis-file-v1\0";

/// A top-level object with at most this many keys gets one field digest per
/// key; a larger one is a single field.
pub const SPLIT_MAX_KEYS: usize = 32;

/// Version byte of [`GenesisDigest::encode`].
const ENCODING_VERSION: u64 = 1;

/// Digest of a genesis file (whole-file digest plus field digests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisDigest {
    /// Digest of the whole canonical file.
    pub digest: B256,
    /// Field path (`chain_id`, `consensus.activations`, `evm_alloc`) to the
    /// digest of that value's canonical bytes.
    pub fields: BTreeMap<String, B256>,
}

/// The canonical bytes of `v`: compact JSON, object keys sorted.
pub fn canonical_json(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // A string serializes infallibly.
                out.extend(serde_json::to_vec(k).expect("serialize a JSON string"));
                out.push(b':');
                write_canonical(&map[k], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        scalar => out.extend(serde_json::to_vec(scalar).expect("serialize a JSON scalar")),
    }
}

impl GenesisDigest {
    /// Digest of a genesis file's text. `Err` when it is not JSON.
    pub fn of_json(json: &str) -> Result<Self, GenesisError> {
        let value: Value = serde_json::from_str(json)?;
        Ok(Self::of_value(&value))
    }

    /// Digest of a parsed genesis JSON value.
    pub fn of_value(value: &Value) -> Self {
        let canonical = canonical_json(value);
        let mut pre = Vec::with_capacity(DIGEST_DOMAIN.len() + canonical.len());
        pre.extend_from_slice(DIGEST_DOMAIN);
        pre.extend_from_slice(&canonical);
        let digest = keccak256(&pre);
        let mut fields = BTreeMap::new();
        match value {
            Value::Object(top) => {
                for (k, v) in top {
                    match v {
                        Value::Object(inner) if inner.len() <= SPLIT_MAX_KEYS => {
                            if inner.is_empty() {
                                fields.insert(k.clone(), keccak256(canonical_json(v)));
                            }
                            for (ik, iv) in inner {
                                fields.insert(format!("{k}.{ik}"), keccak256(canonical_json(iv)));
                            }
                        }
                        _ => {
                            fields.insert(k.clone(), keccak256(canonical_json(v)));
                        }
                    }
                }
            }
            // Not an object: the typed parse refuses it anyway.
            other => {
                fields.insert(String::new(), keccak256(canonical_json(other)));
            }
        }
        GenesisDigest { digest, fields }
    }

    /// The field paths whose values differ between `self` and `other`
    /// (changed, added or removed), sorted.
    pub fn changed_fields(&self, other: &GenesisDigest) -> Vec<String> {
        let mut changed: Vec<String> = self
            .fields
            .iter()
            .filter(|(k, d)| other.fields.get(*k) != Some(d))
            .map(|(k, _)| k.clone())
            .chain(
                other
                    .fields
                    .keys()
                    .filter(|k| !self.fields.contains_key(*k))
                    .cloned(),
            )
            .collect();
        changed.sort();
        changed.dedup();
        changed
    }

    /// The stored form: JSON `{"version":1,"digest":"0x..","fields":{..}}`.
    pub fn encode(&self) -> Vec<u8> {
        let fields: serde_json::Map<String, Value> = self
            .fields
            .iter()
            .map(|(k, d)| (k.clone(), Value::String(format!("{d:#x}"))))
            .collect();
        let v = serde_json::json!({
            "version": ENCODING_VERSION,
            "digest": format!("{:#x}", self.digest),
            "fields": fields,
        });
        canonical_json(&v)
    }

    /// Read [`Self::encode`]'s form back.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("stored genesis digest: {e}"))?;
        let version = v["version"].as_u64();
        if version != Some(ENCODING_VERSION) {
            return Err(format!(
                "stored genesis digest: unknown version {:?} (this build reads {ENCODING_VERSION})",
                v["version"]
            ));
        }
        let parse = |s: &Value| -> Result<B256, String> {
            s.as_str()
                .and_then(|s| s.parse::<B256>().ok())
                .ok_or_else(|| format!("stored genesis digest: bad hash {s}"))
        };
        let digest = parse(&v["digest"])?;
        let fields = v["fields"]
            .as_object()
            .ok_or("stored genesis digest: no fields")?
            .iter()
            .map(|(k, d)| Ok((k.clone(), parse(d)?)))
            .collect::<Result<_, String>>()?;
        Ok(GenesisDigest { digest, fields })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVNET: &str = include_str!("../../../devnet/genesis.json");

    fn devnet_value() -> Value {
        serde_json::from_str(DEVNET).unwrap()
    }

    /// Every object's keys in reverse order (`serde_json::Map` is a BTreeMap
    /// here, so the reversal is done on the text).
    fn reversed_keys_text(v: &Value) -> String {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                keys.reverse();
                let parts: Vec<String> = keys
                    .into_iter()
                    .map(|k| {
                        format!(
                            "{}  :  {}",
                            serde_json::to_string(k).unwrap(),
                            reversed_keys_text(&m[k])
                        )
                    })
                    .collect();
                format!("{{\n   {}\n}}", parts.join(" ,\n\t"))
            }
            Value::Array(a) => {
                let parts: Vec<String> = a.iter().map(reversed_keys_text).collect();
                format!("[ {} ]", parts.join(" , "))
            }
            s => serde_json::to_string(s).unwrap(),
        }
    }

    #[test]
    fn canonical_form_is_compact_with_sorted_keys() {
        let v: Value = serde_json::from_str(
            r#" { "b" : 1, "a" : [ true, null, "x\"y" ], "aa": {"z":0,"c":-2} } "#,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(canonical_json(&v)).unwrap(),
            r#"{"a":[true,null,"x\"y"],"aa":{"c":-2,"z":0},"b":1}"#
        );
    }

    #[test]
    fn formatting_and_key_order_do_not_change_the_digest() {
        let v = devnet_value();
        let file = GenesisDigest::of_json(DEVNET).unwrap();
        for text in [
            serde_json::to_string(&v).unwrap(),
            serde_json::to_string_pretty(&v).unwrap(),
            reversed_keys_text(&v),
        ] {
            assert_eq!(GenesisDigest::of_json(&text).unwrap(), file, "{text:.200}");
        }
    }

    #[test]
    fn a_changed_consensus_field_changes_the_digest_and_is_named() {
        let base = GenesisDigest::of_json(DEVNET).unwrap();
        let mut v = devnet_value();
        let t = v["consensus"]["timeout_base_ms"].as_u64().unwrap();
        v["consensus"]["timeout_base_ms"] = Value::from(t + 1);
        let changed = GenesisDigest::of_value(&v);
        assert_ne!(changed.digest, base.digest);
        assert_eq!(
            base.changed_fields(&changed),
            vec!["consensus.timeout_base_ms"]
        );

        let mut v = devnet_value();
        v["consensus"]["activations"] = serde_json::json!({ "running_state_hash": 10 });
        v["chain_name"] = Value::from("other");
        let changed = GenesisDigest::of_value(&v);
        assert_ne!(changed.digest, base.digest);
        assert_eq!(
            base.changed_fields(&changed),
            vec!["chain_name", "consensus.activations"]
        );
        // Removed keys are named too (the other direction).
        assert_eq!(
            changed.changed_fields(&base),
            vec!["chain_name", "consensus.activations"]
        );
    }

    #[test]
    fn large_maps_are_one_field() {
        let mut v = devnet_value();
        let alloc: serde_json::Map<String, Value> = (0..=SPLIT_MAX_KEYS)
            .map(|i| (format!("0x{i:040x}"), serde_json::json!({ "balance": "1" })))
            .collect();
        v["evm_alloc"] = Value::Object(alloc);
        let d = GenesisDigest::of_value(&v);
        assert!(d.fields.contains_key("evm_alloc"));
        assert!(!d.fields.keys().any(|k| k.starts_with("evm_alloc.")));
        assert!(d.fields.contains_key("consensus.epoch_length"));
    }

    #[test]
    fn encode_decode_round_trip() {
        let d = GenesisDigest::of_json(DEVNET).unwrap();
        assert_eq!(GenesisDigest::decode(&d.encode()).unwrap(), d);
        assert!(GenesisDigest::decode(b"{\"version\":2}").is_err());
        assert!(GenesisDigest::decode(b"not json").is_err());
    }

    #[test]
    fn not_json_is_an_error() {
        assert!(GenesisDigest::of_json("{ \"chain_id\": ").is_err());
    }
}

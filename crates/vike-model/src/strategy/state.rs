//! A strategy's durable STATE ([`StrategyState`]): what `Strategy::save_state` returns and
//! `Strategy::load_state` is handed, as msgpack WITH FIELD NAMES (`rmp_serde::to_vec_named`) of the
//! strategy's own serde types (decision 0296's ABI 15, the owner's "2 (б)" of 2026-11-19).
//!
//! **Why bytes, not a JSON document.** A live maker saves its state every second and after every fill or
//! order event, and the next quote waits behind the save: as a `serde_json::Value` it was built, formatted
//! as text, crossed a strategy file's ABI and the live pipe as text, and parsed back. As msgpack it is
//! encoded once from the strategy's own types, crosses both as the same bytes, and is written to the
//! state sidecar as they are. Floats keep their bits (a NaN survives), and field names keep it readable
//! by a reader that knows only some of the fields.
//!
//! **Old JSON states still read** ([`StrategyState::from_json_value`]): a sidecar written before is a JSON
//! document, and its msgpack encoding reads into the strategy's types as the document did, so no state is
//! migrated (the owner's rule: a new format reads the old one).

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

/// A decoder's error in words, its depth limit named as [`StrategyState::MAX_DEPTH`].
fn words(e: rmp_serde::decode::Error) -> String {
    match e {
        rmp_serde::decode::Error::DepthLimitExceeded => {
            format!("it nests deeper than {} levels", StrategyState::MAX_DEPTH)
        }
        e => e.to_string(),
    }
}

/// A strategy's durable state: msgpack with field names of the strategy's own serde types. Built by
/// [`StrategyState::of`], read back by [`StrategyState::read`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrategyState(Vec<u8>);

impl StrategyState {
    /// How deep a state may nest (arrays and maps inside one another); [`StrategyState::check`] and
    /// [`StrategyState::read`] refuse a deeper one in words. A strategy's state never nests this deep, and
    /// the scan recurses once a level on the reader's stack: msgpack's own limit (1024) overflows a 2 MiB
    /// thread stack in a debug build, and an overflow on the tradehub's reader thread kills the tradehub.
    pub const MAX_DEPTH: usize = 128;

    /// The state `value` encodes to (msgpack with field names). `Err` in words for a value msgpack
    /// cannot hold (a map whose key will not serialize).
    pub fn of<T: Serialize + ?Sized>(value: &T) -> Result<StrategyState, String> {
        rmp_serde::to_vec_named(value)
            .map(StrategyState)
            .map_err(|e| format!("the strategy state does not encode: {e}"))
    }

    /// The state read into the strategy's own type `T`; `Err` in words for bytes that are not one, or that
    /// nest deeper than [`StrategyState::MAX_DEPTH`] ([`StrategyState::check`] refuses those too, so the
    /// two agree).
    pub fn read<T: DeserializeOwned>(&self) -> Result<T, String> {
        let mut decoder = rmp_serde::Deserializer::from_read_ref(&self.0);
        decoder.set_max_depth(Self::MAX_DEPTH + 1);
        T::deserialize(&mut decoder)
            .map_err(|e| format!("the strategy state does not read: {}", words(e)))
    }

    /// The state whose encoding is `bytes`, as a save wrote them (a strategy file's answer across its
    /// ABI, a sandboxed mount's pipe, a sidecar), taken as they are. [`StrategyState::check`] says whether
    /// they are one msgpack value.
    pub fn from_bytes(bytes: Vec<u8>) -> StrategyState {
        StrategyState(bytes)
    }

    /// Its encoding.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Its encoding, owned.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Whether `bytes` are exactly one msgpack value, nothing after it, nested at most
    /// [`StrategyState::MAX_DEPTH`] deep; `Err` in words otherwise. A scan, no document built: what a reader
    /// of the state checks once, as it arrives.
    pub fn check(bytes: &[u8]) -> Result<(), String> {
        let mut reader = bytes;
        let mut decoder = rmp_serde::Deserializer::new(&mut reader);
        decoder.set_max_depth(Self::MAX_DEPTH + 1);
        IgnoredAny::deserialize(&mut decoder)
            .map_err(|e| format!("the strategy state is not msgpack: {}", words(e)))?;
        match reader.len() {
            0 => Ok(()),
            n => Err(format!("the strategy state is not msgpack: {n} bytes after its value")),
        }
    }

    /// The state an OLD JSON document holds (a sidecar written before states were msgpack): its msgpack
    /// encoding, which reads into the strategy's types as the document did.
    pub fn from_json_value(value: &serde_json::Value) -> StrategyState {
        StrategyState::of(value).expect("a JSON document always encodes as msgpack")
    }

    /// The state as a JSON document, for a record that keeps JSON (the journal's emulated orders);
    /// `Err` in words for bytes that are not msgpack.
    pub fn to_json_value(&self) -> Result<serde_json::Value, String> {
        self.read()
    }
}

/// A state crosses a serde message (the live strategy pipe) as its BYTES (a msgpack `bin`), never re-encoded.
impl Serialize for StrategyState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for StrategyState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> serde::de::Visitor<'de> for Bytes {
            type Value = StrategyState;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a strategy state's bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<StrategyState, E> {
                Ok(StrategyState(bytes.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(
                self,
                bytes: Vec<u8>,
            ) -> Result<StrategyState, E> {
                Ok(StrategyState(bytes))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<StrategyState, A::Error> {
                // The hint is the wire's claim (a 5-byte array header can claim 4 G elements): capped, as
                // serde's own `Vec` caps it.
                let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(64 * 1024));
                while let Some(byte) = seq.next_element()? {
                    bytes.push(byte);
                }
                Ok(StrategyState(bytes))
            }
        }
        deserializer.deserialize_byte_buf(Bytes)
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

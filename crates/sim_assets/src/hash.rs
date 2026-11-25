//! Content hashes of model files: BLAKE3 of the file's bytes, written `blake3:<64 hex digits>`
//! (subsystems §5.10). One file has one hash, which is why a `.gltf` with separate files is packed
//! into one `.glb` on import.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// BLAKE3 of a model file's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// The hash of these bytes.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// The 64 hex digits.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The first 8 hex digits, for labels.
    pub fn short(&self) -> String {
        self.hex()[..8].to_owned()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3:{}", self.hex())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3:{}…", self.short())
    }
}

/// Why a hash did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadHash(pub String);

impl fmt::Display for BadHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a content hash (blake3: and 64 hex digits)",
            self.0
        )
    }
}

impl std::error::Error for BadHash {}

impl FromStr for ContentHash {
    type Err = BadHash;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadHash(s.to_owned());
        let hex = s.strip_prefix("blake3:").ok_or_else(bad)?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(Self(out))
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

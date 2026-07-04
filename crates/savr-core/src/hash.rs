use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

/// A blake3 content hash. Serialized as a lowercase hex string in JSON
/// (PRD-05 §1: "hex in JSON").
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blake3Hash(pub [u8; 32]);

impl Blake3Hash {
    /// Hash a byte slice.
    pub fn of(bytes: &[u8]) -> Self {
        Blake3Hash(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }

    pub fn from_hex(s: &str) -> Result<Self, Error> {
        let h = blake3::Hash::from_hex(s).map_err(|e| Error::Hash(e.to_string()))?;
        Ok(Blake3Hash(*h.as_bytes()))
    }
}

impl std::fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blake3Hash({})", self.to_hex())
    }
}

impl Serialize for Blake3Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Blake3Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Blake3Hash::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_json() {
        let h = Blake3Hash::of(b"hello");
        assert_eq!(h, Blake3Hash::from_hex(&h.to_hex()).unwrap());
        // JSON is a bare hex string, not a byte array.
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.starts_with('"') && json.len() == 66);
        assert_eq!(h, serde_json::from_str(&json).unwrap());
    }
}

//! 内容哈希与十六进制指纹（`SPEC-domain.md` §2）。
//!
//! - [`ContentHash`]：BLAKE3-256，64 位小写十六进制；由字节内容决定，不可伪造。
//! - [`AssetId`]：内容寻址资产 id，即内容哈希（同字节 → 同 `AssetId`）。
//! - [`Sha256Hex`]：SHA-256 的 64 位小写十六进制，用于提示词/输入/配置指纹。
//!
//! 解析严格：必须 64 位、必须小写十六进制；不接受大写、短写或带 `0x` 前缀。
//! 哈希计算是纯 CPU 运算（无 IO），因此在本模块提供 `of` 构造器，
//! 保证「资产即内容」的不变量只有一条构造路径。

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::HashError;

/// 哈希十六进制文本长度（32 字节 → 64 个十六进制字符）。
pub const HASH_HEX_LEN: usize = 64;

/// BLAKE3-256 内容哈希（64 位小写十六进制）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// 字节长度。
    pub const LEN: usize = 32;

    /// 计算字节内容的 BLAKE3-256 哈希。
    ///
    /// 规范 §2：`ContentHash` 由字节内容决定，不可伪造；同字节 → 同哈希。
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// 从 64 位小写十六进制文本解析。
    ///
    /// # Errors
    ///
    /// 长度不是 64 或含非小写十六进制字符时返回 [`HashError::InvalidHex`]。
    pub fn from_hex(text: &str) -> Result<Self, HashError> {
        decode_hex_32(text, "ContentHash").map(Self)
    }

    /// 规范小写十六进制文本。
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }

    /// 32 字节原始值。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode_hex(&self.0))
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ContentHash")
            .field(&encode_hex(&self.0))
            .finish()
    }
}

impl FromStr for ContentHash {
    type Err = HashError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_hex(text)
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// 内容寻址资产 id：`AssetId` 即内容哈希（规范 §2「资产即内容」）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(ContentHash);

impl AssetId {
    /// 由字节内容计算资产 id（同字节 → 同 `AssetId`）。
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(ContentHash::of(bytes))
    }

    /// 由内容哈希构造。
    #[must_use]
    pub const fn from_content_hash(hash: ContentHash) -> Self {
        Self(hash)
    }

    /// 取出内容哈希。
    #[must_use]
    pub const fn content_hash(self) -> ContentHash {
        self.0
    }

    /// 从 64 位小写十六进制文本解析。
    ///
    /// # Errors
    ///
    /// 文本非法时返回 [`HashError::InvalidHex`]。
    pub fn from_hex(text: &str) -> Result<Self, HashError> {
        ContentHash::from_hex(text).map(Self)
    }

    /// 规范小写十六进制文本。
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AssetId").field(&self.0.to_hex()).finish()
    }
}

impl FromStr for AssetId {
    type Err = HashError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_hex(text)
    }
}

impl Serialize for AssetId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_hex())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// SHA-256 的 64 位小写十六进制指纹（提示词/输入/配置哈希）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Hex([u8; 32]);

impl Sha256Hex {
    /// 字节长度。
    pub const LEN: usize = 32;

    /// 计算字节内容的 SHA-256。
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(bytes);
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&digest[..]);
        Self(raw)
    }

    /// 从 64 位小写十六进制文本解析。
    ///
    /// # Errors
    ///
    /// 长度不是 64 或含非小写十六进制字符时返回 [`HashError::InvalidHex`]。
    pub fn from_hex(text: &str) -> Result<Self, HashError> {
        decode_hex_32(text, "Sha256Hex").map(Self)
    }

    /// 规范小写十六进制文本。
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }

    /// 32 字节原始值。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode_hex(&self.0))
    }
}

impl fmt::Debug for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Sha256Hex")
            .field(&encode_hex(&self.0))
            .finish()
    }
}

impl FromStr for Sha256Hex {
    type Err = HashError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_hex(text)
    }
}

impl Serialize for Sha256Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for Sha256Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

fn decode_hex_32(text: &str, kind: &'static str) -> Result<[u8; 32], HashError> {
    if text.len() != HASH_HEX_LEN {
        return Err(HashError::InvalidHex {
            kind,
            reason: "长度必须为 64 个字符",
        });
    }
    let mut raw = [0u8; 32];
    let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty(), "长度已在前面校验为 64");
    for (index, pair) in pairs.iter().enumerate() {
        let high = hex_digit(pair[0]).ok_or(HashError::InvalidHex {
            kind,
            reason: "只允许 0-9 与 a-f 小写十六进制字符",
        })?;
        let low = hex_digit(pair[1]).ok_or(HashError::InvalidHex {
            kind,
            reason: "只允许 0-9 与 a-f 小写十六进制字符",
        })?;
        raw[index] = (high << 4) | low;
    }
    Ok(raw)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn content_hash_of_empty_input_matches_known_vector() {
        // BLAKE3 官方测试向量：空输入哈希（来源：BLAKE3 规范测试向量）。
        assert_eq!(ContentHash::of(b"").to_hex(), EMPTY_BLAKE3);
    }

    #[test]
    fn sha256_of_empty_input_matches_known_vector() {
        // SHA-256 官方测试向量：空输入哈希（FIPS 180-4）。
        assert_eq!(Sha256Hex::of(b"").to_hex(), EMPTY_SHA256);
    }

    #[test]
    fn asset_id_is_content_addressed() {
        let a = AssetId::of(b"same bytes");
        let b = AssetId::of(b"same bytes");
        let c = AssetId::of(b"other bytes");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.content_hash(), ContentHash::of(b"same bytes"));
        assert_eq!(a.to_hex(), a.content_hash().to_hex());
    }

    #[test]
    fn hex_roundtrip() {
        let hash = ContentHash::of(b"x");
        let text = hash.to_hex();
        assert_eq!(ContentHash::from_hex(&text).unwrap(), hash);
        let sha = Sha256Hex::of(b"x");
        assert_eq!(Sha256Hex::from_hex(&sha.to_hex()).unwrap(), sha);
        assert_eq!(
            AssetId::from_hex(&text).unwrap(),
            AssetId::from_content_hash(hash)
        );
    }

    #[test]
    fn rejects_bad_hex() {
        for bad in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(63)] {
            assert!(
                matches!(
                    ContentHash::from_hex(bad),
                    Err(HashError::InvalidHex { .. })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            Sha256Hex::from_hex("0xabc"),
            Err(HashError::InvalidHex { .. })
        ));
    }

    #[test]
    fn serde_is_lowercase_hex_string() {
        let hash = ContentHash::of(b"payload");
        let json = serde_json::to_string(&hash).unwrap();
        assert_eq!(json, format!("\"{}\"", hash.to_hex()));
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hash);
        assert!(serde_json::from_str::<AssetId>("\"short\"").is_err());
        let sha = Sha256Hex::of(b"payload");
        let json = serde_json::to_string(&sha).unwrap();
        assert_eq!(serde_json::from_str::<Sha256Hex>(&json).unwrap(), sha);
    }

    #[test]
    fn debug_prints_hex() {
        let hash = ContentHash::of(b"");
        assert!(format!("{hash:?}").contains(EMPTY_BLAKE3));
        assert!(format!("{:?}", AssetId::of(b"")).starts_with("AssetId("));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：任意字节内容 → 哈希 → 十六进制 → 解析回同值。
        #[test]
        fn prop_content_hash_hex_roundtrip(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let hash = ContentHash::of(&bytes);
            let text = hash.to_hex();
            proptest::prop_assert_eq!(text.len(), 64);
            proptest::prop_assert_eq!(ContentHash::from_hex(&text).unwrap(), hash);
            let sha = Sha256Hex::of(&bytes);
            proptest::prop_assert_eq!(Sha256Hex::from_hex(&sha.to_hex()).unwrap(), sha);
        }

        /// 性质：不同内容不产生相同资产 id（碰撞在此样本空间内不存在）。
        #[test]
        fn prop_different_bytes_different_asset(a in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64), b in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64)) {
            proptest::prop_assume!(a != b);
            proptest::prop_assert_ne!(AssetId::of(&a), AssetId::of(&b));
        }
    }
}

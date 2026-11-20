//! Just enough bencode to read a `.torrent`: the info-hash and the file list.

use sha1::{Digest, Sha1};

#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    /// Keys in file order, with the raw bytes of each value (needed for the info-hash).
    Dict(Vec<(&'a [u8], Value<'a>, &'a [u8])>),
}

#[derive(Debug, thiserror::Error)]
#[error("broken .torrent file at byte {0}")]
pub struct BencodeError(usize);

impl<'a> Value<'a> {
    pub fn get(&self, key: &str) -> Option<&Value<'a>> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _, _)| *k == key.as_bytes()).map(|(_, v, _)| v),
            _ => None,
        }
    }

    fn raw(&self, key: &str) -> Option<&'a [u8]> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _, _)| *k == key.as_bytes()).map(|(_, _, r)| *r),
            _ => None,
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<String> {
        match self {
            Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }
}

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self) -> BencodeError {
        BencodeError(self.pos)
    }

    fn until(&mut self, end: u8) -> Result<&'a [u8], BencodeError> {
        let start = self.pos;
        let len = self.buf[start..].iter().position(|&b| b == end).ok_or(self.err())?;
        self.pos = start + len + 1;
        Ok(&self.buf[start..start + len])
    }

    fn number(s: &[u8]) -> Option<i64> {
        std::str::from_utf8(s).ok()?.parse().ok()
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, BencodeError> {
        if depth > 64 {
            return Err(self.err());
        }
        match *self.buf.get(self.pos).ok_or(self.err())? {
            b'i' => {
                self.pos += 1;
                let n = self.until(b'e')?;
                Self::number(n).map(Value::Int).ok_or(self.err())
            }
            b'l' => {
                self.pos += 1;
                let mut list = Vec::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    list.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(list))
            }
            b'd' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    let Value::Bytes(key) = self.bytes()? else {
                        unreachable!()
                    };
                    let start = self.pos;
                    let v = self.value(depth + 1)?;
                    items.push((key, v, &self.buf[start..self.pos]));
                }
                self.pos += 1;
                Ok(Value::Dict(items))
            }
            b'0'..=b'9' => self.bytes(),
            _ => Err(self.err()),
        }
    }

    fn bytes(&mut self) -> Result<Value<'a>, BencodeError> {
        let len = Self::number(self.until(b':')?).ok_or(self.err())? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|&e| e <= self.buf.len())
            .ok_or(self.err())?;
        let b = &self.buf[self.pos..end];
        self.pos = end;
        Ok(Value::Bytes(b))
    }
}

pub fn decode(buf: &[u8]) -> Result<Value<'_>, BencodeError> {
    Parser { buf, pos: 0 }.value(0)
}

/// A file inside a torrent, in torrent order.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentFile {
    /// Path inside the torrent, `/`-separated, without the top folder.
    pub path: String,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TorrentMeta {
    pub infohash: String,
    pub name: String,
    pub files: Vec<TorrentFile>,
}

/// Reads the info-hash, name and files of a `.torrent`.
pub fn parse_torrent(buf: &[u8]) -> Result<TorrentMeta, BencodeError> {
    let root = decode(buf)?;
    let raw = root.raw("info").ok_or(BencodeError(0))?;
    let info = root.get("info").ok_or(BencodeError(0))?;
    let infohash = hex::encode(Sha1::digest(raw));
    let name = info.get("name").and_then(Value::str).unwrap_or_default();
    let files = match info.get("files") {
        Some(Value::List(list)) => list
            .iter()
            .map(|f| TorrentFile {
                path: match f.get("path") {
                    Some(Value::List(parts)) => parts.iter().filter_map(Value::str).collect::<Vec<_>>().join("/"),
                    _ => String::new(),
                },
                length: f.get("length").and_then(Value::int).unwrap_or(0).max(0) as u64,
            })
            .collect(),
        _ => vec![TorrentFile {
            path: name.clone(),
            length: info.get("length").and_then(Value::int).unwrap_or(0).max(0) as u64,
        }],
    };
    Ok(TorrentMeta { infohash, name, files })
}

/// Info-hash from a magnet link (hex or base32 form), lowercase hex.
pub fn magnet_infohash(magnet: &str) -> Option<String> {
    let start = magnet.find("xt=urn:btih:")? + "xt=urn:btih:".len();
    let h: String = magnet[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    match h.len() {
        40 if h.chars().all(|c| c.is_ascii_hexdigit()) => Some(h.to_lowercase()),
        32 => base32(&h).map(hex::encode),
        _ => None,
    }
}

fn base32(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c.to_ascii_uppercase() {
            b @ b'A'..=b'Z' => b - b'A',
            b @ b'2'..=b'7' => b - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a tiny multi-file torrent by hand.
    pub fn sample() -> Vec<u8> {
        b"d8:announce3:url4:infod5:filesld6:lengthi100e4:pathl3:S016:e1.mkveed6:lengthi200e4:pathl3:S016:e2.mkveee4:name4:Show12:piece lengthi16384e6:pieces0:ee".to_vec()
    }

    #[test]
    fn reads_torrent() {
        let buf = sample();
        let meta = parse_torrent(&buf).unwrap();
        assert_eq!(meta.name, "Show");
        assert_eq!(meta.files.len(), 2);
        assert_eq!(meta.files[1].path, "S01/e2.mkv");
        assert_eq!(meta.files[1].length, 200);
        // The hash covers exactly the bytes of the info dict.
        let start = buf.windows(6).position(|w| w == b"4:info").unwrap() + 6;
        let raw = &buf[start..buf.len() - 1];
        assert_eq!(meta.infohash, hex::encode(Sha1::digest(raw)));
        assert!(parse_torrent(b"d4:infoi1e").is_err());
        assert!(parse_torrent(b"d3:abc99999:x").is_err());
    }

    #[test]
    fn magnets() {
        let hex = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        assert_eq!(
            magnet_infohash(&format!("magnet:?xt=urn:btih:{}&dn=x", hex.to_uppercase())).as_deref(),
            Some(hex)
        );
        assert_eq!(
            magnet_infohash("magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK").as_deref(),
            Some(hex)
        );
        assert_eq!(magnet_infohash("magnet:?dn=x"), None);
    }
}

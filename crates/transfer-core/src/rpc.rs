//! Bounded directory and GET RPC payloads on Transfer's Control QUIC.
//! No NAT/TLS implementation here. Remote access is always scoped to an
//! explicitly granted capability root, never a process-wide filesystem.

use std::{ffi::OsString, path::Path};

use crate::secure_io::{FileAccessError, SharedRoot};
use crate::protocol::MAX_PAYLOAD;

const MAX_PATH: usize = 4096;
const LIST_OP: u8 = 1;
const GET_OP: u8 = 2;
const MAX_ENTRIES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcRequest {
    List { directory: String },
    Get { source: String, destination: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RpcError {
    InvalidFormat,
    InvalidPath,
    Oversized,
    Utf8,
    Filesystem(FileAccessError),
}

fn push_string(buf: &mut Vec<u8>, string: &str) -> Result<(), RpcError> {
    if string.len() > MAX_PATH || string.contains('\0') {
        return Err(RpcError::InvalidPath);
    }
    buf.extend_from_slice(&(string.len() as u16).to_be_bytes());
    buf.extend_from_slice(string.as_bytes());
    Ok(())
}

fn read_string<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<&'a str, RpcError> {
    let header = bytes.get(*pos..*pos + 2).ok_or(RpcError::InvalidFormat)?;
    let size = u16::from_be_bytes([header[0], header[1]]) as usize;
    *pos += 2;
    if size > MAX_PATH { return Err(RpcError::InvalidPath); }
    let data = bytes.get(*pos..*pos + size).ok_or(RpcError::InvalidFormat)?;
    *pos += size;
    let s = std::str::from_utf8(data).map_err(|_| RpcError::Utf8)?;
    if s.contains('\0') { return Err(RpcError::InvalidPath); }
    Ok(s)
}

impl RpcRequest {
    pub fn encode(&self) -> Result<Vec<u8>, RpcError> {
        let mut bytes = Vec::new();
        match self {
            Self::List { directory } => {
                bytes.push(LIST_OP);
                push_string(&mut bytes, directory)?;
            }
            Self::Get { source, destination } => {
                bytes.push(GET_OP);
                push_string(&mut bytes, source)?;
                push_string(&mut bytes, destination)?;
            }
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, RpcError> {
        if bytes.is_empty() || bytes.len() > MAX_PAYLOAD {
            return Err(RpcError::InvalidFormat);
        }
        let mut pos = 1;
        let result = match bytes[0] {
            LIST_OP => Self::List { directory: read_string(bytes, &mut pos)?.to_owned() },
            GET_OP => Self::Get {
                source: read_string(bytes, &mut pos)?.to_owned(),
                destination: read_string(bytes, &mut pos)?.to_owned(),
            },
            _ => return Err(RpcError::InvalidFormat),
        };
        if pos != bytes.len() { return Err(RpcError::InvalidFormat); }
        Ok(result)
    }
}

/// Format one safe directory listing; never resolves outside SharedRoot.
/// Empty string lists the authorized root.
pub fn list_in_root(root: &SharedRoot, path: &str) -> Result<Vec<String>, RpcError> {
    let relative = if path.is_empty() { None } else { Some(Path::new(path)) };
    let names = root.list(relative).map_err(RpcError::Filesystem)?;
    if names.len() > MAX_ENTRIES { return Err(RpcError::Oversized); }
    names.into_iter().map(|name: OsString| name.into_string()
        .map_err(|_| RpcError::Utf8)).collect()
}

pub fn authorize_get(root: &SharedRoot, source: &str) -> Result<(), RpcError> {
    root.open_read(Path::new(source)).map(|_| ()).map_err(RpcError::Filesystem)
}

/// Count-prefixed strings prevent ambiguous names with newlines.
pub fn encode_listing(names: &[String]) -> Result<Vec<u8>, RpcError> {
    if names.len() > MAX_ENTRIES { return Err(RpcError::Oversized); }
    let mut buf = Vec::new();
    buf.extend_from_slice(&(names.len() as u16).to_be_bytes());
    for name in names {
        if name.len() > MAX_PATH { return Err(RpcError::Oversized); }
        push_string(&mut buf, name)?;
        if buf.len() > MAX_PAYLOAD { return Err(RpcError::Oversized); }
    }
    Ok(buf)
}

pub fn decode_listing(data: &[u8]) -> Result<Vec<String>, RpcError> {
    if data.len() < 2 || data.len() > MAX_PAYLOAD { return Err(RpcError::InvalidFormat); }
    let count = u16::from_be_bytes([data[0], data[1]]) as usize;
    if count > MAX_ENTRIES { return Err(RpcError::Oversized); }
    let mut pos = 2;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(read_string(data, &mut pos)?.to_owned());
    }
    if pos != data.len() { return Err(RpcError::InvalidFormat); }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrips_list_get_unicode_and_newline_names() {
        for r in [
            RpcRequest::List { directory: "子目录".into() },
            RpcRequest::Get { source: "foo bar.txt".into(), destination: "远端/文件.txt".into() },
        ] {
            assert_eq!(RpcRequest::decode(&r.encode().unwrap()), Ok(r));
        }
        let names = vec!["Hello world".into(), "文件\n带换行".into()];
        assert_eq!(decode_listing(&encode_listing(&names).unwrap()), Ok(names));
    }
    #[test]
    fn rejects_corrupt_oversized_and_embedded_nul() {
        assert_eq!(RpcRequest::decode(&[]), Err(RpcError::InvalidFormat));
        assert_eq!(RpcRequest::decode(&[99, 0, 0]), Err(RpcError::InvalidFormat));
        assert_eq!(RpcRequest::List { directory: "bad\0path".into() }.encode(), Err(RpcError::InvalidPath));
        let mut sample = RpcRequest::List { directory: "foo".into() }.encode().unwrap();
        sample.push(1);
        assert_eq!(RpcRequest::decode(&sample), Err(RpcError::InvalidFormat));
        assert_eq!(decode_listing(&[0, 1, 0, 10, b'x']), Err(RpcError::InvalidFormat));
    }
}

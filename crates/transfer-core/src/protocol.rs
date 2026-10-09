//! Bounded application-control frames for the new Rust-only wire protocol.
//!
//! Framing is deliberately independent of transport. Callers must use
//! authenticated p2p-sdk sessions and MUST limit buffered input; this codec
//! does not establish peer identity or implement file transfer.
//!
//! Wire format, v1 (big-endian):
//!   magic[4] version[2] kind[2] request_id[8] payload_len[4] payload[N]
//! No backward compatibility with the Go p2p-friend wire protocol.

pub const MAGIC: [u8; 4] = *b"P2PT";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 20;
pub const MAX_PAYLOAD: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum FrameKind {
    Hello = 1,
    RpcRequest = 2,
    RpcResponse = 3,
    TransferControl = 4,
    Ack = 5,
    Cancel = 6,
    Error = 7,
    Bye = 8,
}

impl TryFrom<u16> for FrameKind {
    type Error = CodecError;

    fn try_from(value: u16) -> Result<Self, CodecError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::RpcRequest),
            3 => Ok(Self::RpcResponse),
            4 => Ok(Self::TransferControl),
            5 => Ok(Self::Ack),
            6 => Ok(Self::Cancel),
            7 => Ok(Self::Error),
            8 => Ok(Self::Bye),
            _ => Err(CodecError::UnknownKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    Truncated,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind,
    OversizedPayload,
    InvalidLength,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(CodecError::OversizedPayload);
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_be_bytes());
        out.extend_from_slice(&(self.kind as u16).to_be_bytes());
        out.extend_from_slice(&self.request_id.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decode exactly one complete frame. A stream reader should inspect
    /// the fixed-size header first, reject oversized lengths, then allocate
    /// only the declared bounded payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.len() < HEADER_LEN {
            return Err(CodecError::Truncated);
        }
        if bytes[..4] != MAGIC {
            return Err(CodecError::InvalidMagic);
        }
        let version = u16::from_be_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(CodecError::UnsupportedVersion);
        }
        let kind = FrameKind::try_from(u16::from_be_bytes([bytes[6], bytes[7]]))?;
        let request_id = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
        let payload_len = u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if payload_len > MAX_PAYLOAD {
            return Err(CodecError::OversizedPayload);
        }
        if bytes.len() != HEADER_LEN + payload_len {
            return Err(CodecError::InvalidLength);
        }
        Ok(Self {
            kind,
            request_id,
            payload: bytes[HEADER_LEN..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_control_frame() {
        let frame = Frame {
            kind: FrameKind::RpcRequest,
            request_id: 0x1020_3040_5060_7080,
            payload: b"hello".to_vec(),
        };
        let wire = frame.encode().unwrap();
        assert_eq!(wire.len(), HEADER_LEN + 5);
        assert_eq!(wire[..4], MAGIC);
        assert_eq!(Frame::decode(&wire), Ok(frame));
    }

    #[test]
    fn accepts_empty_payload() {
        let frame = Frame {
            kind: FrameKind::Bye,
            request_id: 0,
            payload: Vec::new(),
        };
        assert_eq!(Frame::decode(&frame.encode().unwrap()), Ok(frame));
    }

    #[test]
    fn rejects_oversized_frame_before_allocating_payload() {
        let mut header = Frame {
            kind: FrameKind::Hello,
            request_id: 1,
            payload: Vec::new(),
        }
        .encode()
        .unwrap();
        header[16..20].copy_from_slice(&((MAX_PAYLOAD as u32) + 1).to_be_bytes());
        assert_eq!(Frame::decode(&header), Err(CodecError::OversizedPayload));
        let oversized = Frame {
            kind: FrameKind::Hello,
            request_id: 1,
            payload: vec![0_u8; MAX_PAYLOAD + 1],
        };
        assert_eq!(oversized.encode(), Err(CodecError::OversizedPayload));
    }

    #[test]
    fn rejects_truncation_extra_bytes_and_unknown_type() {
        let frame = Frame {
            kind: FrameKind::Ack,
            request_id: 2,
            payload: vec![1, 2, 3],
        };
        let mut wire = frame.encode().unwrap();
        assert_eq!(Frame::decode(&wire[..12]), Err(CodecError::Truncated));
        wire.push(0);
        assert_eq!(Frame::decode(&wire), Err(CodecError::InvalidLength));
        wire.pop();
        wire[6..8].copy_from_slice(&99_u16.to_be_bytes());
        assert_eq!(Frame::decode(&wire), Err(CodecError::UnknownKind));
    }

    #[test]
    fn rejects_unknown_version_and_magic() {
        let frame = Frame {
            kind: FrameKind::Hello,
            request_id: 2,
            payload: Vec::new(),
        };
        let mut wire = frame.encode().unwrap();
        wire[4..6].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(Frame::decode(&wire), Err(CodecError::UnsupportedVersion));
        wire[4..6].copy_from_slice(&VERSION.to_be_bytes());
        wire[0] = b'X';
        assert_eq!(Frame::decode(&wire), Err(CodecError::InvalidMagic));
    }
}

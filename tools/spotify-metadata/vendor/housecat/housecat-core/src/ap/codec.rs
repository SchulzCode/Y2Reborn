//! Shannon-encrypted framing for the access-point socket (spec section 5.1, step 4).
//!
//! Frame: `cmd u8 | len u16 BE | payload | mac [u8; 4]`. Each direction keeps its own
//! cipher state and a u32 nonce that increments once per frame.

use std::io;

use bytes::{BufMut, Bytes, BytesMut};
use shannon::Shannon;
use tokio_util::codec::{Decoder, Encoder};

const HEADER_SIZE: usize = 3;
const MAC_SIZE: usize = 4;

enum DecodeState {
    Header,
    Payload { cmd: u8, len: usize },
}

pub struct ShannonCodec {
    encode_nonce: u32,
    encode_cipher: Shannon,
    decode_nonce: u32,
    decode_cipher: Shannon,
    decode_state: DecodeState,
}

impl std::fmt::Debug for ShannonCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShannonCodec")
            .field("encode_nonce", &self.encode_nonce)
            .field("decode_nonce", &self.decode_nonce)
            .finish_non_exhaustive()
    }
}

impl ShannonCodec {
    pub fn new(send_key: &[u8], recv_key: &[u8]) -> Self {
        ShannonCodec {
            encode_nonce: 0,
            encode_cipher: Shannon::new(send_key),
            decode_nonce: 0,
            decode_cipher: Shannon::new(recv_key),
            decode_state: DecodeState::Header,
        }
    }
}

impl Encoder<(u8, Vec<u8>)> for ShannonCodec {
    type Error = io::Error;

    fn encode(&mut self, (cmd, payload): (u8, Vec<u8>), dst: &mut BytesMut) -> io::Result<()> {
        let len = u16::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload exceeds 65535 bytes"))?;

        let start = dst.len();
        dst.reserve(HEADER_SIZE + payload.len() + MAC_SIZE);
        dst.put_u8(cmd);
        dst.put_u16(len);
        dst.put_slice(&payload);

        self.encode_cipher.nonce_u32(self.encode_nonce);
        self.encode_nonce = self.encode_nonce.wrapping_add(1);
        self.encode_cipher.encrypt(&mut dst[start..]);

        let mut mac = [0u8; MAC_SIZE];
        self.encode_cipher.finish(&mut mac);
        dst.put_slice(&mac);
        Ok(())
    }
}

impl Decoder for ShannonCodec {
    type Item = (u8, Bytes);
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<(u8, Bytes)>> {
        if let DecodeState::Header = self.decode_state {
            if src.len() < HEADER_SIZE {
                return Ok(None);
            }
            let mut header = [0u8; HEADER_SIZE];
            header.copy_from_slice(&src.split_to(HEADER_SIZE));

            self.decode_cipher.nonce_u32(self.decode_nonce);
            self.decode_nonce = self.decode_nonce.wrapping_add(1);
            self.decode_cipher.decrypt(&mut header);

            let cmd = header[0];
            let len = u16::from_be_bytes([header[1], header[2]]) as usize;
            self.decode_state = DecodeState::Payload { cmd, len };
        }

        if let DecodeState::Payload { cmd, len } = self.decode_state {
            if src.len() < len + MAC_SIZE {
                return Ok(None);
            }
            self.decode_state = DecodeState::Header;

            let mut payload = src.split_to(len + MAC_SIZE);
            let mac = payload.split_off(len);
            self.decode_cipher.decrypt(&mut payload);
            self.decode_cipher.check_mac(&mac)?;
            return Ok(Some((cmd, payload.freeze())));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (ShannonCodec, ShannonCodec) {
        let key_a = [1u8; 32];
        let key_b = [2u8; 32];
        (ShannonCodec::new(&key_a, &key_b), ShannonCodec::new(&key_b, &key_a))
    }

    #[test]
    fn round_trips_frames_in_order() {
        let (mut client, mut server) = pair();
        let mut wire = BytesMut::new();
        client.encode((0x04, vec![1, 2, 3, 4]), &mut wire).unwrap();
        client.encode((0xab, vec![]), &mut wire).unwrap();
        // header + payload + mac for each frame; the second frame has an empty payload
        assert_eq!(wire.len(), (3 + 4 + 4) + (3 + 4));
        assert_ne!(&wire[..7], &[0x04, 0, 4, 1, 2, 3, 4], "header and payload must be encrypted");

        let first = server.decode(&mut wire).unwrap().unwrap();
        assert_eq!(first, (0x04, Bytes::from_static(&[1, 2, 3, 4])));
        let second = server.decode(&mut wire).unwrap().unwrap();
        assert_eq!(second, (0xab, Bytes::new()));
        assert!(server.decode(&mut wire).unwrap().is_none());
        assert!(wire.is_empty());
    }

    #[test]
    fn handles_partial_input() {
        let (mut client, mut server) = pair();
        let mut wire = BytesMut::new();
        client.encode((0x50, vec![9; 10]), &mut wire).unwrap();
        let full = wire.split();

        let mut partial = BytesMut::new();
        partial.extend_from_slice(&full[..5]); // header plus two payload bytes
        assert!(server.decode(&mut partial).unwrap().is_none());
        partial.extend_from_slice(&full[5..]);
        let frame = server.decode(&mut partial).unwrap().unwrap();
        assert_eq!(frame.0, 0x50);
        assert_eq!(&frame.1[..], &[9u8; 10]);
    }

    #[test]
    fn wrong_key_never_yields_a_frame() {
        let (mut client, _) = pair();
        let mut wrong = ShannonCodec::new(&[2u8; 32], &[3u8; 32]);
        let mut wire = BytesMut::new();
        client.encode((0x04, vec![1, 2, 3]), &mut wire).unwrap();
        assert!(!matches!(wrong.decode(&mut wire), Ok(Some(_))));
    }

    #[test]
    fn tampering_fails_the_mac() {
        let (mut client, mut server) = pair();
        let mut wire = BytesMut::new();
        client.encode((0x04, vec![1, 2, 3]), &mut wire).unwrap();
        wire[4] ^= 0xff; // second payload byte
        assert!(server.decode(&mut wire).is_err());
    }

    #[test]
    fn rejects_payloads_over_u16() {
        let (mut client, _) = pair();
        let mut wire = BytesMut::new();
        assert!(client.encode((0x04, vec![0; 70_000]), &mut wire).is_err());
    }
}

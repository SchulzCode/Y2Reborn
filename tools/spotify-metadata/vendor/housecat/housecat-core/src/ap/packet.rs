//! Command bytes on the access-point socket (spec section 5.2).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PacketType {
    SecretBlock = 0x02,
    Ping = 0x04,
    StreamChunk = 0x08,
    StreamChunkRes = 0x09,
    ChannelError = 0x0a,
    ChannelAbort = 0x0b,
    RequestKey = 0x0c,
    AesKey = 0x0d,
    AesKeyError = 0x0e,
    Image = 0x19,
    CountryCode = 0x1b,
    UnknownDataAllZeros = 0x1f,
    Pong = 0x49,
    PongAck = 0x4a,
    Pause = 0x4b,
    ProductInfo = 0x50,
    LegacyWelcome = 0x69,
    PreferredLocale = 0x74,
    LicenseVersion = 0x76,
    TrackEndedTime = 0x82,
    Login = 0xab,
    ApWelcome = 0xac,
    AuthFailure = 0xad,
    MercuryReq = 0xb2,
    MercurySub = 0xb3,
    MercuryUnsub = 0xb4,
    MercuryEvent = 0xb5,
}

impl PacketType {
    pub fn from_u8(cmd: u8) -> Option<Self> {
        use PacketType::*;
        Some(match cmd {
            0x02 => SecretBlock,
            0x04 => Ping,
            0x08 => StreamChunk,
            0x09 => StreamChunkRes,
            0x0a => ChannelError,
            0x0b => ChannelAbort,
            0x0c => RequestKey,
            0x0d => AesKey,
            0x0e => AesKeyError,
            0x19 => Image,
            0x1b => CountryCode,
            0x1f => UnknownDataAllZeros,
            0x49 => Pong,
            0x4a => PongAck,
            0x4b => Pause,
            0x50 => ProductInfo,
            0x69 => LegacyWelcome,
            0x74 => PreferredLocale,
            0x76 => LicenseVersion,
            0x82 => TrackEndedTime,
            0xab => Login,
            0xac => ApWelcome,
            0xad => AuthFailure,
            0xb2 => MercuryReq,
            0xb3 => MercurySub,
            0xb4 => MercuryUnsub,
            0xb5 => MercuryEvent,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bytes_round_trip() {
        for packet in [
            PacketType::Ping,
            PacketType::Pong,
            PacketType::PongAck,
            PacketType::CountryCode,
            PacketType::ProductInfo,
            PacketType::RequestKey,
            PacketType::AesKey,
            PacketType::AesKeyError,
            PacketType::Login,
            PacketType::ApWelcome,
            PacketType::AuthFailure,
            PacketType::MercuryEvent,
        ] {
            assert_eq!(PacketType::from_u8(packet as u8), Some(packet));
        }
        assert_eq!(PacketType::Login as u8, 0xab);
        assert_eq!(PacketType::RequestKey as u8, 0x0c);
        assert_eq!(PacketType::AesKeyError as u8, 0x0e);
    }

    #[test]
    fn unknown_bytes_are_none() {
        assert_eq!(PacketType::from_u8(0x00), None);
        assert_eq!(PacketType::from_u8(0xff), None);
    }
}

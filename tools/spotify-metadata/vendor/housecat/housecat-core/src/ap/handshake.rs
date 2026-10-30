//! The access-point handshake (spec section 5.1, step 3).
//!
//! Byte-for-byte the exchange librespot performs: ClientHello, APResponseMessage,
//! server-signature check, HMAC-SHA1 key derivation, ClientResponsePlaintext.

use hmac::{Hmac, Mac};
use prost::Message;
use rsa::{BigUint as RsaBigUint, Pkcs1v15Sign, RsaPublicKey};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::{Decoder, Framed};

use housecat_protocol as proto;

use crate::ap::codec::ShannonCodec;
use crate::ap::dh::DhKeys;
use crate::{consts, random, Error, Result};

pub(crate) struct DerivedKeys {
    pub challenge: Vec<u8>,
    pub send_key: Vec<u8>,
    pub recv_key: Vec<u8>,
}

/// Run the handshake on a freshly connected stream and return it wrapped in the codec.
pub async fn handshake<T: AsyncRead + AsyncWrite + Unpin>(mut stream: T) -> Result<Framed<T, ShannonCodec>> {
    let keys = DhKeys::random();
    let mut accumulator = send_client_hello(&mut stream, keys.public_key()).await?;

    let response: proto::ApResponseMessage = recv_prefixed(&mut stream, &mut accumulator).await?;
    if let Some(failed) = response.login_failed {
        return Err(Error::protocol(
            "handshake",
            format!("access point refused the handshake with error code {}", failed.error_code),
        ));
    }
    let dh = response
        .challenge
        .and_then(|challenge| challenge.login_crypto_challenge.diffie_hellman)
        .ok_or_else(|| Error::protocol("handshake", "APResponseMessage carries no Diffie-Hellman challenge"))?;

    verify_server_key(&dh.gs, &dh.gs_signature)?;

    let shared_secret = keys.shared_secret(&dh.gs);
    let derived = compute_keys(&shared_secret, &accumulator);
    send_client_response(&mut stream, derived.challenge).await?;

    Ok(ShannonCodec::new(&derived.send_key, &derived.recv_key).framed(stream))
}

/// Send the ClientHello and return every byte written, which seeds key derivation.
async fn send_client_hello<T: AsyncWrite + Unpin>(stream: &mut T, gc: Vec<u8>) -> Result<Vec<u8>> {
    let hello = proto::ClientHello {
        build_info: proto::BuildInfo {
            product: proto::Product::Client as i32,
            product_flags: vec![proto::ProductFlags::ProductFlagNone as i32],
            platform: consts::platform() as i32,
            version: consts::SPOTIFY_VERSION,
        },
        fingerprints_supported: vec![],
        cryptosuites_supported: vec![proto::Cryptosuite::CryptoSuiteShannon as i32],
        powschemes_supported: vec![],
        login_crypto_hello: proto::LoginCryptoHelloUnion {
            diffie_hellman: Some(proto::LoginCryptoDiffieHellmanHello { gc, server_keys_known: 1 }),
        },
        client_nonce: random::random_vec(16),
        padding: Some(vec![0x1e]),
        feature_set: None,
    };

    let body = hello.encode_to_vec();
    let mut packet = Vec::with_capacity(6 + body.len());
    packet.extend_from_slice(&[0x00, 0x04]);
    packet.extend_from_slice(&((2 + 4 + body.len()) as u32).to_be_bytes());
    packet.extend_from_slice(&body);

    stream.write_all(&packet).await?;
    Ok(packet)
}

async fn send_client_response<T: AsyncWrite + Unpin>(stream: &mut T, challenge: Vec<u8>) -> Result<()> {
    let response = proto::ClientResponsePlaintext {
        login_crypto_response: proto::LoginCryptoResponseUnion {
            diffie_hellman: Some(proto::LoginCryptoDiffieHellmanResponse { hmac: challenge }),
        },
        pow_response: proto::PoWResponseUnion::default(),
        crypto_response: proto::CryptoResponseUnion::default(),
    };

    let body = response.encode_to_vec();
    let mut packet = Vec::with_capacity(4 + body.len());
    packet.extend_from_slice(&((4 + body.len()) as u32).to_be_bytes());
    packet.extend_from_slice(&body);

    stream.write_all(&packet).await?;
    Ok(())
}

/// Read one `u32 BE length || message` frame, appending the raw bytes to the accumulator.
async fn recv_prefixed<T, M>(stream: &mut T, accumulator: &mut Vec<u8>) -> Result<M>
where
    T: AsyncRead + Unpin,
    M: Message + Default,
{
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    accumulator.extend_from_slice(&header);

    let size = u32::from_be_bytes(header) as usize;
    let body_len = size
        .checked_sub(4)
        .ok_or_else(|| Error::protocol("handshake", "response length is shorter than its own header"))?;

    let start = accumulator.len();
    accumulator.resize(start + body_len, 0);
    stream.read_exact(&mut accumulator[start..]).await?;

    M::decode(&accumulator[start..]).map_err(|e| Error::protocol("handshake", format!("bad response message: {e}")))
}

/// PKCS#1 v1.5 over SHA1(gs) with Spotify's published access-point key.
fn verify_server_key(gs: &[u8], signature: &[u8]) -> Result<()> {
    let n = RsaBigUint::from_bytes_be(&consts::SERVER_KEY);
    let e = RsaBigUint::from(consts::SERVER_KEY_EXPONENT);
    let key = RsaPublicKey::new(n, e).map_err(|e| Error::protocol("handshake", format!("server key: {e}")))?;

    let digest = Sha1::digest(gs);
    key.verify(Pkcs1v15Sign::new::<Sha1>(), &digest, signature).map_err(|_| {
        Error::protocol("handshake", "server signature did not verify; possible man-in-the-middle")
    })
}

pub(crate) fn compute_keys(shared_secret: &[u8], packets: &[u8]) -> DerivedKeys {
    type HmacSha1 = Hmac<Sha1>;

    let mut data = Vec::with_capacity(100);
    for i in 1..=5u8 {
        let mut mac = HmacSha1::new_from_slice(shared_secret).expect("hmac accepts any key length");
        mac.update(packets);
        mac.update(&[i]);
        data.extend_from_slice(&mac.finalize().into_bytes());
    }

    let mut mac = HmacSha1::new_from_slice(&data[..20]).expect("hmac accepts any key length");
    mac.update(packets);

    DerivedKeys {
        challenge: mac.finalize().into_bytes().to_vec(),
        send_key: data[20..52].to_vec(),
        recv_key: data[52..84].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn compute_keys_matches_independent_vector() {
        let secret: Vec<u8> = (0u8..96).collect();
        let packets = b"client-hello-bytesap-response-bytes";
        let keys = compute_keys(&secret, packets);
        assert_eq!(hex::encode(&keys.challenge), "a74581c17d7837a05ef7594d2812ea660196fba4");
        assert_eq!(
            hex::encode(&keys.send_key),
            "5b422bcafbb6acb569ce1d025d22edae53dd25d2dc84382715ceb3ab18100060"
        );
        assert_eq!(
            hex::encode(&keys.recv_key),
            "dab63755840f60c7cafd2056686c6faab77bf9761328d9de808fff72df498650"
        );
    }

    #[tokio::test]
    async fn client_hello_is_framed_and_well_formed() {
        let mut sink: Vec<u8> = Vec::new();
        let packet = send_client_hello(&mut sink, vec![7, 7, 7]).await.unwrap();
        assert_eq!(sink, packet, "the accumulator must hold exactly what was sent");
        assert_eq!(&packet[..2], &[0x00, 0x04]);
        let declared = u32::from_be_bytes(packet[2..6].try_into().unwrap()) as usize;
        assert_eq!(declared, packet.len());

        let hello = proto::ClientHello::decode(&packet[6..]).unwrap();
        assert_eq!(hello.build_info.product, proto::Product::Client as i32);
        assert_eq!(hello.build_info.version, consts::SPOTIFY_VERSION);
        assert_eq!(hello.build_info.platform, consts::platform() as i32);
        assert_eq!(hello.cryptosuites_supported, vec![proto::Cryptosuite::CryptoSuiteShannon as i32]);
        let dh = hello.login_crypto_hello.diffie_hellman.unwrap();
        assert_eq!(dh.gc, vec![7, 7, 7]);
        assert_eq!(dh.server_keys_known, 1);
        assert_eq!(hello.client_nonce.len(), 16);
        assert_eq!(hello.padding, Some(vec![0x1e]));
    }

    #[tokio::test]
    async fn client_response_is_length_prefixed() {
        let mut sink: Vec<u8> = Vec::new();
        send_client_response(&mut sink, vec![1; 20]).await.unwrap();
        let declared = u32::from_be_bytes(sink[..4].try_into().unwrap()) as usize;
        assert_eq!(declared, sink.len());
        let response = proto::ClientResponsePlaintext::decode(&sink[4..]).unwrap();
        assert_eq!(response.login_crypto_response.diffie_hellman.unwrap().hmac, vec![1; 20]);
    }

    #[tokio::test]
    async fn recv_prefixed_reads_one_message_and_accumulates_it() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let message = proto::ApResponseMessage { challenge: None, upgrade: None, login_failed: None };
        let body = message.encode_to_vec();
        let mut wire = ((4 + body.len()) as u32).to_be_bytes().to_vec();
        wire.extend_from_slice(&body);
        server.write_all(&wire).await.unwrap();

        let mut accumulator = vec![0xaa];
        let decoded: proto::ApResponseMessage = recv_prefixed(&mut client, &mut accumulator).await.unwrap();
        assert_eq!(decoded, message);
        assert_eq!(&accumulator[1..], &wire[..]);
    }

    #[test]
    fn bad_server_signature_is_rejected() {
        let err = verify_server_key(b"not-a-real-gs", &[0u8; 256]).unwrap_err();
        assert!(matches!(err, Error::Protocol { what: "handshake", .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn handshake_fails_cleanly_on_a_forged_server() {
        let (client, mut server) = tokio::io::duplex(65536);
        let fake_server = tokio::spawn(async move {
            // Consume the client hello, then answer with an unsigned DH challenge.
            let mut header = [0u8; 6];
            tokio::io::AsyncReadExt::read_exact(&mut server, &mut header).await.unwrap();
            let total = u32::from_be_bytes(header[2..6].try_into().unwrap()) as usize;
            let mut rest = vec![0u8; total - 6];
            tokio::io::AsyncReadExt::read_exact(&mut server, &mut rest).await.unwrap();

            let message = proto::ApResponseMessage {
                challenge: Some(proto::ApChallenge {
                    login_crypto_challenge: proto::LoginCryptoChallengeUnion {
                        diffie_hellman: Some(proto::LoginCryptoDiffieHellmanChallenge {
                            gs: vec![2],
                            server_signature_key: 0,
                            gs_signature: vec![0u8; 256],
                        }),
                    },
                    fingerprint_challenge: Default::default(),
                    pow_challenge: Default::default(),
                    crypto_challenge: Default::default(),
                    server_nonce: vec![0; 16],
                    padding: None,
                }),
                upgrade: None,
                login_failed: None,
            };
            let body = message.encode_to_vec();
            let mut wire = ((4 + body.len()) as u32).to_be_bytes().to_vec();
            wire.extend_from_slice(&body);
            server.write_all(&wire).await.unwrap();
        });

        let err = handshake(client).await.unwrap_err();
        assert!(matches!(err, Error::Protocol { what: "handshake", .. }), "got {err:?}");
        fake_server.await.unwrap();
    }
}

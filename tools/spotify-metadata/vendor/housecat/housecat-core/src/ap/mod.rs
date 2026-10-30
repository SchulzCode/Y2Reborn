//! The access-point connection: TCP, handshake, Shannon framing, login (spec section 5.1).

pub mod codec;
pub mod dh;
pub mod handshake;
pub mod packet;

use futures::{SinkExt, StreamExt};
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use housecat_protocol as proto;

use crate::auth::credentials::{CredentialKind, Credentials};
use crate::{consts, Error, Result};
use codec::ShannonCodec;
use packet::PacketType;

pub type Transport = Framed<TcpStream, ShannonCodec>;

/// TCP connect to `host:port` and run the handshake, each bounded by `AP_CONNECT_TIMEOUT`.
pub async fn connect(addr: &str) -> Result<Transport> {
    let stream = tokio::time::timeout(consts::AP_CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::Timeout("access point connect"))??;
    stream.set_nodelay(true)?;
    tokio::time::timeout(consts::AP_CONNECT_TIMEOUT, handshake::handshake(stream))
        .await
        .map_err(|_| Error::Timeout("access point handshake"))?
}

/// Log in over an established transport. Returns the reusable stored credentials.
pub async fn login<T>(
    transport: &mut Framed<T, ShannonCodec>,
    credentials: &Credentials,
    device_id: &str,
) -> Result<Credentials>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let packet = proto::ClientResponseEncrypted {
        login_credentials: proto::LoginCredentials {
            username: credentials.username.clone(),
            typ: credentials.kind.auth_type(),
            auth_data: Some(credentials.data.clone()),
        },
        account_creation: None,
        fingerprint_response: None,
        peer_ticket: None,
        system_info: proto::SystemInfo {
            cpu_family: consts::cpu_family() as i32,
            cpu_subtype: None,
            cpu_ext: None,
            brand: None,
            brand_flags: None,
            os: consts::os() as i32,
            os_version: None,
            os_ext: None,
            system_information_string: Some(consts::VERSION_STRING.to_string()),
            device_id: Some(device_id.to_string()),
        },
        platform_model: None,
        version_string: Some(consts::VERSION_STRING.to_string()),
        appkey: None,
        client_info: None,
    };

    tracing::debug!(kind = ?credentials.kind, "logging in to the access point");
    transport.send((PacketType::Login as u8, packet.encode_to_vec())).await?;

    let (cmd, data) = transport.next().await.ok_or(Error::Disconnected)??;
    match PacketType::from_u8(cmd) {
        Some(PacketType::ApWelcome) => {
            let welcome = proto::ApWelcome::decode(&data[..])
                .map_err(|e| Error::protocol("login", format!("bad APWelcome: {e}")))?;
            let kind = CredentialKind::from_auth_type(welcome.reusable_auth_credentials_type).ok_or_else(|| {
                Error::protocol(
                    "login",
                    format!("unknown reusable credential type {}", welcome.reusable_auth_credentials_type),
                )
            })?;
            Ok(Credentials {
                username: Some(welcome.canonical_username),
                kind,
                data: welcome.reusable_auth_credentials,
            })
        }
        Some(PacketType::AuthFailure) => {
            let failed = proto::ApLoginFailed::decode(&data[..])
                .map_err(|e| Error::protocol("login", format!("bad APLoginFailed: {e}")))?;
            Err(login_failure(failed))
        }
        _ => Err(Error::protocol("login", format!("unexpected packet 0x{cmd:02x} in reply to login"))),
    }
}

/// Map `APLoginFailed` to the error the app should act on.
pub(crate) fn login_failure(failed: proto::ApLoginFailed) -> Error {
    use proto::ErrorCode as Code;

    let code = Code::try_from(failed.error_code).ok();
    let description = failed.error_description.unwrap_or_default();
    match code {
        Some(Code::PremiumAccountRequired) => Error::PremiumRequired,
        Some(Code::TryAnotherAp) => Error::TryAnotherAccessPoint,
        Some(Code::BadCredentials | Code::CouldNotValidateCredentials | Code::ExtraVerificationRequired) => {
            Error::AuthRequired(format!("the access point rejected the credentials ({code:?}) {description}").trim().to_string())
        }
        _ => Error::protocol(
            "login",
            format!("access point refused login with code {} ({code:?}) {description}", failed.error_code).trim().to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credentials::Credentials;
    use crate::{consts, Error};
    use codec::ShannonCodec;
    use futures::{SinkExt, StreamExt};
    use housecat_protocol as proto;
    use packet::PacketType;
    use prost::Message;
    use tokio::io::DuplexStream;
    use tokio_util::codec::{Decoder, Framed};

    type Fake = Framed<DuplexStream, ShannonCodec>;

    fn framed_pair() -> (Fake, Fake) {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let client = ShannonCodec::new(&[1u8; 32], &[2u8; 32]).framed(client_io);
        let server = ShannonCodec::new(&[2u8; 32], &[1u8; 32]).framed(server_io);
        (client, server)
    }

    async fn expect_login(server: &mut Fake) -> proto::ClientResponseEncrypted {
        let (cmd, data) = server.next().await.unwrap().unwrap();
        assert_eq!(cmd, PacketType::Login as u8);
        proto::ClientResponseEncrypted::decode(&data[..]).unwrap()
    }

    #[tokio::test]
    async fn login_with_access_token_returns_stored_credentials() {
        let (mut client, mut server) = framed_pair();
        let fake_server = tokio::spawn(async move {
            let request = expect_login(&mut server).await;
            assert_eq!(request.login_credentials.typ, proto::AuthenticationType::AuthenticationSpotifyToken as i32);
            assert_eq!(request.login_credentials.username, None);
            assert_eq!(request.login_credentials.auth_data.as_deref(), Some(&b"tok"[..]));
            assert_eq!(request.system_info.device_id.as_deref(), Some("dev-1"));
            assert_eq!(request.system_info.os, consts::os() as i32);
            assert_eq!(request.version_string.as_deref(), Some(consts::VERSION_STRING));

            let welcome = proto::ApWelcome {
                canonical_username: "alice".to_string(),
                account_type_logged_in: 0,
                credentials_type_logged_in: 0,
                reusable_auth_credentials_type: proto::AuthenticationType::AuthenticationStoredSpotifyCredentials as i32,
                reusable_auth_credentials: vec![9, 9, 9],
                lfs_secret: None,
                account_info: None,
                fb: None,
            };
            server.send((PacketType::ApWelcome as u8, welcome.encode_to_vec())).await.unwrap();
        });

        let stored = login(&mut client, &Credentials::access_token("tok"), "dev-1").await.unwrap();
        assert_eq!(stored, Credentials::stored("alice", vec![9, 9, 9]));
        fake_server.await.unwrap();
    }

    #[tokio::test]
    async fn login_with_stored_credentials_sends_the_username() {
        let (mut client, mut server) = framed_pair();
        let fake_server = tokio::spawn(async move {
            let request = expect_login(&mut server).await;
            assert_eq!(request.login_credentials.typ, proto::AuthenticationType::AuthenticationStoredSpotifyCredentials as i32);
            assert_eq!(request.login_credentials.username.as_deref(), Some("alice"));
            let welcome = proto::ApWelcome {
                canonical_username: "alice".to_string(),
                account_type_logged_in: 0,
                credentials_type_logged_in: 0,
                reusable_auth_credentials_type: 1,
                reusable_auth_credentials: vec![1],
                lfs_secret: None,
                account_info: None,
                fb: None,
            };
            server.send((PacketType::ApWelcome as u8, welcome.encode_to_vec())).await.unwrap();
        });

        login(&mut client, &Credentials::stored("alice", vec![5]), "dev-1").await.unwrap();
        fake_server.await.unwrap();
    }

    async fn login_failing_with(code: i32) -> Error {
        let (mut client, mut server) = framed_pair();
        tokio::spawn(async move {
            expect_login(&mut server).await;
            let failed = proto::ApLoginFailed { error_code: code, retry_delay: None, expiry: None, error_description: None };
            server.send((PacketType::AuthFailure as u8, failed.encode_to_vec())).await.unwrap();
        });
        login(&mut client, &Credentials::access_token("tok"), "dev-1").await.unwrap_err()
    }

    #[tokio::test]
    async fn login_failures_map_to_typed_errors() {
        assert!(matches!(login_failing_with(11).await, Error::PremiumRequired));
        assert!(matches!(login_failing_with(12).await, Error::AuthRequired(_)));
        assert!(matches!(login_failing_with(13).await, Error::AuthRequired(_)));
        assert!(matches!(login_failing_with(2).await, Error::TryAnotherAccessPoint));
        assert!(matches!(login_failing_with(17).await, Error::Protocol { what: "login", .. }));
    }

    #[tokio::test]
    async fn closed_socket_is_disconnected() {
        let (mut client, server) = framed_pair();
        drop(server);
        let err = login(&mut client, &Credentials::access_token("tok"), "dev-1").await.unwrap_err();
        assert!(matches!(err, Error::Disconnected | Error::Io(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn connect_does_not_hang_when_the_peer_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), connect(&addr)).await;
        assert!(matches!(result, Ok(Err(_))), "connect must fail, not hang");
    }
}

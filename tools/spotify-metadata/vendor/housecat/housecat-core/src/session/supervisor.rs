//! Owns the access-point connection for the life of a session (spec section 5.3).
//!
//! Runs the dispatcher; when the socket is lost, reconnects through the connector the
//! session provides, with exponential backoff. Rejected credentials end the loop in
//! `AuthRequired`; closing the session ends it in `Closed`.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use futures::{Sink, Stream};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

use crate::auth::credentials::Credentials;
use crate::session::dispatcher::{self, RunEnd, Welcome};
use crate::session::link::{ApLink, SessionState};
use crate::{consts, Error, Result};

/// A freshly logged-in socket and the stored credentials the access point returned.
pub(crate) struct Connection<T> {
    pub transport: T,
    pub credentials: Credentials,
}

impl<T> std::fmt::Debug for Connection<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("username", &self.credentials.username)
            .finish_non_exhaustive()
    }
}

pub(crate) type ConnectFuture<T> = Pin<Box<dyn Future<Output = Result<Connection<T>>> + Send>>;
pub(crate) type Connector<T> = Box<dyn FnMut() -> ConnectFuture<T> + Send>;

pub(crate) async fn run<T>(
    first: T,
    first_welcome: Welcome,
    mut connector: Connector<T>,
    link: Arc<ApLink>,
    mut closing: watch::Receiver<bool>,
    on_credentials: Box<dyn Fn(&Credentials) + Send>,
) where
    T: Stream<Item = io::Result<(u8, Bytes)>> + Sink<(u8, Vec<u8>), Error = io::Error> + Unpin + Send + 'static,
{
    let mut transport = first;
    let mut welcome = Some(first_welcome);

    loop {
        let (tx, rx) = mpsc::unbounded_channel();
        link.attach(tx);
        let welcome_now = welcome
            .take()
            .unwrap_or_else(|| Welcome { country: watch::channel(None).0, product: watch::channel(None).0 });

        let end = dispatcher::run(transport, rx, closing.clone(), link.clone(), welcome_now).await;
        if end == RunEnd::Closed || *closing.borrow() {
            link.set_state(SessionState::Closed);
            return;
        }

        link.set_state(SessionState::Reconnecting);
        let mut delay = consts::RECONNECT_INITIAL_DELAY;
        transport = loop {
            tokio::select! {
                _ = closing.changed() => {
                    link.set_state(SessionState::Closed);
                    return;
                }
                _ = sleep(delay) => {}
            }

            tracing::info!(?delay, "reconnecting to an access point");
            match connector().await {
                Ok(Connection { transport, credentials }) => {
                    on_credentials(&credentials);
                    break transport;
                }
                Err(error @ (Error::AuthRequired(_) | Error::PremiumRequired)) => {
                    tracing::error!(%error, "reconnect refused; the session needs a new login");
                    link.set_state(SessionState::AuthRequired);
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, ?delay, "reconnect failed; backing off");
                    delay = (delay * 2).min(consts::RECONNECT_MAX_DELAY);
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ap::codec::ShannonCodec;
    use crate::ap::packet::PacketType;
    use crate::{FileId, SpotifyId};
    use futures::{SinkExt, StreamExt};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tokio::io::DuplexStream;
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, Instant};
    use tokio_util::codec::{Decoder, Framed};

    type Fake = Framed<DuplexStream, ShannonCodec>;

    fn framed_pair() -> (Fake, Fake) {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let client = ShannonCodec::new(&[1u8; 32], &[2u8; 32]).framed(client_io);
        let server = ShannonCodec::new(&[2u8; 32], &[1u8; 32]).framed(server_io);
        (client, server)
    }

    /// What the scripted connector does on each call; after the script runs out it connects.
    enum Script {
        Connect,
        Fail(fn() -> Error),
    }

    fn io_error() -> Error {
        Error::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"))
    }

    fn auth_error() -> Error {
        Error::AuthRequired("stored credentials rejected".to_string())
    }

    struct Harness {
        link: Arc<ApLink>,
        first_server: Fake,
        servers: mpsc::UnboundedReceiver<Fake>,
        attempts: Arc<Mutex<Vec<Instant>>>,
        credentials_seen: Arc<Mutex<Vec<String>>>,
        task: JoinHandle<()>,
    }

    fn harness(script: Vec<Script>) -> Harness {
        let (first_client, first_server) = framed_pair();
        let (server_tx, servers) = mpsc::unbounded_channel();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let connector: Connector<Fake> = Box::new({
            let script = script.clone();
            let attempts = attempts.clone();
            move || {
                attempts.lock().unwrap().push(Instant::now());
                let next = script.lock().unwrap().pop_front();
                let server_tx = server_tx.clone();
                Box::pin(async move {
                    match next {
                        Some(Script::Fail(make)) => Err(make()),
                        Some(Script::Connect) | None => {
                            let (client, server) = framed_pair();
                            server_tx.send(server).expect("the test receives new servers");
                            Ok(Connection { transport: client, credentials: Credentials::stored("alice", vec![7]) })
                        }
                    }
                })
            }
        });

        let (closing_tx, closing_rx) = watch::channel(false);
        let link = Arc::new(ApLink::new(closing_tx));
        let credentials_seen = Arc::new(Mutex::new(Vec::new()));
        let on_credentials: Box<dyn Fn(&Credentials) + Send> = Box::new({
            let seen = credentials_seen.clone();
            move |credentials| seen.lock().unwrap().push(credentials.username.clone().unwrap_or_default())
        });
        let welcome = Welcome { country: watch::channel(None).0, product: watch::channel(None).0 };
        let task = tokio::spawn(run(first_client, welcome, connector, link.clone(), closing_rx, on_credentials));
        Harness { link, first_server, servers, attempts, credentials_seen, task }
    }

    async fn wait_for(link: &ApLink, wanted: SessionState) {
        let mut state = link.state();
        loop {
            if *state.borrow_and_update() == wanted {
                return;
            }
            state.changed().await.expect("state sender alive");
        }
    }

    fn track() -> SpotifyId {
        SpotifyId::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn reconnects_after_the_socket_is_lost_and_serves_keys_again() {
        let mut h = harness(vec![Script::Connect]);
        wait_for(&h.link, SessionState::Connected).await;

        drop(h.first_server);
        wait_for(&h.link, SessionState::Reconnecting).await;
        let mut server = h.servers.recv().await.expect("a reconnect happens");
        wait_for(&h.link, SessionState::Connected).await;
        assert_eq!(h.credentials_seen.lock().unwrap().as_slice(), ["alice"]);

        let waiting = tokio::spawn({
            let link = h.link.clone();
            async move { link.audio_key(track(), FileId([3u8; 20])).await }
        });
        let (cmd, data) = server.next().await.unwrap().unwrap();
        assert_eq!(cmd, PacketType::RequestKey as u8);
        let seq = u32::from_be_bytes(data[36..40].try_into().unwrap());
        let mut reply = seq.to_be_bytes().to_vec();
        reply.extend_from_slice(&[0xcd; 16]);
        server.send((PacketType::AesKey as u8, reply)).await.unwrap();
        assert_eq!(waiting.await.unwrap().unwrap(), [0xcd; 16]);

        h.link.close();
        h.task.await.unwrap();
        assert_eq!(h.link.current_state(), SessionState::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_doubles_between_failed_attempts() {
        let mut h = harness(vec![Script::Fail(io_error), Script::Fail(io_error), Script::Connect]);
        wait_for(&h.link, SessionState::Connected).await;

        let lost_at = Instant::now();
        drop(h.first_server);
        let _server = h.servers.recv().await.expect("the third attempt connects");
        wait_for(&h.link, SessionState::Connected).await;

        let attempts = h.attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0] - lost_at, Duration::from_secs(1));
        assert_eq!(attempts[1] - attempts[0], Duration::from_secs(2));
        assert_eq!(attempts[2] - attempts[1], Duration::from_secs(4));

        h.link.close();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_is_capped() {
        let failures: Vec<Script> = (0..7).map(|_| Script::Fail(io_error)).collect();
        let mut h = harness(failures);
        wait_for(&h.link, SessionState::Connected).await;
        drop(h.first_server);
        let _server = h.servers.recv().await.expect("the eighth attempt connects");
        wait_for(&h.link, SessionState::Connected).await;

        let attempts = h.attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 8);
        // gaps between the eight attempts: 2, 4, 8, 16, 30, 30, 30 seconds
        assert_eq!(attempts[4] - attempts[3], Duration::from_secs(16));
        assert_eq!(attempts[5] - attempts[4], consts::RECONNECT_MAX_DELAY);
        assert_eq!(attempts[6] - attempts[5], consts::RECONNECT_MAX_DELAY);
        assert_eq!(attempts[7] - attempts[6], consts::RECONNECT_MAX_DELAY);

        h.link.close();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_credentials_stop_the_reconnect_loop() {
        let h = harness(vec![Script::Fail(auth_error)]);
        wait_for(&h.link, SessionState::Connected).await;
        drop(h.first_server);
        wait_for(&h.link, SessionState::AuthRequired).await;
        h.task.await.unwrap();
        assert_eq!(h.attempts.lock().unwrap().len(), 1);
        assert!(matches!(h.link.audio_key(track(), FileId([1; 20])).await, Err(Error::Disconnected)));
    }

    #[tokio::test(start_paused = true)]
    async fn close_during_backoff_ends_closed() {
        let h = harness(vec![Script::Fail(io_error), Script::Fail(io_error), Script::Fail(io_error)]);
        wait_for(&h.link, SessionState::Connected).await;
        drop(h.first_server);
        wait_for(&h.link, SessionState::Reconnecting).await;

        h.link.close();
        tokio::time::timeout(Duration::from_secs(60), h.task).await.expect("the loop stops").unwrap();
        assert_eq!(h.link.current_state(), SessionState::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn closing_a_connected_session_never_reconnects() {
        let h = harness(vec![Script::Connect]);
        wait_for(&h.link, SessionState::Connected).await;
        h.link.close();
        h.task.await.unwrap();
        assert_eq!(h.link.current_state(), SessionState::Closed);
        assert!(h.attempts.lock().unwrap().is_empty());
    }
}

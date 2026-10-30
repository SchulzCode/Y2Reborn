//! The live link to the access point: packet sender, pending audio-key requests, state.
//!
//! The socket behind the link is replaceable: the supervisor attaches a new sender after
//! every reconnect, and the `Session` handle the app holds never changes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use tokio::sync::{mpsc, oneshot, watch};

use crate::ap::packet::PacketType;
use crate::{consts, Error, FileId, Result, SpotifyId};

/// Spec section 5.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The first connection is being established.
    Connecting,
    Connected,
    /// The socket was lost; the supervisor is reconnecting with backoff.
    Reconnecting,
    /// Reconnecting stopped because the credentials were rejected; log in again.
    AuthRequired,
    Closed,
}

pub(crate) type KeyReply = Result<[u8; 16]>;
type Sender = mpsc::UnboundedSender<(u8, Vec<u8>)>;

pub struct ApLink {
    sender: Mutex<Option<Sender>>,
    closing: watch::Sender<bool>,
    pending_keys: Mutex<HashMap<u32, oneshot::Sender<KeyReply>>>,
    next_seq: AtomicU32,
    state: watch::Sender<SessionState>,
}

impl ApLink {
    pub(crate) fn new(closing: watch::Sender<bool>) -> Self {
        ApLink {
            sender: Mutex::new(None),
            closing,
            pending_keys: Mutex::new(HashMap::new()),
            next_seq: AtomicU32::new(0),
            state: watch::channel(SessionState::Connecting).0,
        }
    }

    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.state.subscribe()
    }

    pub fn current_state(&self) -> SessionState {
        *self.state.borrow()
    }

    pub fn is_connected(&self) -> bool {
        self.current_state() == SessionState::Connected
    }

    pub(crate) fn set_state(&self, state: SessionState) {
        self.state.send_replace(state);
    }

    /// Route packets through a freshly connected socket.
    pub(crate) fn attach(&self, sender: Sender) {
        *self.sender.lock().expect("sender lock") = Some(sender);
        self.set_state(SessionState::Connected);
    }

    /// Forget the socket and fail every waiting request. The caller decides the next state.
    pub(crate) fn detach(&self) {
        *self.sender.lock().expect("sender lock") = None;
        let pending: Vec<_> = self.pending_keys.lock().expect("pending keys lock").drain().collect();
        for (_, waiter) in pending {
            let _ = waiter.send(Err(Error::Disconnected));
        }
    }

    pub(crate) fn send(&self, cmd: PacketType, payload: Vec<u8>) -> Result<()> {
        match self.sender.lock().expect("sender lock").as_ref() {
            Some(sender) => sender.send((cmd as u8, payload)).map_err(|_| Error::Disconnected),
            None => Err(Error::Disconnected),
        }
    }

    /// Request the AES key for one audio file of one track (spec section 5.2).
    pub async fn audio_key(&self, track: SpotifyId, file: FileId) -> Result<[u8; 16]> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.pending_keys.lock().expect("pending keys lock").insert(seq, reply_tx);

        let mut payload = Vec::with_capacity(42);
        payload.extend_from_slice(&file.0);
        payload.extend_from_slice(&track.to_raw());
        payload.extend_from_slice(&seq.to_be_bytes());
        payload.extend_from_slice(&[0, 0]);
        if let Err(e) = self.send(PacketType::RequestKey, payload) {
            self.pending_keys.lock().expect("pending keys lock").remove(&seq);
            return Err(e);
        }

        match tokio::time::timeout(consts::AUDIO_KEY_TIMEOUT, reply_rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(Error::Disconnected),
            Err(_) => {
                self.pending_keys.lock().expect("pending keys lock").remove(&seq);
                Err(Error::Timeout("audio key"))
            }
        }
    }

    /// Deliver a key reply. Returns false when nobody was waiting for `seq`.
    pub(crate) fn resolve_key(&self, seq: u32, reply: KeyReply) -> bool {
        match self.pending_keys.lock().expect("pending keys lock").remove(&seq) {
            Some(waiter) => waiter.send(reply).is_ok(),
            None => false,
        }
    }

    /// Ask the socket task to stop, drop the socket, and mark the session closed.
    pub fn close(&self) {
        self.closing.send_replace(true);
        self.detach();
        self.set_state(SessionState::Closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn link() -> ApLink {
        ApLink::new(watch::channel(false).0)
    }

    fn track() -> SpotifyId {
        SpotifyId::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    #[tokio::test]
    async fn starts_connecting_and_attach_makes_it_connected() {
        let link = link();
        assert_eq!(link.current_state(), SessionState::Connecting);
        assert!(!link.is_connected());

        let (tx, mut rx) = mpsc::unbounded_channel();
        link.attach(tx);
        assert!(link.is_connected());
        link.send(PacketType::Pong, vec![1]).unwrap();
        assert_eq!(rx.recv().await, Some((PacketType::Pong as u8, vec![1])));
    }

    #[tokio::test]
    async fn send_and_audio_key_without_a_socket_are_disconnected() {
        let link = link();
        assert!(matches!(link.send(PacketType::Pong, vec![]), Err(Error::Disconnected)));
        assert!(matches!(link.audio_key(track(), FileId([1; 20])).await, Err(Error::Disconnected)));
    }

    #[tokio::test]
    async fn detach_fails_pending_requests_and_leaves_the_state_to_the_caller() {
        let link = Arc::new(link());
        let (tx, mut rx) = mpsc::unbounded_channel();
        link.attach(tx);
        let waiting = tokio::spawn({
            let link = link.clone();
            async move { link.audio_key(track(), FileId([2; 20])).await }
        });
        let (cmd, _) = rx.recv().await.unwrap();
        assert_eq!(cmd, PacketType::RequestKey as u8);

        link.detach();
        assert!(matches!(waiting.await.unwrap(), Err(Error::Disconnected)));
        assert_eq!(link.current_state(), SessionState::Connected, "detach does not decide the next state");
        link.set_state(SessionState::Reconnecting);
        assert_eq!(link.current_state(), SessionState::Reconnecting);
        assert!(matches!(link.send(PacketType::Pong, vec![]), Err(Error::Disconnected)));
    }

    #[tokio::test]
    async fn close_signals_the_socket_task_and_ends_closed() {
        let (closing_tx, closing_rx) = watch::channel(false);
        let link = ApLink::new(closing_tx);
        let (tx, _rx) = mpsc::unbounded_channel();
        link.attach(tx);

        link.close();
        assert!(*closing_rx.borrow());
        assert_eq!(link.current_state(), SessionState::Closed);
        assert!(matches!(link.send(PacketType::Pong, vec![]), Err(Error::Disconnected)));
    }

    #[tokio::test]
    async fn state_watchers_see_every_transition() {
        let link = link();
        let mut watcher = link.state();
        assert_eq!(*watcher.borrow_and_update(), SessionState::Connecting);
        let (tx, _rx) = mpsc::unbounded_channel();
        link.attach(tx);
        watcher.changed().await.unwrap();
        assert_eq!(*watcher.borrow_and_update(), SessionState::Connected);
        link.set_state(SessionState::AuthRequired);
        watcher.changed().await.unwrap();
        assert_eq!(*watcher.borrow_and_update(), SessionState::AuthRequired);
    }
}

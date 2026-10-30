//! The socket task (spec section 5.2): sends queued frames, routes received frames by
//! command byte, and runs the keepalive state machine.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use bytes::Bytes;
use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Instant};

use crate::ap::packet::PacketType;
use crate::session::link::ApLink;
use crate::{consts, Error, Result};

/// Values `connect()` waits for right after login.
pub(crate) struct Welcome {
    pub country: watch::Sender<Option<String>>,
    pub product: watch::Sender<Option<HashMap<String, String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keepalive {
    ExpectingPing,
    PendingPong,
    ExpectingPongAck,
}

enum KeepaliveEvent {
    Ping,
    PongAck,
}

/// Why the socket task stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunEnd {
    /// The application closed the session (or every handle to it is gone).
    Closed,
    /// The socket failed, ended, or missed a keepalive deadline.
    Lost,
}

pub(crate) async fn run<T>(
    mut transport: T,
    mut outgoing: mpsc::UnboundedReceiver<(u8, Vec<u8>)>,
    mut closing: watch::Receiver<bool>,
    link: Arc<ApLink>,
    welcome: Welcome,
) -> RunEnd
where
    T: Stream<Item = io::Result<(u8, Bytes)>> + Sink<(u8, Vec<u8>), Error = io::Error> + Unpin,
{
    let mut keepalive = Keepalive::ExpectingPing;
    let deadline = sleep(consts::KEEPALIVE_INITIAL_PING_TIMEOUT);
    tokio::pin!(deadline);

    let end = loop {
        tokio::select! {
            biased;

            _ = closing.changed() => {
                tracing::debug!("session closed by the application");
                break RunEnd::Closed;
            }

            frame = outgoing.recv() => match frame {
                Some(frame) => {
                    if let Err(error) = transport.send(frame).await {
                        tracing::warn!(%error, "access point write failed");
                        break RunEnd::Lost;
                    }
                }
                None => break RunEnd::Closed, // the link dropped its sender
            },

            frame = transport.next() => match frame {
                Some(Ok((cmd, data))) => match handle(cmd, data, &link, &welcome) {
                    Ok(Some(KeepaliveEvent::Ping)) => {
                        keepalive = Keepalive::PendingPong;
                        deadline.as_mut().reset(Instant::now() + consts::KEEPALIVE_PONG_DELAY);
                    }
                    Ok(Some(KeepaliveEvent::PongAck)) => {
                        keepalive = Keepalive::ExpectingPing;
                        deadline.as_mut().reset(Instant::now() + consts::KEEPALIVE_PING_TIMEOUT);
                    }
                    Ok(None) => {}
                    Err(error) => tracing::debug!(%error, cmd = format_args!("0x{cmd:02x}"), "could not handle packet"),
                },
                Some(Err(error)) => {
                    tracing::warn!(%error, "access point read failed");
                    break RunEnd::Lost;
                }
                None => {
                    tracing::warn!("access point closed the connection");
                    break RunEnd::Lost;
                }
            },

            _ = &mut deadline => match keepalive {
                Keepalive::PendingPong => {
                    tracing::trace!("sending pong");
                    if let Err(error) = transport.send((PacketType::Pong as u8, vec![0, 0, 0, 0])).await {
                        tracing::warn!(%error, "could not send pong");
                        break RunEnd::Lost;
                    }
                    keepalive = Keepalive::ExpectingPongAck;
                    deadline.as_mut().reset(Instant::now() + consts::KEEPALIVE_PONG_ACK_TIMEOUT);
                }
                Keepalive::ExpectingPing | Keepalive::ExpectingPongAck => {
                    tracing::warn!(?keepalive, "keepalive deadline missed; dropping the connection");
                    break RunEnd::Lost;
                }
            },
        }
    };

    link.detach();
    end
}

fn handle(cmd: u8, data: Bytes, link: &ApLink, welcome: &Welcome) -> Result<Option<KeepaliveEvent>> {
    use PacketType::*;

    match PacketType::from_u8(cmd) {
        Some(Ping) => {
            tracing::trace!("ping received");
            Ok(Some(KeepaliveEvent::Ping))
        }
        Some(PongAck) => {
            tracing::trace!("pong acknowledged");
            Ok(Some(KeepaliveEvent::PongAck))
        }
        Some(CountryCode) => {
            let country = String::from_utf8(data.to_vec())
                .map_err(|e| Error::protocol("country code", e.to_string()))?;
            tracing::info!(%country, "country code received");
            welcome.country.send_replace(Some(country));
            Ok(None)
        }
        Some(ProductInfo) => {
            let xml = std::str::from_utf8(&data).map_err(|e| Error::protocol("product info", e.to_string()))?;
            let attributes = parse_product_info(xml)?;
            tracing::debug!(count = attributes.len(), "product info received");
            welcome.product.send_replace(Some(attributes));
            Ok(None)
        }
        Some(AesKey) => {
            deliver_audio_key(&data, link, true)?;
            Ok(None)
        }
        Some(AesKeyError) => {
            deliver_audio_key(&data, link, false)?;
            Ok(None)
        }
        Some(other) => {
            tracing::trace!(?other, len = data.len(), "ignoring packet");
            Ok(None)
        }
        None => {
            tracing::trace!(cmd = format_args!("0x{cmd:02x}"), len = data.len(), "ignoring unknown packet");
            Ok(None)
        }
    }
}

/// Reply layout: `seq: u32 BE` then either a 16-byte key or a 2-byte error code.
fn deliver_audio_key(data: &Bytes, link: &ApLink, is_key: bool) -> Result<()> {
    if data.len() < 4 {
        return Err(Error::protocol("audio key", "reply shorter than a sequence number"));
    }
    let seq = u32::from_be_bytes(data[..4].try_into().expect("4 bytes"));
    let reply = if is_key {
        if data.len() < 20 {
            Err(Error::protocol("audio key", format!("key reply of {} bytes", data.len())))
        } else {
            let mut key = [0u8; 16];
            key.copy_from_slice(&data[4..20]);
            Ok(key)
        }
    } else {
        let code = if data.len() >= 6 { u16::from_be_bytes([data[4], data[5]]) } else { 0 };
        tracing::warn!(seq, code, "audio key refused");
        Err(Error::AudioKeyRefused { code })
    };
    if !link.resolve_key(seq, reply) {
        tracing::debug!(seq, "audio key reply for a request nobody is waiting on");
    }
    Ok(())
}

/// Flatten the ProductInfo XML into `element name -> text` for every leaf element,
/// the way librespot builds its user attributes. `type` is `premium` for a usable account.
pub(crate) fn parse_product_info(xml: &str) -> Result<HashMap<String, String>> {
    let document = roxmltree::Document::parse(xml).map_err(|e| Error::protocol("product info", e.to_string()))?;
    let mut attributes = HashMap::new();
    for node in document.descendants().filter(|n| n.is_element()) {
        let has_element_children = node.children().any(|c| c.is_element());
        if has_element_children {
            continue;
        }
        let text = node.text().map(str::trim).unwrap_or("");
        attributes.insert(node.tag_name().name().to_string(), text.to_string());
    }
    Ok(attributes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ap::codec::ShannonCodec;
    use crate::session::link::SessionState;
    use crate::{FileId, SpotifyId};
    use futures::SinkExt;
    use std::time::Duration;
    use tokio::io::DuplexStream;
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use tokio_util::codec::{Decoder, Framed};

    struct Harness {
        link: Arc<ApLink>,
        server: Framed<DuplexStream, ShannonCodec>,
        country: watch::Receiver<Option<String>>,
        product: watch::Receiver<Option<HashMap<String, String>>>,
        task: JoinHandle<RunEnd>,
    }

    fn harness() -> Harness {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let client = ShannonCodec::new(&[1u8; 32], &[2u8; 32]).framed(client_io);
        let server = ShannonCodec::new(&[2u8; 32], &[1u8; 32]).framed(server_io);
        let (tx, rx) = mpsc::unbounded_channel();
        let (closing_tx, closing_rx) = watch::channel(false);
        let link = Arc::new(ApLink::new(closing_tx));
        link.attach(tx);
        let (country_tx, country) = watch::channel(None);
        let (product_tx, product) = watch::channel(None);
        let task = tokio::spawn({
            let link = link.clone();
            async move {
                let end = run(client, rx, closing_rx, link.clone(), Welcome { country: country_tx, product: product_tx }).await;
                link.set_state(SessionState::Closed);
                end
            }
        });
        Harness { link, server, country, product, task }
    }

    fn track() -> SpotifyId {
        SpotifyId::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    #[test]
    fn product_info_xml_becomes_a_map() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><products><product><type>premium</type><head-files-url>https://heads-fa.spotify.com/head/{file_id}</head-files-url><catalogue>premium</catalogue></product></products>"#;
        let map = parse_product_info(xml).unwrap();
        assert_eq!(map["type"], "premium");
        assert_eq!(map["catalogue"], "premium");
        assert_eq!(map["head-files-url"], "https://heads-fa.spotify.com/head/{file_id}");
        assert!(!map.contains_key("product"), "container elements carry no text");
        assert!(matches!(parse_product_info("<not xml"), Err(Error::Protocol { .. })));
    }

    #[tokio::test]
    async fn country_and_product_info_are_published() {
        let mut h = harness();
        h.server.send((PacketType::CountryCode as u8, b"SE".to_vec())).await.unwrap();
        h.country.changed().await.unwrap();
        assert_eq!(h.country.borrow().as_deref(), Some("SE"));

        let xml = r#"<products><product><type>premium</type></product></products>"#;
        h.server.send((PacketType::ProductInfo as u8, xml.as_bytes().to_vec())).await.unwrap();
        h.product.changed().await.unwrap();
        assert_eq!(h.product.borrow().as_ref().unwrap()["type"], "premium");
        assert!(h.link.is_connected());
    }

    #[tokio::test]
    async fn audio_key_request_and_reply_round_trip() {
        let mut h = harness();
        let file = FileId([7u8; 20]);
        let waiting = tokio::spawn({
            let link = h.link.clone();
            async move { link.audio_key(track(), file).await }
        });

        let (cmd, data) = h.server.next().await.unwrap().unwrap();
        assert_eq!(cmd, PacketType::RequestKey as u8);
        assert_eq!(data.len(), 42);
        assert_eq!(&data[..20], &[7u8; 20]);
        assert_eq!(&data[20..36], &track().to_raw());
        let seq = u32::from_be_bytes(data[36..40].try_into().unwrap());
        assert_eq!(&data[40..], &[0, 0]);

        let mut reply = seq.to_be_bytes().to_vec();
        reply.extend_from_slice(&[0xab; 16]);
        h.server.send((PacketType::AesKey as u8, reply)).await.unwrap();
        assert_eq!(waiting.await.unwrap().unwrap(), [0xab; 16]);
    }

    #[tokio::test]
    async fn audio_key_refusal_carries_the_code() {
        let mut h = harness();
        let waiting = tokio::spawn({
            let link = h.link.clone();
            async move { link.audio_key(track(), FileId([1u8; 20])).await }
        });
        let (_, data) = h.server.next().await.unwrap().unwrap();
        let seq = u32::from_be_bytes(data[36..40].try_into().unwrap());
        let mut reply = seq.to_be_bytes().to_vec();
        reply.extend_from_slice(&[0x00, 0x01]);
        h.server.send((PacketType::AesKeyError as u8, reply)).await.unwrap();
        let err = waiting.await.unwrap().unwrap_err();
        assert!(matches!(err, Error::AudioKeyRefused { code: 1 }), "got {err:?}");
    }

    #[tokio::test]
    async fn unknown_packets_are_ignored() {
        let mut h = harness();
        h.server.send((0xb2, vec![1, 2, 3])).await.unwrap(); // Mercury, unused
        h.server.send((0xff, vec![])).await.unwrap(); // unknown
        h.server.send((PacketType::CountryCode as u8, b"NO".to_vec())).await.unwrap();
        h.country.changed().await.unwrap();
        assert_eq!(h.country.borrow().as_deref(), Some("NO"));
    }

    #[tokio::test]
    async fn socket_close_fails_pending_requests_and_marks_closed() {
        let mut h = harness();
        let waiting = tokio::spawn({
            let link = h.link.clone();
            async move { link.audio_key(track(), FileId([1u8; 20])).await }
        });
        h.server.next().await.unwrap().unwrap();
        drop(h.server);

        assert!(matches!(waiting.await.unwrap(), Err(Error::Disconnected)));
        assert_eq!(h.task.await.unwrap(), RunEnd::Lost);
        assert_eq!(*h.link.state().borrow(), SessionState::Closed);
        assert!(matches!(h.link.audio_key(track(), FileId([1u8; 20])).await, Err(Error::Disconnected)));
    }

    #[tokio::test]
    async fn close_stops_the_task() {
        let h = harness();
        h.link.close();
        let end = tokio::time::timeout(Duration::from_secs(5), h.task).await.expect("task must stop").unwrap();
        assert_eq!(end, RunEnd::Closed);
        assert_eq!(*h.link.state().borrow(), SessionState::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn keepalive_pongs_after_the_delay_and_gives_up_without_an_ack() {
        let mut h = harness();
        h.server.send((PacketType::Ping as u8, vec![0, 0, 0, 0])).await.unwrap();

        tokio::time::sleep(consts::KEEPALIVE_PONG_DELAY + Duration::from_secs(1)).await;
        let (cmd, data) = tokio::time::timeout(Duration::from_secs(1), h.server.next())
            .await
            .expect("pong must have been sent")
            .unwrap()
            .unwrap();
        assert_eq!(cmd, PacketType::Pong as u8);
        assert_eq!(&data[..], &[0, 0, 0, 0]);

        tokio::time::sleep(consts::KEEPALIVE_PONG_ACK_TIMEOUT + Duration::from_secs(1)).await;
        let end = tokio::time::timeout(Duration::from_secs(1), h.task).await.expect("dispatcher must stop").unwrap();
        assert_eq!(end, RunEnd::Lost);
        assert_eq!(*h.link.state().borrow(), SessionState::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn keepalive_survives_a_full_ping_pong_ack_cycle() {
        let mut h = harness();
        h.server.send((PacketType::Ping as u8, vec![0, 0, 0, 0])).await.unwrap();
        tokio::time::sleep(consts::KEEPALIVE_PONG_DELAY + Duration::from_secs(1)).await;
        let (cmd, _) = h.server.next().await.unwrap().unwrap();
        assert_eq!(cmd, PacketType::Pong as u8);
        h.server.send((PacketType::PongAck as u8, vec![])).await.unwrap();
        tokio::time::sleep(Duration::from_secs(70)).await; // inside the 80 s ping window
        assert!(h.link.is_connected());
        assert!(!h.task.is_finished());
    }
}

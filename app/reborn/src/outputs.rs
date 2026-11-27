//! Choosing and keeping the audio output: automatic output, a lost Bluetooth peer. Codec choices are
//! the radio daemon's (it applies the owner's choice when a headset connects or its stream stops).
use crate::*;

/// Bluetooth headphones win over the headphone jack. The first time a
/// connected peer's playback PCM is ready, audio moves to it (playback
/// continues there). Once per connection: choosing the jack afterwards sticks
/// until the peer reconnects, and a codec change that recreates the PCM is not
/// a new connection. A working Bluetooth output is never displaced. Losing the
/// peer returns to the jack and pauses (`Event::BluetoothDisconnected`).
///
/// Ready means what `Runtime::switch` needs: a stereo PCM with its rate known.
/// A PCM that BlueALSA lists before its codec is configured is not ready yet,
/// and a switch that fails is tried again (`failed`) instead of leaving the
/// music on the jack for the whole connection (Y2Reborn #103: the first
/// connect after Bluetooth on stayed silent until a reconnect).
///
/// A peer counts as routed only once the switch held: `started` parks it in
/// `in_flight`, and `settle` moves it to `routed` when the output really is that
/// peer. A start that fails later rolls the output back to the jack, and then the
/// switch is tried again like any failed one. Every snapshot is `observe`d, because
/// the main loop can receive a drop and the reconnect in one pass, and a peer that
/// stayed in `routed` through that was never routed again (the music started on
/// the speaker until the owner reconnected the headphones by hand).
#[derive(Default)]
pub(crate) struct AutoOutput {
    pub(crate) routed: HashSet<String>,
    /// Failed switches per peer: how many and when the last one was.
    pub(crate) failures: std::collections::HashMap<String, (u32, Instant)>,
    /// Switches accepted (the start may still be opening the stream), not yet known to hold.
    pub(crate) in_flight: HashSet<String>,
    /// What was last logged per peer: why it is not routed, and what it looked like.
    held: std::collections::HashMap<String, String>,
    seen: std::collections::HashMap<String, (bool, String)>,
}

/// A peer whose state changed between two snapshots.
pub(crate) struct PeerChange {
    /// Whether it was connected before (`None`: not seen before).
    pub(crate) was_connected: Option<bool>,
    pub(crate) connected: bool,
    pub(crate) fact: Value,
}

/// After a failed automatic switch, the next try waits this long...
pub(crate) const AUTO_OUTPUT_RETRY: Duration = Duration::from_secs(2);
/// ...and after this many failures in one connection the jack stays.
pub(crate) const AUTO_OUTPUT_TRIES: u32 = 5;

impl AutoOutput {
    /// One snapshot of the radio daemon's state, however many arrive in a pass: a peer that is
    /// not connected in it starts over when it connects again. A failed observation proves nothing.
    pub(crate) fn observe(&mut self, status: &bluetooth::Status) {
        if !status.available {
            return;
        }
        let connected = |address: &String| {
            status.powered
                && status
                    .devices
                    .iter()
                    .any(|d| d.connected && d.address.eq_ignore_ascii_case(address))
        };
        self.routed.retain(connected);
        self.failures.retain(|address, _| connected(address));
        self.in_flight.retain(connected);
        self.held.retain(|address, _| connected(address));
    }
    /// The selected peer was lost: its next connection is a new one.
    pub(crate) fn forget(&mut self, address: &str) {
        self.routed.retain(|a| !a.eq_ignore_ascii_case(address));
        self.failures
            .retain(|a, _| !a.eq_ignore_ascii_case(address));
        self.in_flight.retain(|a| !a.eq_ignore_ascii_case(address));
        self.held.retain(|a, _| !a.eq_ignore_ascii_case(address));
    }
    /// A switch to `address` was accepted: it is routed once the output holds it. The failures so
    /// far stay, so rolled-back starts count toward the same few tries as refused ones.
    pub(crate) fn started(&mut self, address: String) {
        self.in_flight.insert(address);
    }
    /// Accepted switches whose start has finished (`pending` false): the peer is routed when
    /// the output is on it, else the start was rolled back and the switch failed after all.
    /// Returns each peer with whether it held.
    pub(crate) fn settle(&mut self, output: &AudioOutput, pending: bool) -> Vec<(String, bool)> {
        if pending {
            return Vec::new();
        }
        let mut settled: Vec<String> = self.in_flight.drain().collect();
        settled.sort();
        settled
            .into_iter()
            .map(|address| {
                let holds = matches!(
                    output,
                    AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&address)
                );
                if holds {
                    self.routed(address.clone());
                } else {
                    self.failed(address.clone());
                }
                (address, holds)
            })
            .collect()
    }
    /// Why each connected audio peer is not being routed, when that changed since last asked
    /// (the log of a stuck output: the reason says which rule held it back).
    pub(crate) fn why_held(
        &mut self,
        status: &bluetooth::Status,
        output: &AudioOutput,
    ) -> Vec<(String, String)> {
        let mut changed = Vec::new();
        for device in status.devices.iter().filter(|d| d.connected && d.audio) {
            let address = &device.address;
            let on_it = matches!(
                output,
                AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(address)
            );
            let reason = if on_it {
                None
            } else if self.in_flight.contains(address) {
                Some("switch_starting".to_string())
            } else if self.routed.contains(address) {
                Some("routed_but_output_is_not_it".to_string())
            } else if self
                .failures
                .get(address)
                .is_some_and(|(_, at)| at.elapsed() < AUTO_OUTPUT_RETRY)
            {
                Some("resting_after_failure".to_string())
            } else if let Err(error) = status.playback_rate(address) {
                Some(format!("not_ready: {error}"))
            } else {
                None
            };
            if self.held.get(address) == reason.as_ref() {
                continue;
            }
            match &reason {
                Some(r) => self.held.insert(address.clone(), r.clone()),
                None => self.held.remove(address),
            };
            changed.push((address.clone(), reason.unwrap_or_else(|| "eligible".into())));
        }
        changed
    }
    /// What each connected or audio peer looks like, for those that changed since last asked.
    pub(crate) fn changed_peers(&mut self, status: &bluetooth::Status) -> Vec<PeerChange> {
        if !status.available {
            return Vec::new();
        }
        let mut changed = Vec::new();
        for device in status.devices.iter().filter(|d| d.connected || d.audio) {
            let pcm = status
                .pcms
                .iter()
                .find(|p| p.is_a2dp_playback_for(&device.path));
            let fact = json!({
                "address": device.address,
                "connected": device.connected,
                "audio": device.audio,
                "settling": device.settling,
                "bluealsa": status.bluealsa,
                "pcm": pcm.map(|p| json!({
                    "generation": p.transport_generation,
                    "codec": p.codec,
                    "rate": p.rate,
                    "channels": p.channels,
                })),
            });
            let now = (device.connected, fact.to_string());
            let before = self.seen.insert(device.address.clone(), now.clone());
            if before.as_ref() != Some(&now) {
                changed.push(PeerChange {
                    was_connected: before.map(|(connected, _)| connected),
                    connected: device.connected,
                    fact,
                });
            }
        }
        self.seen
            .retain(|address, _| status.devices.iter().any(|d| &d.address == address));
        changed
    }
    /// The log of the Bluetooth peers: each change of state, and the connects and disconnects.
    pub(crate) fn log_peers(&mut self, log: &Observer, status: &bluetooth::Status) {
        for change in self.changed_peers(status) {
            if change.connected && change.was_connected != Some(true) {
                log.add("bluetooth_connects", 1.);
            } else if !change.connected && change.was_connected == Some(true) {
                log.add("bluetooth_disconnects", 1.);
            }
            log.emit(
                Level::Info,
                "bluetooth",
                "peer_state",
                "Bluetooth peer changed",
                None,
                change.fact,
            );
        }
    }
    /// The log of the automatic output: a switch that did not hold, and why a connected peer is
    /// not the output (each once, when it changes).
    pub(crate) fn log_output(&mut self, log: &Observer, rt: &Runtime) {
        for (address, held) in self.settle(&rt.model.output, rt.pending_reconfiguration.is_some()) {
            if !held {
                // The start was rolled back after the switch was accepted: tried again.
                log.emit(
                    Level::Warn,
                    "audio",
                    "output_automatic_rolled_back",
                    "Automatic switch to Bluetooth did not hold",
                    None,
                    json!({"address":address,"output":rt.model.output}),
                );
            }
        }
        for (address, reason) in self.why_held(&rt.bt_state, &rt.model.output) {
            log.emit(
                Level::Info,
                "audio",
                "auto_output_held",
                "Bluetooth peer is connected but the output is not on it",
                None,
                json!({
                    "address":address,
                    "reason":reason,
                    "output":rt.model.output,
                    "pending_reconfiguration":rt.pending_reconfiguration.is_some(),
                    "lost_peer":rt.lost_peer.is_some(),
                }),
            );
        }
    }
    pub(crate) fn next(
        &mut self,
        status: &bluetooth::Status,
        output: &AudioOutput,
    ) -> Option<String> {
        if !status.available {
            return None; // A failed observation is not a disconnection.
        }
        self.observe(status);
        if !status.powered {
            return None;
        }
        let ready = |address: &str| status.playback_rate(address).is_ok();
        let resting = |address: &String| {
            self.failures
                .get(address)
                .is_some_and(|(_, at)| at.elapsed() < AUTO_OUTPUT_RETRY)
        };
        let peer = status.devices.iter().find(|d| {
            d.connected
                && d.audio
                && !self.routed.contains(&d.address)
                && !resting(&d.address)
                && ready(&d.address)
        })?;
        match output {
            AudioOutput::Bluetooth(current) if current.eq_ignore_ascii_case(&peer.address) => {
                // Already there (restored or chosen by hand): this connection is done.
                self.routed.insert(peer.address.clone());
                None
            }
            AudioOutput::Bluetooth(current) if ready(current) => None,
            _ => Some(peer.address.clone()),
        }
    }
    pub(crate) fn routed(&mut self, address: String) {
        self.failures.remove(&address);
        self.routed.insert(address);
    }
    /// A switch to `address` failed: try again after `AUTO_OUTPUT_RETRY`, at most
    /// `AUTO_OUTPUT_TRIES` times per connection.
    pub(crate) fn failed(&mut self, address: String) {
        let tries = self.failures.get(&address).map_or(0, |(n, _)| *n) + 1;
        if tries >= AUTO_OUTPUT_TRIES {
            self.routed(address);
        } else {
            self.failures.insert(address, (tries, Instant::now()));
        }
    }
}

/// Bluetooth headphones that were the output until they dropped (F51).
pub(crate) struct LostPeer {
    pub(crate) address: String,
    /// The loss, or the wake after it.
    pub(crate) since: Instant,
    /// A Play is waiting for them since then.
    pub(crate) play: Option<Instant>,
}

impl LostPeer {
    pub(crate) fn new(address: String, since: Instant) -> Self {
        Self {
            address,
            since,
            play: None,
        }
    }
    /// What a loss of the selected output leaves to wait for: nothing when it was not Bluetooth, and
    /// nothing when the listener switched Bluetooth off (the loss is that choice; Y2Linux #136: Play
    /// said "Waiting for …" for minutes with Bluetooth off).
    pub(crate) fn after_loss(
        output: &AudioOutput,
        switched_off: bool,
        now: Instant,
    ) -> Option<Self> {
        match output {
            AudioOutput::Bluetooth(address) if !switched_off => {
                Some(Self::new(address.clone(), now))
            }
            _ => None,
        }
    }
    pub(crate) fn is(&self, output: &AudioOutput) -> bool {
        matches!(output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&self.address))
    }
    /// Play waits: the jack took over from them a short while ago and holds
    /// no headphones (a plug there is a choice of its own).
    pub(crate) fn applies(
        &self,
        output: &AudioOutput,
        headphones: Option<bool>,
        now: Instant,
    ) -> bool {
        matches!(output, AudioOutput::Wired)
            && headphones != Some(true)
            && !self.gave_up(now)
            && (self.play.is_some() || !self.expired(now))
    }
    pub(crate) fn gave_up(&self, now: Instant) -> bool {
        self.play
            .is_some_and(|at| now.saturating_duration_since(at) >= LOST_PEER_PLAY_WAIT)
    }
    pub(crate) fn expired(&self, now: Instant) -> bool {
        self.play.is_none() && now.saturating_duration_since(self.since) >= LOST_PEER_WINDOW
    }
}

/// A start failed because the selected Bluetooth peer is not connected, and
/// the last observation agrees: the disconnect was missed, not just late.
pub(crate) fn selected_peer_gone(
    output: &AudioOutput,
    state: &bluetooth::Status,
    error: &str,
) -> bool {
    let AudioOutput::Bluetooth(address) = output else {
        return false;
    };
    error.contains("A2DP peer required")
        && state.available
        && !state
            .devices
            .iter()
            .any(|d| d.address.eq_ignore_ascii_case(address) && d.connected)
}

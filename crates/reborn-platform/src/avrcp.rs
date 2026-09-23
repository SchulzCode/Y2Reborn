#![forbid(unsafe_code)]
//! BlueZ's registered MPRIS player projects the existing model; it owns no playback state.
use dbus::{
    arg::{PropMap, Variant},
    blocking::{stdintf::org_freedesktop_dbus::ObjectManager, Connection},
    channel::{MatchingReceiver, Sender},
    message::MatchRule,
    Message, Path,
};
use reborn_core::{Action, AppModel, PlaybackState};
use reborn_observability::{HealthState, Observer};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const PLAYER: &str = "/org/reborn/player";
const IFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}
impl RemoteAction {
    pub fn semantic(self, model: &AppModel) -> Option<Action> {
        let active = matches!(
            model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        match self {
            Self::Play if active => None,
            Self::Pause if !active => None,
            Self::Play | Self::Pause | Self::Toggle => Some(Action::PlayPause),
            Self::Next => Some(Action::NextTrack),
            Self::Previous => Some(Action::PreviousTrack),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    status: String,
    track: Option<i64>,
    title: String,
    artist: String,
    album: String,
    length_us: i64,
    position_us: i64,
    can_play: bool,
    can_next: bool,
    can_previous: bool,
}
impl Default for View {
    fn default() -> Self {
        Self::model(&AppModel::default())
    }
}
impl View {
    pub fn model(model: &AppModel) -> Self {
        let track = model.current();
        let bounded = |s: &str| s.chars().take(256).collect::<String>();
        Self {
            status: match model.playback {
                PlaybackState::Playing => "Playing",
                PlaybackState::Paused | PlaybackState::Buffering => "Paused",
                _ => "Stopped",
            }
            .into(),
            track: track.map(|t| t.id),
            title: track.map(|t| bounded(&t.title)).unwrap_or_default(),
            artist: track.map(|t| bounded(&t.artist)).unwrap_or_default(),
            album: track.map(|t| bounded(&t.album)).unwrap_or_default(),
            length_us: track
                .map(|t| t.duration_ms.saturating_mul(1000).min(i64::MAX as u64) as i64)
                .unwrap_or(0),
            position_us: model.position_ms.saturating_mul(1000).min(i64::MAX as u64) as i64,
            can_play: track.is_some_and(|t| t.online),
            can_next: !model.queue.is_empty(),
            can_previous: !model.queue.is_empty(),
        }
    }
    fn properties(&self) -> PropMap {
        let mut metadata = PropMap::new();
        let id = self
            .track
            .map(|id| format!("/org/reborn/track/t{}", id.unsigned_abs()))
            .unwrap_or_else(|| "/org/mpris/MediaPlayer2/TrackList/NoTrack".into());
        metadata.insert("mpris:trackid".into(), Variant(Box::new(Path::from(id))));
        metadata.insert("mpris:length".into(), Variant(Box::new(self.length_us)));
        metadata.insert("xesam:title".into(), Variant(Box::new(self.title.clone())));
        metadata.insert(
            "xesam:artist".into(),
            Variant(Box::new(vec![self.artist.clone()])),
        );
        metadata.insert("xesam:album".into(), Variant(Box::new(self.album.clone())));
        let mut p = PropMap::new();
        p.insert("Metadata".into(), Variant(Box::new(metadata)));
        p.insert(
            "PlaybackStatus".into(),
            Variant(Box::new(self.status.clone())),
        );
        p.insert("Position".into(), Variant(Box::new(self.position_us)));
        for (name, value) in [
            ("CanControl", true),
            ("CanPlay", self.can_play),
            ("CanPause", self.can_play),
            ("CanGoNext", self.can_next),
            ("CanGoPrevious", self.can_previous),
            ("CanSeek", false),
        ] {
            p.insert(name.into(), Variant(Box::new(value)));
        }
        p
    }
    fn notification(&self) -> Self {
        let mut v = self.clone();
        v.position_us = 0;
        v
    }
}

pub struct Player {
    pub actions: Receiver<RemoteAction>,
    view: Arc<Mutex<View>>,
    stop: Arc<AtomicBool>,
}
impl Player {
    pub fn spawn(log: Observer) -> std::io::Result<Self> {
        Self::spawn_with(log, Connection::new_system)
    }
    fn spawn_with(
        log: Observer,
        connect: impl Fn() -> Result<Connection, dbus::Error> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, actions) = sync_channel(8);
        let view = Arc::new(Mutex::new(View::default()));
        let shared = view.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let ending = stop.clone();
        thread::Builder::new().name("avrcp".into()).spawn(move || {
            while !ending.load(Ordering::Relaxed) {
                let c = match connect() {
                    Ok(c) => c,
                    Err(_) => {
                        thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };
                let authorized = Arc::new(Mutex::new(String::new()));
                let auth = authorized.clone();
                let current = shared.clone();
                let commands = sender.clone();
                let mut rule = MatchRule::new_method_call();
                rule.path = Some(PLAYER.into());
                c.start_receive(
                    rule,
                    Box::new(move |msg, c| {
                        let owner = auth.lock().map(|s| s.clone()).unwrap_or_default();
                        let reject = || {
                            msg.error(
                                &"org.bluez.Error.Rejected".into(),
                                c"Player unavailable or request queue full",
                            )
                        };
                        if owner.is_empty()
                            || msg.sender().map(|s| s.to_string()).as_deref()
                                != Some(owner.as_str())
                        {
                            let _ = c.send(reject());
                            return true;
                        }
                        let interface = msg.interface().map(|v| v.to_string()).unwrap_or_default();
                        let member = msg.member().map(|v| v.to_string()).unwrap_or_default();
                        let mut response = None;
                        if interface == IFACE {
                            let action = match member.as_str() {
                                "Play" => Some(RemoteAction::Play),
                                "Pause" | "Stop" => Some(RemoteAction::Pause),
                                "PlayPause" => Some(RemoteAction::Toggle),
                                "Next" => Some(RemoteAction::Next),
                                "Previous" => Some(RemoteAction::Previous),
                                _ => None,
                            };
                            if let Some(action) = action {
                                if commands.try_send(action).is_ok() {
                                    response = Some(msg.method_return());
                                }
                            }
                        } else if interface == "org.freedesktop.DBus.Properties" {
                            if let Ok(view) = current.lock() {
                                if member == "GetAll"
                                    && msg.read1::<String>().is_ok_and(|v| v == IFACE)
                                {
                                    response = Some(msg.method_return().append1(view.properties()));
                                } else if member == "Get" {
                                    if let Ok((interface, name)) = msg.read2::<String, String>() {
                                        if interface == IFACE {
                                            if let Some(value) = view.properties().remove(&name) {
                                                response = Some(msg.method_return().append1(value));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _ = c.send(response.unwrap_or_else(reject));
                        true
                    }),
                );
                let mut registered = String::new();
                let mut last = View::default();
                let mut refresh = Instant::now() - Duration::from_secs(5);
                while !ending.load(Ordering::Relaxed) {
                    if refresh.elapsed() >= Duration::from_secs(3) {
                        refresh = Instant::now();
                        let result: Result<(String,), _> = c
                            .with_proxy(
                                "org.freedesktop.DBus",
                                "/org/freedesktop/DBus",
                                Duration::from_secs(1),
                            )
                            .method_call("org.freedesktop.DBus", "GetNameOwner", ("org.bluez",));
                        let owner = result.map(|v| v.0).unwrap_or_default();
                        if let Ok(mut a) = authorized.lock() {
                            *a = owner.clone();
                        }
                        if owner != registered && !owner.is_empty() {
                            type Objects = HashMap<Path<'static>, HashMap<String, PropMap>>;
                            let objects: Result<Objects, _> = c
                                .with_proxy(owner.as_str(), "/", Duration::from_secs(1))
                                .get_managed_objects();
                            if let Ok(objects) = objects {
                                if let Some((path, _)) = objects
                                    .iter()
                                    .find(|(_, i)| i.contains_key("org.bluez.Media1"))
                                {
                                    let props =
                                        shared.lock().map(|v| v.properties()).unwrap_or_default();
                                    let result: Result<(), _> = c
                                        .with_proxy(
                                            owner.as_str(),
                                            path.clone(),
                                            Duration::from_secs(1),
                                        )
                                        .method_call(
                                            "org.bluez.Media1",
                                            "RegisterPlayer",
                                            (Path::from(PLAYER), props),
                                        );
                                    if result.is_ok() {
                                        registered = owner.clone();
                                    }
                                }
                            }
                        }
                        if owner.is_empty() {
                            registered.clear();
                        }
                        log.health_set(
                            "avrcp",
                            if registered.is_empty() {
                                HealthState::Unavailable
                            } else {
                                HealthState::Ok
                            },
                            false,
                            if registered.is_empty() {
                                "BlueZ player unavailable"
                            } else {
                                "BlueZ player registered"
                            },
                        );
                    }
                    if c.process(Duration::from_millis(200)).is_err() {
                        break;
                    }
                    let value = shared.lock().map(|v| v.clone()).unwrap_or_default();
                    if !registered.is_empty() && value.notification() != last.notification() {
                        let mut props = value.properties();
                        props.remove("Position");
                        if let Ok(signal) = Message::new_signal(
                            PLAYER,
                            "org.freedesktop.DBus.Properties",
                            "PropertiesChanged",
                        ) {
                            let _ = c.send(signal.append3(IFACE, props, Vec::<String>::new()));
                        }
                        last = value;
                    }
                    log.heartbeat("avrcp", 10);
                }
                // Closing the connection unregisters exactly this player.
            }
        })?;
        Ok(Self {
            actions,
            view,
            stop,
        })
    }
    pub fn publish(&self, model: &AppModel) {
        if let Ok(mut view) = self.view.lock() {
            *view = View::model(model);
        }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_play_pause_are_idempotent_and_metadata_is_only_a_projection() {
        let mut model = AppModel::default();
        assert_eq!(RemoteAction::Play.semantic(&model), Some(Action::PlayPause));
        model.playback = PlaybackState::Playing;
        assert_eq!(RemoteAction::Play.semantic(&model), None);
        assert_eq!(
            RemoteAction::Pause.semantic(&model),
            Some(Action::PlayPause)
        );
        assert_eq!(RemoteAction::Next.semantic(&model), Some(Action::NextTrack));
        assert_eq!(View::model(&model).status, "Playing");
        let v = View::model(&model);
        model.position_ms = 500;
        assert_eq!(v.notification(), View::model(&model).notification());
        model.playback = PlaybackState::Paused;
        assert_eq!(RemoteAction::Pause.semantic(&model), None);
        assert_eq!(View::model(&model).status, "Paused");
    }

    #[test]
    fn private_bus_registration_controls_properties_and_sender_authority() {
        use dbus::{arg::RefArg, channel::Channel};
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
        };
        fn connect(address: &str) -> Result<Connection, dbus::Error> {
            let mut channel = Channel::open_private(address.trim())?;
            channel.register()?;
            Ok(channel.into())
        }
        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut bus = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let service = connect(&address).unwrap();
        service
            .request_name("org.bluez", false, true, false)
            .unwrap();
        let (registered, registration) = sync_channel::<String>(1);
        service.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg, c| {
                let member = msg.member().map(|s| s.to_string()).unwrap_or_default();
                let reply = if member == "GetManagedObjects" {
                    let objects: HashMap<Path<'static>, HashMap<String, PropMap>> =
                        HashMap::from([(
                            Path::from("/org/bluez/hci0"),
                            HashMap::from([("org.bluez.Media1".into(), PropMap::new())]),
                        )]);
                    msg.method_return().append1(objects)
                } else if member == "RegisterPlayer" {
                    let (path, props) = msg.read2::<Path, PropMap>().unwrap();
                    assert_eq!(path.to_string(), PLAYER);
                    assert!(props.contains_key("Metadata"));
                    registered
                        .try_send(msg.sender().unwrap().to_string())
                        .unwrap();
                    msg.method_return()
                } else {
                    msg.method_return()
                };
                c.send(reply).unwrap();
                true
            }),
        );
        let (requests, commands) = sync_channel::<(String, String)>(16);
        let (results, answers) = sync_channel::<Result<String, String>>(16);
        let end = Arc::new(AtomicBool::new(false));
        let ending = end.clone();
        let worker = thread::spawn(move || {
            while !ending.load(Ordering::Relaxed) {
                service.process(Duration::from_millis(10)).unwrap();
                if let Ok((owner, method)) = commands.try_recv() {
                    let proxy = service.with_proxy(owner, PLAYER, Duration::from_secs(2));
                    let response = if method == "GetAll" {
                        let value: Result<(PropMap,), _> = proxy.method_call(
                            "org.freedesktop.DBus.Properties",
                            "GetAll",
                            (IFACE,),
                        );
                        value
                            .map(|(p,)| p["PlaybackStatus"].0.as_str().unwrap().to_owned())
                            .map_err(|e| e.to_string())
                    } else {
                        let value: Result<(), _> = proxy.method_call(IFACE, method, ());
                        value.map(|()| "ok".into()).map_err(|e| e.to_string())
                    };
                    results.try_send(response).unwrap();
                }
            }
        });
        let root = std::env::temp_dir().join(format!("reborn-avrcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let log = Observer::new(&root).unwrap();
        let copy = address.clone();
        let player = Player::spawn_with(log, move || connect(&copy)).unwrap();
        let mut model = AppModel {
            playback: PlaybackState::Playing,
            ..Default::default()
        };
        player.publish(&model);
        let owner = registration.recv_timeout(Duration::from_secs(5)).unwrap();
        requests.send((owner.clone(), "Pause".into())).unwrap();
        assert!(answers
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_ok());
        assert_eq!(
            player.actions.recv_timeout(Duration::from_secs(1)).unwrap(),
            RemoteAction::Pause
        );
        requests.send((owner.clone(), "GetAll".into())).unwrap();
        assert_eq!(
            answers
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap(),
            "Playing"
        );
        // A request never mutates metadata; only the actual runtime model does.
        model.playback = PlaybackState::Paused;
        player.publish(&model);
        requests.send((owner.clone(), "GetAll".into())).unwrap();
        assert_eq!(
            answers
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap(),
            "Paused"
        );
        let unauthorized = connect(&address).unwrap();
        let denied: Result<(), _> = unauthorized
            .with_proxy(owner, PLAYER, Duration::from_secs(2))
            .method_call(IFACE, "Next", ());
        assert!(denied.is_err());
        assert!(player.actions.try_recv().is_err());
        drop(player);
        end.store(true, Ordering::Relaxed);
        worker.join().unwrap();
    }
}

#![forbid(unsafe_code)]
use dbus::{
    arg::{PropMap, RefArg},
    blocking::{
        stdintf::org_freedesktop_dbus::{ObjectManager, Properties},
        Connection,
    },
    channel::{MatchingReceiver, Sender},
    message::MatchRule,
    Message, Path as DbusPath,
};
use reborn_core::atomic_write;
use reborn_observability::{HealthState, Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
    pub connected: bool,
    pub audio: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub available: bool,
    pub powered: bool,
    pub discovering: bool,
    pub bluealsa: bool,
    pub pcms: Vec<serde_json::Value>,
    pub devices: Vec<Device>,
    pub pending: Option<Pairing>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub device: String,
    pub display: String,
}
pub enum Command {
    Power(bool),
    Scan(bool),
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    Confirm(bool),
    Refresh,
    Stop,
}
pub struct Bluetooth {
    pub commands: SyncSender<Command>,
    pub events: Receiver<Status>,
}
struct Pending {
    message: Message,
    display: Pairing,
    started: Instant,
}
type Operations = Arc<Mutex<HashMap<u32, (u64, Instant, String)>>>;
type Objects = HashMap<DbusPath<'static>, HashMap<String, PropMap>>;
fn objects(c: &Connection) -> Result<Objects, String> {
    c.with_proxy("org.bluez", "/", Duration::from_secs(2))
        .get_managed_objects()
        .map_err(|e| {
            format!(
                "D-Bus org.bluez ObjectManager: {}",
                e.name().unwrap_or("unknown")
            )
        })
}
fn text(p: &PropMap, k: &str) -> String {
    p.get(k).and_then(|v| v.0.as_str()).unwrap_or("").into()
}
fn yes(p: &PropMap, k: &str) -> bool {
    p.get(k).and_then(|v| v.0.as_i64()).unwrap_or(0) != 0
}
fn status(c: &Connection) -> Result<(String, Status), String> {
    let mut state = Status::default();
    let mut adapter = String::new();
    for (path, interfaces) in objects(c)? {
        if let Some(p) = interfaces.get("org.bluez.Adapter1") {
            if adapter.is_empty() {
                adapter = path.to_string();
                state.available = true;
                state.powered = yes(p, "Powered");
                state.discovering = yes(p, "Discovering");
            }
        }
        if let Some(p) = interfaces.get("org.bluez.Device1") {
            let audio = p
                .get("UUIDs")
                .and_then(|v| v.0.as_iter())
                .is_some_and(|mut a| {
                    a.any(|v| v.as_str().is_some_and(|s| s.starts_with("0000110b")))
                });
            state.devices.push(Device {
                path: path.to_string(),
                name: text(p, "Alias"),
                address: text(p, "Address"),
                paired: yes(p, "Paired"),
                connected: yes(p, "Connected"),
                audio,
            });
        }
    }
    let reply: Result<(bool,), _> = c
        .with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(1),
        )
        .method_call("org.freedesktop.DBus", "NameHasOwner", ("org.bluealsa",));
    state.bluealsa = reply.is_ok_and(|r| r.0);
    let pcms: Result<(HashMap<DbusPath<'static>, PropMap>,), _> = c
        .with_proxy("org.bluealsa", "/org/bluealsa", Duration::from_secs(1))
        .method_call("org.bluealsa.Manager1", "GetPCMs", ());
    if let Ok((pcms,)) = pcms {
        state.pcms=pcms.into_iter().take(16).map(|(path,p)|json!({"object":path.to_string(),"transport":text(&p,"Transport"),"mode":text(&p,"Mode"),"codec":text(&p,"Codec"),"sampling":p.get("Sampling").and_then(|v|v.0.as_u64()),"channels":p.get("Channels").and_then(|v|v.0.as_u64())})).collect();
    }
    state.devices.sort_by(|a, b| a.name.cmp(&b.name));
    state.devices.truncate(128);
    Ok((adapter, state))
}
fn dbus_error(e: dbus::Error) -> String {
    format!("D-Bus org.bluez: {}", e.name().unwrap_or("unknown"))
}
fn call(c: &Connection, path: &str, interface: &str, method: &str) -> Result<(), String> {
    let _: () = c
        .with_proxy("org.bluez", path, Duration::from_secs(5))
        .method_call(interface, method, ())
        .map_err(dbus_error)?;
    Ok(())
}
fn power(c: &Connection, adapter: &str, on: bool, persist: bool) -> Result<(), String> {
    c.with_proxy("org.bluez", adapter, Duration::from_secs(15))
        .set("org.bluez.Adapter1", "Powered", on)
        .map_err(dbus_error)?;
    if persist {
        atomic_write(
            Path::new("/data/bluetooth/enabled"),
            if on { b"1\n" } else { b"0\n" },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn rejected(msg: &Message) -> Message {
    msg.error(
        &"org.bluez.Error.Rejected".into(),
        c"Pairing requires explicit user confirmation",
    )
}
impl Bluetooth {
    pub fn spawn(log: Observer) -> std::io::Result<Self> {
        let (tx, rx) = sync_channel(8);
        let (et, er) = sync_channel(2);
        thread::Builder::new()
            .name("bluetooth".into())
            .spawn(move || {
                let mut connected: Vec<String> = vec![];
                loop {
                    let c = match Connection::new_system() {
                        Ok(c) => c,
                        Err(_) => {
                            log.health_set(
                                "bluetooth",
                                HealthState::Unavailable,
                                false,
                                "system D-Bus unavailable",
                            );
                            let _ = et.try_send(Status {
                                error: Some("system D-Bus unavailable".into()),
                                ..Default::default()
                            });
                            match rx.recv_timeout(Duration::from_secs(3)) {
                                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                                _ => continue,
                            }
                        }
                    };
                    let operations:Operations=Arc::new(Mutex::new(HashMap::new()));
                    for kind in [dbus::MessageType::MethodReturn,dbus::MessageType::Error] {
                        let ops=operations.clone();let observer=log.clone();let mut rule=MatchRule::new();rule.msg_type=Some(kind);
                        c.start_receive(rule,Box::new(move|mut msg,_|{
                            if let Some(serial)=msg.get_reply_serial(){if let Ok(mut ops)=ops.lock(){if let Some((id,_,method))=ops.remove(&serial){
                                match msg.as_result(){Ok(_)=>observer.emit(Level::Info,"bluetooth","operation_completed","BlueZ operation completed",Some(id),json!({"method":method})),Err(e)=>{observer.add("bluetooth_errors",1.);observer.emit(Level::Error,"bluetooth","operation_failed",&dbus_error(e),Some(id),json!({"interface":"org.bluez.Device1","method":method,"recovery_attempted":false}));}}
                            }}}true
                        }));
                    }
                    let pending: Arc<Mutex<Option<Pending>>> = Arc::new(Mutex::new(None));
                    let handler = pending.clone();
                    let mut rule = MatchRule::new_method_call();
                    rule.interface = Some("org.bluez.Agent1".into());
                    rule.path = Some("/org/reborn/agent".into());
                    c.start_receive(
                        rule,
                        Box::new(move |msg, c| {
                            let member = msg.member().map(|s| s.to_string()).unwrap_or_default();
                            match member.as_str() {
                                "Release" | "Cancel" => {
                                    if let Ok(mut p) = handler.lock() {
                                        if let Some(old) = p.take() {
                                            let _ = c.send(rejected(&old.message));
                                        }
                                    }
                                    let _ = c.send(msg.method_return());
                                }
                                "RequestConfirmation"
                                | "RequestAuthorization"
                                | "AuthorizeService" => {
                                    let device = msg
                                        .read1::<DbusPath>()
                                        .map(|s| s.to_string())
                                        .unwrap_or_default();
                                    let display = if member == "RequestConfirmation" {
                                        msg.read2::<DbusPath, u32>()
                                            .map(|(_, n)| format!("Confirm {n:06}"))
                                            .unwrap_or_else(|_| "Confirm pairing".into())
                                    } else {
                                        "Allow audio device?".into()
                                    };
                                    if let Ok(mut p) = handler.lock() {
                                        if p.is_some() {
                                            let _ = c.send(rejected(&msg));
                                        } else {
                                            *p = Some(Pending {
                                                message: msg,
                                                display: Pairing { device, display },
                                                started: Instant::now(),
                                            });
                                        }
                                    }
                                }
                                // Baseline agent advertises DisplayYesNo; legacy PIN/keyboard requests are explicitly rejected.
                                _ => {
                                    let _ = c.send(rejected(&msg));
                                }
                            }
                            true
                        }),
                    );
                    let registered: Result<(), dbus::Error> = c
                        .with_proxy("org.bluez", "/org/bluez", Duration::from_secs(3))
                        .method_call(
                            "org.bluez.AgentManager1",
                            "RegisterAgent",
                            (DbusPath::from("/org/reborn/agent"), "DisplayYesNo"),
                        );
                    if let Err(e) = registered {
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "agent_unavailable",
                            &dbus_error(e),
                            None,
                            json!({}),
                        );
                    }
                    let mut refresh = Instant::now() - Duration::from_secs(5);
                    let mut current = Status::default();
                    let mut adapter = String::new();
                    loop {
                        if c.process(Duration::from_millis(20)).is_err(){break}
                        if let Ok(mut ops)=operations.lock(){ops.retain(|_,(id,start,method)|{if start.elapsed()>Duration::from_secs(70){log.add("bluetooth_errors",1.);log.emit(Level::Error,"bluetooth","operation_timeout","BlueZ operation deadline exceeded",Some(*id),json!({"method":method}));false}else{true}});}
                        if refresh.elapsed() > Duration::from_secs(2) {
                            match status(&c) {
                                Ok((a, s)) => {
                                    adapter = a;
                                    current = s
                                }
                                Err(e) => {
                                    current.available = false;
                                    current.error = Some(e)
                                }
                            }
                            refresh = Instant::now();
                        }
                        if let Ok(mut p) = pending.lock() {
                            if p.as_ref()
                                .is_some_and(|v| v.started.elapsed() > Duration::from_secs(60))
                            {
                                if let Some(v) = p.take() {
                                    let _ = c.send(rejected(&v.message));
                                }
                            }
                            current.pending = p.as_ref().map(|v| v.display.clone());
                        }
                        let command = match rx.try_recv() {
                            Ok(c) => Some(c),
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                            Err(_) => None,
                        };
                        let mut changed = false;
                        if let Some(cmd) = command {
                            if matches!(cmd, Command::Stop) {
                                return;
                            }
                            let id = log.correlation();
                            let operation = match &cmd {
                                Command::Pair(_) => "Pair",
                                Command::Connect(_) => "Connect",
                                Command::Disconnect(_) => "Disconnect",
                                _ => "RemoveDevice",
                            };
                            let result = match cmd {
                                Command::Stop | Command::Refresh => Ok(()),
                                Command::Power(on) => power(&c, &adapter, on, true),
                                Command::Scan(on) => call(
                                    &c,
                                    &adapter,
                                    "org.bluez.Adapter1",
                                    if on {
                                        "StartDiscovery"
                                    } else {
                                        "StopDiscovery"
                                    },
                                ),
                                Command::Confirm(accept) => {
                                    if let Ok(mut p) = pending.lock() {
                                        if let Some(v) = p.take() {
                                            let _ = c.send(if accept {
                                                v.message.method_return()
                                            } else {
                                                rejected(&v.message)
                                            });
                                        }
                                    }
                                    Ok(())
                                }
                                Command::Pair(path)
                                | Command::Connect(path)
                                | Command::Disconnect(path)
                                | Command::Forget(path) => {
                                    let member = operation;
                                    if !current.devices.iter().any(|d| d.path == path) {
                                        Err("unknown discovered Bluetooth device".into())
                                    } else if member == "RemoveDevice" {
                                        let r: Result<(), _> = c
                                            .with_proxy(
                                                "org.bluez",
                                                adapter.as_str(),
                                                Duration::from_secs(3),
                                            )
                                            .method_call(
                                                "org.bluez.Adapter1",
                                                member,
                                                (DbusPath::from(path),),
                                            );
                                        r.map_err(dbus_error)
                                    } else {
                                        // Send asynchronous method calls on the Agent's own connection so callbacks are dispatched by this worker.
                                        let message = Message::new_method_call(
                                            "org.bluez",
                                            path,
                                            "org.bluez.Device1",
                                            member,
                                        )
                                        .map_err(|_| "invalid D-Bus path".to_string());
                                        message.and_then(|m| {
                                            c.send(m)
                                                .map(|serial| {if let Ok(mut ops)=operations.lock(){if ops.len()<16{ops.insert(serial,(id,Instant::now(),member.into()));}}})
                                                .map_err(|_| "D-Bus send failed".into())
                                        })
                                    }
                                }
                            };
                            if let Err(e) = result {
                                current.error = Some(e.clone());
                                log.add("bluetooth_errors", 1.);
                                log.emit(
                                    Level::Warn,
                                    "bluetooth",
                                    "operation_failed",
                                    &e,
                                    Some(id),
                                    json!({"recovery_attempted":false}),
                                );
                            } else {
                                log.emit(
                                    Level::Info,
                                    "bluetooth",
                                    "operation_requested",
                                    "BlueZ operation sent",
                                    Some(id),
                                    json!({}),
                                );
                            }
                            changed = true;
                            refresh = Instant::now() - Duration::from_secs(5);
                        }
                        let now = current
                            .devices
                            .iter()
                            .filter(|d| d.connected)
                            .map(|d| d.path.clone())
                            .collect::<Vec<_>>();
                        for p in &now {
                            if !connected.contains(p) {
                                log.add("bluetooth_connects", 1.);
                                changed = true
                            }
                        }
                        for p in &connected {
                            if !now.contains(p) {
                                log.add("bluetooth_disconnects", 1.);
                                changed = true
                            }
                        }
                        connected = now;
                        log.health_set(
                            "bluetooth",
                            if current.available {
                                HealthState::Ok
                            } else {
                                HealthState::Unavailable
                            },
                            false,
                            if current.available {
                                "BlueZ adapter available"
                            } else {
                                "BlueZ unavailable"
                            },
                        );
                        if changed
                            || refresh.elapsed() < Duration::from_millis(80)
                            || current.pending.is_some()
                        {
                            let _ = et.try_send(current.clone());
                        }
                    }
                }
            })?;
        Ok(Self {
            commands: tx,
            events: er,
        })
    }
}
pub fn service_test() -> Result<serde_json::Value, String> {
    let c = Connection::new_system().map_err(dbus_error)?;
    let (_, s) = status(&c)?;
    Ok(
        json!({"passed":s.available,"adapter":s.available,"bluealsa":s.bluealsa,"connected":s.devices.iter().filter(|d|d.connected).count()}),
    )
}
pub fn scan_test(seconds: u64) -> Result<serde_json::Value, String> {
    if !(1..=15).contains(&seconds) {
        return Err("scan duration 1..15 seconds".into());
    }
    let c = Connection::new_system().map_err(dbus_error)?;
    let (adapter, before) = status(&c)?;
    let result = (|| {
        if !before.powered {
            power(&c, &adapter, true, false)?
        }
        if !before.discovering {
            call(&c, &adapter, "org.bluez.Adapter1", "StartDiscovery")?
        }
        thread::sleep(Duration::from_secs(seconds));
        let (_, s) = status(&c)?;
        Ok(json!({"passed":true,"count":s.devices.len()}))
    })();
    if !before.discovering {
        let _ = call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery");
    }
    if !before.powered {
        let _ = power(&c, &adapter, false, false);
    }
    result
}

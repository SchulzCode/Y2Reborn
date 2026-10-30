//! The Bluetooth thread: connects to the system D-Bus (again after it went away), registers the pairing
//! agent, watches BlueZ and BlueALSA, carries out the commands and publishes the status.
use super::*;
use std::ops::ControlFlow;

/// What the thread owns.
pub(super) struct BluetoothWorker<C> {
    pub(super) log: Observer,
    pub(super) connect: C,
    pub(super) preference: PathBuf,
    pub(super) rx: Receiver<Command>,
    pub(super) et: EventSender,
    pub(super) wake_receiver: UnixStream,
}

impl<C: Fn() -> Result<Connection, dbus::Error> + Send + 'static> BluetoothWorker<C> {
    pub(super) fn run(self) {
        let BluetoothWorker {
            log,
            connect,
            rx,
            et,
            wake_receiver,
            ..
        } = &self;
        let mut connected: Vec<String> = vec![];
        loop {
            let c = match connect() {
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
                        Ok(_) => {
                            let _ = (&*wake_receiver).read(&mut [0u8]);
                            continue;
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                    }
                }
            };
            if self.session(c, &mut connected).is_break() {
                return;
            }
        }
    }

    /// One connection to the system D-Bus: the agent, the watches and the event loop until the bus goes
    /// away (`Continue`: connect again) or the thread ends (`Break`).
    fn session(&self, c: Connection, connected: &mut Vec<String>) -> ControlFlow<()> {
        let BluetoothWorker {
            log,
            preference,
            rx,
            et,
            wake_receiver,
            ..
        } = self;
        c.set_signal_match_mode(true);
        let observation_dirty = Arc::new(AtomicBool::new(true));
        let operations: Operations = Arc::new(Mutex::new(HashMap::new()));
        // (error, device of a Connect that BlueZ refused as busy: tried again)
        let (completed_tx, completed_rx) = sync_channel::<(Option<String>, Option<String>)>(16);
        for kind in [dbus::MessageType::MethodReturn, dbus::MessageType::Error] {
            let ops = operations.clone();
            let observer = log.clone();
            let completed = completed_tx.clone();
            let dirty = observation_dirty.clone();
            let mut rule = MatchRule::new();
            rule.msg_type = Some(kind);
            c.start_receive(rule,Box::new(move|mut msg,connection|{
                if let Some(serial)=msg.get_reply_serial(){if let Ok(mut ops)=ops.lock(){if let Some(mut operation)=ops.remove(&serial){
                    dirty.store(true,Ordering::Release);
                    match operation.reply(connection, &mut msg) {
                        Ok(Some(next)) => {
                            observer.emit(Level::Info,"bluetooth","pair_bonded","Bond saved; audio connection requested",Some(operation.id),json!({"trusted":true}));
                            ops.insert(next, operation);
                        }
                        Ok(None) => {
                            observer.emit(Level::Info,"bluetooth","operation_completed","BlueZ operation completed",Some(operation.id),json!({"method":operation.method}));
                            let _ = completed.try_send((None, None));
                        }
                        Err(mut error) => {
                            if let Err(cleanup) = operation.restore_pairable(connection) { error.push_str(&format!("; Pairable restoration failed: {cleanup}")); }
                            if operation.lease.is_some() { let _=write_intent(failed_intent(&error)); }
                            observer.add("bluetooth_errors",1.);
                            let busy=(operation.method=="Connect" && connect_busy(&error)).then(|| operation.path.clone());
                            observer.emit(Level::Error,"bluetooth","operation_failed",&error,Some(operation.id),json!({"interface":"org.bluez.Device1","method":operation.method,"recovery_attempted":busy.is_some()}));
                            let _ = completed.try_send((Some(error), busy));
                        }
                    }
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
                    "RequestConfirmation" | "RequestAuthorization" | "AuthorizeService" => {
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
        let mut agent_owner = name_owner(&c, "org.bluez");
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
        let transport_invalidations = Arc::new(Mutex::new(HashSet::new()));
        let invalidation_sink = transport_invalidations.clone();
        let daemon_invalidated = Arc::new(AtomicBool::new(false));
        let daemon_invalidation_sink = daemon_invalidated.clone();
        let owner_dirty = observation_dirty.clone();
        let agent_invalidated = Arc::new(AtomicBool::new(false));
        let agent_invalidation_sink = agent_invalidated.clone();
        let mut transport_watches_ready = true;
        let mut owner_changed = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged");
        owner_changed.sender = Some("org.freedesktop.DBus".into());
        owner_changed.path = Some(DbusPath::from("/org/freedesktop/DBus"));
        if let Err(error) =
            c.add_match::<(String, String, String), _>(owner_changed, move |(name, _, _), _, _| {
                if name == "org.bluez" {
                    agent_invalidation_sink.store(true, Ordering::Release);
                }
                if name == "org.bluez" || name == "org.bluealsa" {
                    daemon_invalidation_sink.store(true, Ordering::Release);
                    owner_dirty.store(true, Ordering::Release);
                }
                true
            })
        {
            transport_watches_ready = false;
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_owner_watch_failed",
                &dbus_error(error),
                None,
                json!({"recovery":"property_and_owner_snapshot_only"}),
            );
        }
        let mut pcm_removed =
            MatchRule::new_signal("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved");
        pcm_removed.sender = Some("org.bluealsa".into());
        pcm_removed.path = Some(DbusPath::from("/org/bluealsa"));
        if let Err(error) = c.add_match::<(DbusPath, Vec<String>), _>(
            pcm_removed,
            move |(object, interfaces), _, _| {
                if interfaces.iter().any(|name| name == "org.bluealsa.PCM1") {
                    if let Ok(mut invalidated) = invalidation_sink.lock() {
                        invalidated.insert(object.to_string());
                    }
                }
                true
            },
        ) {
            transport_watches_ready = false;
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_remove_watch_failed",
                &dbus_error(error),
                None,
                json!({"recovery":"periodic_snapshot_only"}),
            );
        }
        let property_invalidation_sink = transport_invalidations.clone();
        let mut properties_changed =
            MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged");
        properties_changed.sender = Some("org.bluealsa".into());
        if let Err(error) = c.add_match::<(String, PropMap, Vec<String>), _>(
            properties_changed,
            move |(interface, changed, invalidated), _, message| {
                let relevant = [
                    "Device",
                    "Transport",
                    "Mode",
                    "Codec",
                    "Format",
                    "Rate",
                    "Channels",
                ];
                if interface == "org.bluealsa.PCM1"
                    && relevant.iter().any(|name| {
                        changed.contains_key(*name) || invalidated.iter().any(|value| value == name)
                    })
                {
                    if let (Some(path), Ok(mut objects)) =
                        (message.path(), property_invalidation_sink.lock())
                    {
                        objects.insert(path.to_string());
                    }
                }
                true
            },
        ) {
            transport_watches_ready = false;
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_property_watch_failed",
                &dbus_error(error),
                None,
                json!({"recovery":"periodic_snapshot_only"}),
            );
        }
        // Subscribe before the initial snapshot so additions, removals,
        // device/adapter state and PCM Running changes cannot wait for
        // a periodic refresh. RSSI/encoder statistics do not enumerate.
        for service in ["org.bluez", "org.bluealsa"] {
            for member in ["InterfacesAdded", "InterfacesRemoved", "PropertiesChanged"] {
                let mut rule = MatchRule::new_signal(
                    if member == "PropertiesChanged" {
                        "org.freedesktop.DBus.Properties"
                    } else {
                        "org.freedesktop.DBus.ObjectManager"
                    },
                    member,
                );
                rule.sender = Some(service.into());
                let dirty = observation_dirty.clone();
                let registered = c.add_match::<(), _>(rule, move |_, _, msg| {
                    let relevant = if member == "PropertiesChanged" {
                        msg.read3::<String, PropMap, Vec<String>>().is_ok_and(
                            |(interface, changed, invalidated)| {
                                let keys: &[&str] = match interface.as_str() {
                                    "org.bluez.Adapter1" => &["Powered", "Discovering"],
                                    "org.bluez.Device1" => &[
                                        "Alias",
                                        "Address",
                                        "UUIDs",
                                        "Paired",
                                        "Bonded",
                                        "Trusted",
                                        "Connected",
                                    ],
                                    "org.bluealsa.PCM1" => &[
                                        "Device",
                                        "Transport",
                                        "Mode",
                                        "Codec",
                                        "Format",
                                        "Rate",
                                        "Channels",
                                        "Running",
                                    ],
                                    "org.bluez.MediaTransport1" => &["Volume"],
                                    _ => &[],
                                };
                                keys.iter().any(|key| {
                                    changed.contains_key(*key)
                                        || invalidated.iter().any(|v| v == key)
                                })
                            },
                        )
                    } else {
                        true
                    };
                    if relevant {
                        dirty.store(true, Ordering::Release);
                    }
                    true
                });
                if let Err(error) = registered {
                    transport_watches_ready = false;
                    log.emit(
                        Level::Warn,
                        "bluetooth",
                        "state_watch_failed",
                        &dbus_error(error),
                        None,
                        json!({"service":service,"member":member}),
                    );
                }
            }
        }
        let mut transport_epochs = TransportEpochs::default();
        let mut refresh = Instant::now() - Duration::from_secs(30);
        let mut current = Status::default();
        let mut adapter = String::new();
        let mut discovery: Option<Discovery> = None;
        let mut operation_error = None;
        // A codec choice refused only because the peer was not ready yet
        // (connecting, PCM not registered, another operation running) is
        // tried again: it was dropped (reports 61caa81e/297f1032).
        let mut codec_retry: Option<CodecRetry> = None;
        let mut busy_retry: Option<BusyRetry> = None;
        // The window of the user's current waiting Connect, kept
        // across retries so a busy answer cannot extend it.
        let mut busy_window: Option<Instant> = None;
        let mut unpublished = false;
        let mut commands_pending = false;
        loop {
            let mut disconnected = false;
            for index in 0..64 {
                match c.process(Duration::ZERO) {
                    Ok(false) => break,
                    Ok(true) => {
                        if index == 63 {
                            observation_dirty.store(true, Ordering::Release);
                        }
                    }
                    Err(_) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            let now = Instant::now();
            let mut until = refresh + Duration::from_secs(30);
            if commands_pending {
                until = now;
            }
            if unpublished {
                until = until.min(now + Duration::from_millis(100));
            }
            if observation_dirty.load(Ordering::Acquire) {
                until = now;
            }
            if let Some(scan) = &discovery {
                until = until.min(scan.until);
            }
            if let Some(retry) = &codec_retry {
                until = until.min(retry.due);
            }
            if let Some(retry) = &busy_retry {
                until = until.min(retry.due);
            }
            if let Ok(ops) = operations.lock() {
                for operation in ops.values() {
                    until = until.min(operation.started + Duration::from_secs(70));
                }
            }
            if let Ok(p) = pending.lock() {
                if current.pending.as_ref().map(|v| (&v.device, &v.display))
                    != p.as_ref().map(|v| (&v.display.device, &v.display.display))
                {
                    until = now;
                }
                if let Some(p) = p.as_ref() {
                    until = until.min(p.started + Duration::from_secs(60));
                }
            }
            let wait = until.saturating_duration_since(now);
            let watch = c.channel().watch();
            let result = crate::native::bluetooth_wait(
                watch.fd,
                watch.read,
                watch.write,
                wake_receiver.as_raw_fd(),
                wait,
            )
            .map_err(|e| e.to_string())
            .and_then(|()| {
                // Drain buffered messages before sleeping again. A
                // zero wait also flushes pending async method calls.
                for _ in 0..64 {
                    if !c.process(Duration::ZERO).map_err(dbus_error)? {
                        break;
                    }
                }
                Ok(())
            });
            if disconnected || result.is_err() {
                cancel_operations(&c, &operations);
                current.available = false;
                current.error = Some("Bluetooth service disconnected".into());
                if current.scan.active() {
                    current.scan = RadioScan::Failed {
                        message: "Bluetooth service disconnected".into(),
                    };
                }
                let _ = et.try_send(current);
                break;
            }
            // Drain notifications independently of queue consumption:
            // a command can be consumed during D-Bus activity before
            // its sender writes the wake byte. Leaving that late byte
            // unread would make poll permanently ready.
            while (&*wake_receiver).read(&mut [0u8; 64]).is_ok_and(|n| n > 0) {}
            let mut changed = false;
            if let Ok(mut ops) = operations.lock() {
                ops.retain(|_, operation| {
                    if operation.started.elapsed() > Duration::from_secs(70) {
                        operation.cancel(&c);
                        log.add("bluetooth_errors", 1.);
                        log.emit(
                            Level::Error,
                            "bluetooth",
                            "operation_timeout",
                            "BlueZ operation deadline exceeded",
                            Some(operation.id),
                            json!({"method":operation.method}),
                        );
                        operation_error = Some("Bluetooth operation timed out".into());
                        current.error = operation_error.clone();
                        changed = true;
                        false
                    } else {
                        true
                    }
                });
            }
            while let Ok((error, busy)) = completed_rx.try_recv() {
                if let Some(path) = busy {
                    let now = Instant::now();
                    let until = *busy_window.get_or_insert(now + BUSY_RETRY_FOR);
                    if now < until && busy_retry.is_none() {
                        current.waiting = Some(path.clone());
                        busy_retry = Some(BusyRetry {
                            command: Command::Connect(path),
                            due: now + BUSY_RETRY_DELAY,
                            until,
                        });
                        changed = true;
                        continue;
                    }
                }
                if busy_retry.is_none() && current.waiting.take().is_some() {
                    busy_window = None;
                }
                operation_error = error;
                current.error = operation_error.clone();
                refresh = Instant::now() - Duration::from_secs(30);
                changed = true;
            }
            if observation_dirty.swap(false, Ordering::AcqRel)
                || refresh.elapsed() >= Duration::from_secs(30)
            {
                if agent_invalidated.swap(false, Ordering::AcqRel) {
                    agent_owner = None;
                }
                match status(&c) {
                    Ok((a, mut s)) => {
                        if !transport_watches_ready {
                            // A same-path daemon/object restart could otherwise
                            // survive the periodic snapshot. Refuse to expose a
                            // playable transport if its invalidation watches failed.
                            s.pcms.clear();
                        }
                        let invalidated = transport_invalidations
                            .lock()
                            .map(|mut objects| std::mem::take(&mut *objects))
                            .unwrap_or_default();
                        transport_epochs.refresh(
                            &mut s,
                            &invalidated,
                            daemon_invalidated.swap(false, Ordering::AcqRel),
                        );
                        if current.bluez_owner != s.bluez_owner {
                            cancel_operations(&c, &operations);
                        }
                        if agent_owner != s.bluez_owner {
                            agent_owner = s.bluez_owner.clone();
                            if let Some(owner) = agent_owner.as_deref() {
                                let registration: Result<(), dbus::Error> = c
                                    .with_proxy(owner, "/org/bluez", Duration::from_secs(3))
                                    .method_call(
                                        "org.bluez.AgentManager1",
                                        "RegisterAgent",
                                        (DbusPath::from("/org/reborn/agent"), "DisplayYesNo"),
                                    );
                                if let Err(error) = registration {
                                    log.emit(
                                        Level::Warn,
                                        "bluetooth",
                                        "agent_unavailable",
                                        &dbus_error(error),
                                        None,
                                        json!({}),
                                    );
                                }
                            }
                        }
                        adapter = a;
                        s.codec_policy =
                            refresh_codec_policy(current.codec_policy.as_ref(), &s, || {
                                discover_codec_policy(
                                    &c,
                                    &s,
                                    Path::new("/etc/y2linux/bluetooth-codecs.json"),
                                    crate::codecs::experimental_enabled(),
                                )
                            });
                        s.scan = current.scan.clone();
                        s.error = operation_error.clone();
                        current = s
                    }
                    Err(e) => {
                        cancel_operations(&c, &operations);
                        current.available = false;
                        current.pcms.clear();
                        current.codec_policy = None;
                        current.devices.clear();
                        current.bluez_owner = None;
                        current.bluealsa_owner = None;
                        current.bluealsa = false;
                        current.error = Some(e)
                    }
                }
                refresh = Instant::now();
                changed = true;
            }
            if discovery.is_some() && (!current.available || !current.powered) {
                let message = "Bluetooth radio became unavailable during scan".to_string();
                if let Some(job) = discovery.take() {
                    log.add("bluetooth_errors", 1.);
                    log.emit(
                        Level::Warn,
                        "bluetooth",
                        "scan_failed",
                        &message,
                        Some(job.id),
                        json!({"recovery_attempted":false}),
                    );
                }
                current.scan = RadioScan::Failed {
                    message: message.clone(),
                };
                current.error = Some(message.clone());
                operation_error = Some(message);
                changed = true;
            }
            if discovery
                .as_ref()
                .is_some_and(|d| d.expired(Instant::now()))
            {
                let job = discovery.take().unwrap();
                // Stop only the discovery session held by this D-Bus connection.
                match call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery") {
                    Ok(()) => {
                        current.scan = RadioScan::Complete {
                            found: current.devices.len(),
                        };
                        current.discovering = false;
                        log.emit(
                            Level::Info,
                            "bluetooth",
                            "scan_complete",
                            "Bluetooth scan completed",
                            Some(job.id),
                            json!({"count":current.devices.len()}),
                        );
                    }
                    Err(e) => {
                        current.scan = RadioScan::Failed { message: e.clone() };
                        current.error = Some(e.clone());
                        operation_error = Some(e.clone());
                        log.add("bluetooth_errors", 1.);
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "scan_failed",
                            &e,
                            Some(job.id),
                            json!({"method":"StopDiscovery","recovery_attempted":false}),
                        );
                    }
                }
                refresh = Instant::now() - Duration::from_secs(30);
                changed = true;
            }
            if let Ok(mut p) = pending.lock() {
                if p.as_ref()
                    .is_some_and(|v| v.started.elapsed() > Duration::from_secs(60))
                {
                    if let Some(v) = p.take() {
                        let _ = c.send(rejected(&v.message));
                    }
                }
                let display = p.as_ref().map(|v| v.display.clone());
                changed |= current.pending.as_ref().map(|v| (&v.device, &v.display))
                    != display.as_ref().map(|v| (&v.device, &v.display));
                current.pending = display;
            }
            let mut codec_attempt = 0u8;
            let mut busy_until = None;
            let command = match rx.try_recv() {
                Ok(c) => {
                    if matches!(c, Command::Codec { .. }) {
                        codec_retry = None; // a newer choice replaces it
                    }
                    if !matches!(
                        c,
                        Command::Codec { .. }
                            | Command::Refresh
                            | Command::Scan(_)
                            | Command::Confirm(_)
                    ) {
                        busy_window = None;
                        if busy_retry.take().is_some() {
                            current.waiting = None; // so does a newer device or radio choice
                            changed = true;
                        }
                    }
                    Some(c)
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    cancel_operations(&c, &operations);
                    return ControlFlow::Break(());
                }
                Err(_) => match codec_retry.take() {
                    Some(retry) if retry.due <= Instant::now() => {
                        codec_attempt = retry.attempt;
                        Some(Command::Codec {
                            address: retry.address,
                            preference: retry.preference,
                        })
                    }
                    other => {
                        codec_retry = other;
                        match busy_retry.take() {
                            // The automatic reconnect brought this very device back.
                            Some(BusyRetry {
                                command: Command::Connect(path),
                                ..
                            }) if current
                                .devices
                                .iter()
                                .any(|d| d.path == path && d.connected) =>
                            {
                                current.waiting = None;
                                changed = true;
                                None
                            }
                            Some(retry) if retry.due <= Instant::now() => {
                                busy_until = Some(retry.until);
                                busy_window = Some(retry.until);
                                Some(retry.command)
                            }
                            other => {
                                busy_retry = other;
                                None
                            }
                        }
                    }
                },
            };
            commands_pending = command.is_some();
            if let Some(cmd) = command {
                if matches!(cmd, Command::Stop) {
                    cancel_operations(&c, &operations);
                    return ControlFlow::Break(());
                }
                // The headphones' volume is no operation: no error
                // state, no busy wait; a refused step only logs, the
                // next one tries again.
                if let Command::Volume { device, volume } = &cmd {
                    if let (Some(owner), Some(transport)) = (
                        current.bluez_owner.as_deref(),
                        current
                            .devices
                            .iter()
                            .find(|d| &d.path == device)
                            .and_then(|d| d.transport.as_deref()),
                    ) {
                        let set = c.with_proxy(owner, transport, Duration::from_secs(1)).set(
                            "org.bluez.MediaTransport1",
                            "Volume",
                            u16::from((*volume).min(127)),
                        );
                        if let Err(error) = set {
                            log.emit(
                                Level::Warn,
                                "bluetooth",
                                "volume_failed",
                                &dbus_error(error),
                                None,
                                json!({"volume":volume}),
                            );
                        }
                    }
                    continue;
                }
                let id = log.correlation();
                let operation = match &cmd {
                    Command::Power(_) => "Powered",
                    Command::Scan(true) => "StartDiscovery",
                    Command::Scan(false) => "StopDiscovery",
                    Command::Pair(_) => "Pair",
                    Command::Connect(_) => "Connect",
                    Command::Disconnect(_) => "Disconnect",
                    Command::Forget(_) => "RemoveDevice",
                    Command::Confirm(_) => "ConfirmPairing",
                    Command::Codec { .. } => "SelectCodec",
                    _ => "Refresh",
                };
                // Repeated presses while starting/scanning do not cancel or
                // restart the bounded operation.
                if matches!(cmd, Command::Scan(true)) && discovery.is_some() {
                    continue;
                }
                let scan_command = matches!(cmd, Command::Scan(_));
                let codec_args = match &cmd {
                    Command::Codec {
                        address,
                        preference,
                    } => Some((address.clone(), *preference)),
                    _ => None,
                };
                let busy_command = waits_for_platform_lock(&cmd).then(|| cmd.clone());
                if !matches!(cmd, Command::Refresh) {
                    operation_error = None;
                    current.error = None;
                }
                let result = match cmd {
                    Command::Stop | Command::Refresh | Command::Volume { .. } => Ok(()),
                    Command::Codec {
                        address,
                        preference,
                    } => match codec_request(&c, &current, &address, preference) {
                        Ok(session) => {
                            let error = session.reason.clone();
                            current.codec_policy = Some(session);
                            error.map_or(Ok(()), Err)
                        }
                        Err(error) => Err(error),
                    },
                    Command::Power(on) => {
                        if discovery.is_none() {
                            current.scan = RadioScan::Idle;
                        }
                        if !on {
                            if discovery.take().is_some() {
                                let _ = call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery");
                            }
                            current.scan = RadioScan::Idle;
                        }
                        power_at(&c, &adapter, on, true, preference)
                    }
                    Command::Scan(true) => {
                        current.scan = RadioScan::Starting;
                        let _ = et.try_send(current.clone());
                        log.emit(
                            Level::Info,
                            "bluetooth",
                            "scan_start",
                            "Bluetooth scan requested",
                            Some(id),
                            json!({"enable_if_off":true,"seconds":15}),
                        );
                        let result = start_scan(current.powered, |method| {
                            if adapter.is_empty() {
                                return Err("Bluetooth is still starting; try again".into());
                            }
                            if method == "PowerOn" {
                                power_at(&c, &adapter, true, true, preference)
                            } else {
                                start_discovery(&c, &adapter)
                            }
                        });
                        if result.is_ok() {
                            current.powered = true;
                            current.discovering = true;
                            current.scan = RadioScan::Scanning;
                            discovery = Some(Discovery::new(id, Instant::now()));
                            log.emit(
                                Level::Info,
                                "bluetooth",
                                "scan_started",
                                "Bluetooth discovery active",
                                Some(id),
                                json!({}),
                            );
                        }
                        result
                    }
                    Command::Scan(false) => {
                        discovery = None;
                        current.scan = RadioScan::Idle;
                        call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery")
                    }
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
                            let r = user_intent("forget").and_then(|_lease| {
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
                            });
                            r
                        } else {
                            // Pair and subsequent Connect share the Agent's
                            // connection and operation lease. Pairable is
                            // bounded, restored on completion/cancel/error.
                            (|| {
                                if operations
                                    .lock()
                                    .map_err(|_| "Bluetooth operation lock poisoned")?
                                    .len()
                                    >= 16
                                {
                                    return Err("Too many Bluetooth operations".into());
                                }
                                let lease = user_intent(match member {
                                    "Pair" => "pair",
                                    "Disconnect" => "disconnect",
                                    _ => "connect",
                                })?;
                                let owner = current
                                    .bluez_owner
                                    .as_deref()
                                    .ok_or("BlueZ owner unavailable")?;
                                let mut operation = Operation {
                                    id,
                                    started: Instant::now(),
                                    method: member.into(),
                                    path: path.clone(),
                                    owner: owner.into(),
                                    lease,
                                    pairing: if member == "Pair" {
                                        Some(PairableWindow::begin(&c, owner, &adapter)?)
                                    } else {
                                        None
                                    },
                                };
                                match operation.send(&c) {
                                    Ok(serial) => {
                                        operations
                                            .lock()
                                            .map_err(|_| "Bluetooth operation lock poisoned")?
                                            .insert(serial, operation);
                                        Ok(())
                                    }
                                    Err(error) => {
                                        operation.cancel(&c);
                                        Err(error)
                                    }
                                }
                            })()
                        }
                    }
                };
                if let (Err(e), Some((address, preference))) = (&result, codec_args) {
                    if codec_not_ready(e) && codec_attempt < CODEC_RETRIES {
                        codec_retry = Some(CodecRetry {
                            address,
                            preference,
                            attempt: codec_attempt + 1,
                            due: Instant::now() + CODEC_RETRY_DELAY,
                        });
                    }
                }
                let result = match (result, busy_command) {
                    (Err(e), Some(command)) if lock_busy(&e) => {
                        let now = Instant::now();
                        let until = busy_until.unwrap_or(now + BUSY_RETRY_FOR);
                        if now < until {
                            if busy_until.is_none() {
                                log.emit(Level::Info, "bluetooth", "operation_waiting", &e, Some(id), json!({"method":operation,"retry_seconds":BUSY_RETRY_FOR.as_secs()}));
                            }
                            current.waiting = match &command {
                                Command::Connect(path) => Some(path.clone()),
                                _ => None,
                            };
                            busy_retry = Some(BusyRetry {
                                command,
                                due: now + BUSY_RETRY_DELAY,
                                until,
                            });
                            None
                        } else {
                            current.waiting = None;
                            Some(Err(e))
                        }
                    }
                    (result, _) => {
                        if busy_until.is_some() {
                            current.waiting = None;
                        }
                        Some(result)
                    }
                };
                if let Some(Err(e)) = result {
                    current.error = Some(e.clone());
                    operation_error = Some(e.clone());
                    if scan_command {
                        current.scan = RadioScan::Failed { message: e.clone() };
                    }
                    log.add("bluetooth_errors", 1.);
                    log.emit(
                        Level::Warn,
                        "bluetooth",
                        "operation_failed",
                        &e,
                        Some(id),
                        json!({"method":operation,"recovery_attempted":false}),
                    );
                } else if result.is_some() {
                    log.emit(
                        Level::Info,
                        "bluetooth",
                        "operation_requested",
                        "BlueZ operation sent",
                        Some(id),
                        json!({"method":operation}),
                    );
                }
                changed = true;
                refresh = Instant::now() - Duration::from_secs(30);
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
            for p in connected.iter() {
                if !now.contains(p) {
                    log.add("bluetooth_disconnects", 1.);
                    changed = true
                }
            }
            *connected = now;
            log.health_set(
                "bluetooth",
                if current.available {
                    if current.error.is_some() {
                        HealthState::Degraded
                    } else {
                        HealthState::Ok
                    }
                } else {
                    HealthState::Unavailable
                },
                false,
                if current.available {
                    current.error.as_deref().unwrap_or(if current.powered {
                        "BlueZ adapter powered"
                    } else {
                        "Bluetooth off"
                    })
                } else {
                    "BlueZ unavailable"
                },
            );
            if changed || unpublished {
                match et.try_send(current.clone()) {
                    Ok(()) => unpublished = false,
                    Err(TrySendError::Full(_)) => unpublished = true,
                    Err(TrySendError::Disconnected(_)) => {
                        cancel_operations(&c, &operations);
                        return ControlFlow::Break(());
                    }
                }
            }
        }
        ControlFlow::Continue(())
    }
}

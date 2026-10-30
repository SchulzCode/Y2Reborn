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
        let mut st = self.open(c);
        loop {
            match self.pass(&mut st, connected) {
                ControlFlow::Continue(()) | ControlFlow::Break(Pass::Again) => {}
                ControlFlow::Break(Pass::Reconnect) => return ControlFlow::Continue(()),
                ControlFlow::Break(Pass::Exit) => return ControlFlow::Break(()),
            }
        }
    }

    /// Set up a session on a new connection: the reply and agent handlers, the agent itself, the
    /// transport and object watches, and the state the event loop starts from.
    fn open(&self, c: Connection) -> Session {
        let BluetoothWorker { log, .. } = self;
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
        let agent_owner = name_owner(&c, "org.bluez");
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
        let transport_epochs = TransportEpochs::default();
        let refresh = Instant::now() - Duration::from_secs(30);
        let current = Status::default();
        let adapter = String::new();
        let discovery: Option<Discovery> = None;
        let operation_error = None;
        // A codec choice refused only because the peer was not ready yet
        // (connecting, PCM not registered, another operation running) is
        // tried again: it was dropped (reports 61caa81e/297f1032).
        let codec_retry: Option<CodecRetry> = None;
        let busy_retry: Option<BusyRetry> = None;
        // The window of the user's current waiting Connect, kept
        // across retries so a busy answer cannot extend it.
        let busy_window: Option<Instant> = None;
        let unpublished = false;
        let commands_pending = false;
        Session {
            c,
            observation_dirty,
            operations,
            completed_rx,
            pending,
            agent_owner,
            transport_invalidations,
            daemon_invalidated,
            agent_invalidated,
            transport_watches_ready,
            transport_epochs,
            refresh,
            current,
            adapter,
            discovery,
            operation_error,
            codec_retry,
            busy_retry,
            busy_window,
            unpublished,
            commands_pending,
            changed: false,
        }
    }

    /// One pass of the session's event loop.
    fn pass(&self, st: &mut Session, connected: &mut Vec<String>) -> ControlFlow<Pass> {
        self.wait(st)?;
        self.completions(st);
        self.refresh(st);
        self.discovery(st);
        self.pairing(st);
        self.commands(st)?;
        self.publish(st, connected)?;
        ControlFlow::Continue(())
    }

    /// Wait for D-Bus, a command or the next deadline; a closed connection ends the session.
    fn wait(&self, st: &mut Session) -> ControlFlow<Pass> {
        let BluetoothWorker {
            et, wake_receiver, ..
        } = self;
        let mut disconnected = false;
        for index in 0..64 {
            match st.c.process(Duration::ZERO) {
                Ok(false) => break,
                Ok(true) => {
                    if index == 63 {
                        st.observation_dirty.store(true, Ordering::Release);
                    }
                }
                Err(_) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let now = Instant::now();
        let mut until = st.refresh + Duration::from_secs(30);
        if st.commands_pending {
            until = now;
        }
        if st.unpublished {
            until = until.min(now + Duration::from_millis(100));
        }
        if st.observation_dirty.load(Ordering::Acquire) {
            until = now;
        }
        if let Some(scan) = &st.discovery {
            until = until.min(scan.until);
        }
        if let Some(retry) = &st.codec_retry {
            until = until.min(retry.due);
        }
        if let Some(retry) = &st.busy_retry {
            until = until.min(retry.due);
        }
        if let Ok(ops) = st.operations.lock() {
            for operation in ops.values() {
                until = until.min(operation.started + Duration::from_secs(70));
            }
        }
        if let Ok(p) = st.pending.lock() {
            if st.current.pending.as_ref().map(|v| (&v.device, &v.display))
                != p.as_ref().map(|v| (&v.display.device, &v.display.display))
            {
                until = now;
            }
            if let Some(p) = p.as_ref() {
                until = until.min(p.started + Duration::from_secs(60));
            }
        }
        let wait = until.saturating_duration_since(now);
        let watch = st.c.channel().watch();
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
                if !st.c.process(Duration::ZERO).map_err(dbus_error)? {
                    break;
                }
            }
            Ok(())
        });
        if disconnected || result.is_err() {
            cancel_operations(&st.c, &st.operations);
            st.current.available = false;
            st.current.error = Some("Bluetooth service disconnected".into());
            if st.current.scan.active() {
                st.current.scan = RadioScan::Failed {
                    message: "Bluetooth service disconnected".into(),
                };
            }
            let _ = et.try_send(std::mem::take(&mut st.current));
            return ControlFlow::Break(Pass::Reconnect);
        }
        // Drain notifications independently of queue consumption:
        // a command can be consumed during D-Bus activity before
        // its sender writes the wake byte. Leaving that late byte
        // unread would make poll permanently ready.
        while (&*wake_receiver).read(&mut [0u8; 64]).is_ok_and(|n| n > 0) {}
        ControlFlow::Continue(())
    }

    /// Finished and timed-out BlueZ operations.
    fn completions(&self, st: &mut Session) {
        let BluetoothWorker { log, .. } = self;
        st.changed = false;
        if let Ok(mut ops) = st.operations.lock() {
            ops.retain(|_, operation| {
                if operation.started.elapsed() > Duration::from_secs(70) {
                    operation.cancel(&st.c);
                    log.add("bluetooth_errors", 1.);
                    log.emit(
                        Level::Error,
                        "bluetooth",
                        "operation_timeout",
                        "BlueZ operation deadline exceeded",
                        Some(operation.id),
                        json!({"method":operation.method}),
                    );
                    st.operation_error = Some("Bluetooth operation timed out".into());
                    st.current.error = st.operation_error.clone();
                    st.changed = true;
                    false
                } else {
                    true
                }
            });
        }
        while let Ok((error, busy)) = st.completed_rx.try_recv() {
            if let Some(path) = busy {
                let now = Instant::now();
                let until = *st.busy_window.get_or_insert(now + BUSY_RETRY_FOR);
                if now < until && st.busy_retry.is_none() {
                    st.current.waiting = Some(path.clone());
                    st.busy_retry = Some(BusyRetry {
                        command: Command::Connect(path),
                        due: now + BUSY_RETRY_DELAY,
                        until,
                    });
                    st.changed = true;
                    continue;
                }
            }
            if st.busy_retry.is_none() && st.current.waiting.take().is_some() {
                st.busy_window = None;
            }
            st.operation_error = error;
            st.current.error = st.operation_error.clone();
            st.refresh = Instant::now() - Duration::from_secs(30);
            st.changed = true;
        }
    }

    /// Read BlueZ and BlueALSA again when something changed (or every 30 s).
    fn refresh(&self, st: &mut Session) {
        let BluetoothWorker { log, .. } = self;
        if st.observation_dirty.swap(false, Ordering::AcqRel)
            || st.refresh.elapsed() >= Duration::from_secs(30)
        {
            if st.agent_invalidated.swap(false, Ordering::AcqRel) {
                st.agent_owner = None;
            }
            match status(&st.c) {
                Ok((a, mut s)) => {
                    if !st.transport_watches_ready {
                        // A same-path daemon/object restart could otherwise
                        // survive the periodic snapshot. Refuse to expose a
                        // playable transport if its invalidation watches failed.
                        s.pcms.clear();
                    }
                    let invalidated = st
                        .transport_invalidations
                        .lock()
                        .map(|mut objects| std::mem::take(&mut *objects))
                        .unwrap_or_default();
                    st.transport_epochs.refresh(
                        &mut s,
                        &invalidated,
                        st.daemon_invalidated.swap(false, Ordering::AcqRel),
                    );
                    if st.current.bluez_owner != s.bluez_owner {
                        cancel_operations(&st.c, &st.operations);
                    }
                    if st.agent_owner != s.bluez_owner {
                        st.agent_owner = s.bluez_owner.clone();
                        if let Some(owner) = st.agent_owner.as_deref() {
                            let registration: Result<(), dbus::Error> =
                                st.c.with_proxy(owner, "/org/bluez", Duration::from_secs(3))
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
                    st.adapter = a;
                    s.codec_policy =
                        refresh_codec_policy(st.current.codec_policy.as_ref(), &s, || {
                            discover_codec_policy(
                                &st.c,
                                &s,
                                Path::new("/etc/y2linux/bluetooth-codecs.json"),
                                crate::codecs::experimental_enabled(),
                            )
                        });
                    s.scan = st.current.scan.clone();
                    s.error = st.operation_error.clone();
                    st.current = s
                }
                Err(e) => {
                    cancel_operations(&st.c, &st.operations);
                    st.current.available = false;
                    st.current.pcms.clear();
                    st.current.codec_policy = None;
                    st.current.devices.clear();
                    st.current.bluez_owner = None;
                    st.current.bluealsa_owner = None;
                    st.current.bluealsa = false;
                    st.current.error = Some(e)
                }
            }
            st.refresh = Instant::now();
            st.changed = true;
        }
    }

    /// The running scan: stopped with the adapter, ended when its time is up.
    fn discovery(&self, st: &mut Session) {
        let BluetoothWorker { log, .. } = self;
        if st.discovery.is_some() && (!st.current.available || !st.current.powered) {
            let message = "Bluetooth radio became unavailable during scan".to_string();
            if let Some(job) = st.discovery.take() {
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
            st.current.scan = RadioScan::Failed {
                message: message.clone(),
            };
            st.current.error = Some(message.clone());
            st.operation_error = Some(message);
            st.changed = true;
        }
        if st
            .discovery
            .as_ref()
            .is_some_and(|d| d.expired(Instant::now()))
        {
            let job = st.discovery.take().unwrap();
            // Stop only the discovery session held by this D-Bus connection.
            match call(&st.c, &st.adapter, "org.bluez.Adapter1", "StopDiscovery") {
                Ok(()) => {
                    st.current.scan = RadioScan::Complete {
                        found: st.current.devices.len(),
                    };
                    st.current.discovering = false;
                    log.emit(
                        Level::Info,
                        "bluetooth",
                        "scan_complete",
                        "Bluetooth scan completed",
                        Some(job.id),
                        json!({"count":st.current.devices.len()}),
                    );
                }
                Err(e) => {
                    st.current.scan = RadioScan::Failed { message: e.clone() };
                    st.current.error = Some(e.clone());
                    st.operation_error = Some(e.clone());
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
            st.refresh = Instant::now() - Duration::from_secs(30);
            st.changed = true;
        }
    }

    /// A pairing request the agent is waiting on.
    fn pairing(&self, st: &mut Session) {
        if let Ok(mut p) = st.pending.lock() {
            if p.as_ref()
                .is_some_and(|v| v.started.elapsed() > Duration::from_secs(60))
            {
                if let Some(v) = p.take() {
                    let _ = st.c.send(rejected(&v.message));
                }
            }
            let display = p.as_ref().map(|v| v.display.clone());
            st.changed |= st.current.pending.as_ref().map(|v| (&v.device, &v.display))
                != display.as_ref().map(|v| (&v.device, &v.display));
            st.current.pending = display;
        }
    }

    /// The next command, or a codec or busy retry that is due.
    fn commands(&self, st: &mut Session) -> ControlFlow<Pass> {
        let BluetoothWorker { rx, .. } = self;
        let mut codec_attempt = 0u8;
        let mut busy_until = None;
        let command = match rx.try_recv() {
            Ok(c) => {
                if matches!(c, Command::Codec { .. }) {
                    st.codec_retry = None; // a newer choice replaces it
                }
                if !matches!(
                    c,
                    Command::Codec { .. }
                        | Command::Refresh
                        | Command::Scan(_)
                        | Command::Confirm(_)
                ) {
                    st.busy_window = None;
                    if st.busy_retry.take().is_some() {
                        st.current.waiting = None; // so does a newer device or radio choice
                        st.changed = true;
                    }
                }
                Some(c)
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                cancel_operations(&st.c, &st.operations);
                return ControlFlow::Break(Pass::Exit);
            }
            Err(_) => match st.codec_retry.take() {
                Some(retry) if retry.due <= Instant::now() => {
                    codec_attempt = retry.attempt;
                    Some(Command::Codec {
                        address: retry.address,
                        preference: retry.preference,
                    })
                }
                other => {
                    st.codec_retry = other;
                    match st.busy_retry.take() {
                        // The automatic reconnect brought this very device back.
                        Some(BusyRetry {
                            command: Command::Connect(path),
                            ..
                        }) if st
                            .current
                            .devices
                            .iter()
                            .any(|d| d.path == path && d.connected) =>
                        {
                            st.current.waiting = None;
                            st.changed = true;
                            None
                        }
                        Some(retry) if retry.due <= Instant::now() => {
                            busy_until = Some(retry.until);
                            st.busy_window = Some(retry.until);
                            Some(retry.command)
                        }
                        other => {
                            st.busy_retry = other;
                            None
                        }
                    }
                }
            },
        };
        st.commands_pending = command.is_some();
        if let Some(cmd) = command {
            self.command(st, cmd, codec_attempt, busy_until)?;
        }
        ControlFlow::Continue(())
    }

    /// Carry out one command (or a codec or busy retry): its BlueZ operation, the status it leaves and,
    /// for a device that BlueZ refused as busy, the retry.
    fn command(
        &self,
        st: &mut Session,
        cmd: Command,
        codec_attempt: u8,
        busy_until: Option<Instant>,
    ) -> ControlFlow<Pass> {
        let BluetoothWorker {
            log,
            preference,
            et,
            ..
        } = self;
        if matches!(cmd, Command::Stop) {
            cancel_operations(&st.c, &st.operations);
            return ControlFlow::Break(Pass::Exit);
        }
        // The headphones' volume is no operation: no error
        // state, no busy wait; a refused step only logs, the
        // next one tries again.
        if let Command::Volume { device, volume } = &cmd {
            if let (Some(owner), Some(transport)) = (
                st.current.bluez_owner.as_deref(),
                st.current
                    .devices
                    .iter()
                    .find(|d| &d.path == device)
                    .and_then(|d| d.transport.as_deref()),
            ) {
                let set =
                    st.c.with_proxy(owner, transport, Duration::from_secs(1))
                        .set(
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
            return ControlFlow::Break(Pass::Again);
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
        if matches!(cmd, Command::Scan(true)) && st.discovery.is_some() {
            return ControlFlow::Break(Pass::Again);
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
            st.operation_error = None;
            st.current.error = None;
        }
        let result = match cmd {
            Command::Stop | Command::Refresh | Command::Volume { .. } => Ok(()),
            Command::Codec {
                address,
                preference,
            } => match codec_request(&st.c, &st.current, &address, preference) {
                Ok(session) => {
                    let error = session.reason.clone();
                    st.current.codec_policy = Some(session);
                    error.map_or(Ok(()), Err)
                }
                Err(error) => Err(error),
            },
            Command::Power(on) => {
                if st.discovery.is_none() {
                    st.current.scan = RadioScan::Idle;
                }
                if !on {
                    if st.discovery.take().is_some() {
                        let _ = call(&st.c, &st.adapter, "org.bluez.Adapter1", "StopDiscovery");
                    }
                    st.current.scan = RadioScan::Idle;
                }
                power_at(&st.c, &st.adapter, on, true, preference)
            }
            Command::Scan(true) => {
                st.current.scan = RadioScan::Starting;
                let _ = et.try_send(st.current.clone());
                log.emit(
                    Level::Info,
                    "bluetooth",
                    "scan_start",
                    "Bluetooth scan requested",
                    Some(id),
                    json!({"enable_if_off":true,"seconds":15}),
                );
                let result = start_scan(st.current.powered, |method| {
                    if st.adapter.is_empty() {
                        return Err("Bluetooth is still starting; try again".into());
                    }
                    if method == "PowerOn" {
                        power_at(&st.c, &st.adapter, true, true, preference)
                    } else {
                        start_discovery(&st.c, &st.adapter)
                    }
                });
                if result.is_ok() {
                    st.current.powered = true;
                    st.current.discovering = true;
                    st.current.scan = RadioScan::Scanning;
                    st.discovery = Some(Discovery::new(id, Instant::now()));
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
                st.discovery = None;
                st.current.scan = RadioScan::Idle;
                call(&st.c, &st.adapter, "org.bluez.Adapter1", "StopDiscovery")
            }
            Command::Confirm(accept) => {
                if let Ok(mut p) = st.pending.lock() {
                    if let Some(v) = p.take() {
                        let _ = st.c.send(if accept {
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
                if !st.current.devices.iter().any(|d| d.path == path) {
                    Err("unknown discovered Bluetooth device".into())
                } else if member == "RemoveDevice" {
                    let r = user_intent("forget").and_then(|_lease| {
                        let r: Result<(), _> = st
                            .c
                            .with_proxy("org.bluez", st.adapter.as_str(), Duration::from_secs(3))
                            .method_call("org.bluez.Adapter1", member, (DbusPath::from(path),));
                        r.map_err(dbus_error)
                    });
                    r
                } else {
                    // Pair and subsequent Connect share the Agent's
                    // connection and operation lease. Pairable is
                    // bounded, restored on completion/cancel/error.
                    (|| {
                        if st
                            .operations
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
                        let owner = st
                            .current
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
                                Some(PairableWindow::begin(&st.c, owner, &st.adapter)?)
                            } else {
                                None
                            },
                        };
                        match operation.send(&st.c) {
                            Ok(serial) => {
                                st.operations
                                    .lock()
                                    .map_err(|_| "Bluetooth operation lock poisoned")?
                                    .insert(serial, operation);
                                Ok(())
                            }
                            Err(error) => {
                                operation.cancel(&st.c);
                                Err(error)
                            }
                        }
                    })()
                }
            }
        };
        if let (Err(e), Some((address, preference))) = (&result, codec_args) {
            if codec_not_ready(e) && codec_attempt < CODEC_RETRIES {
                st.codec_retry = Some(CodecRetry {
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
                        log.emit(
                            Level::Info,
                            "bluetooth",
                            "operation_waiting",
                            &e,
                            Some(id),
                            json!({"method":operation,"retry_seconds":BUSY_RETRY_FOR.as_secs()}),
                        );
                    }
                    st.current.waiting = match &command {
                        Command::Connect(path) => Some(path.clone()),
                        _ => None,
                    };
                    st.busy_retry = Some(BusyRetry {
                        command,
                        due: now + BUSY_RETRY_DELAY,
                        until,
                    });
                    None
                } else {
                    st.current.waiting = None;
                    Some(Err(e))
                }
            }
            (result, _) => {
                if busy_until.is_some() {
                    st.current.waiting = None;
                }
                Some(result)
            }
        };
        if let Some(Err(e)) = result {
            st.current.error = Some(e.clone());
            st.operation_error = Some(e.clone());
            if scan_command {
                st.current.scan = RadioScan::Failed { message: e.clone() };
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
        st.changed = true;
        st.refresh = Instant::now() - Duration::from_secs(30);
        ControlFlow::Continue(())
    }

    /// Connects and disconnects, health, and the status for Reborn.
    fn publish(&self, st: &mut Session, connected: &mut Vec<String>) -> ControlFlow<Pass> {
        let BluetoothWorker { log, et, .. } = self;
        let now = st
            .current
            .devices
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.path.clone())
            .collect::<Vec<_>>();
        for p in &now {
            if !connected.contains(p) {
                log.add("bluetooth_connects", 1.);
                st.changed = true
            }
        }
        for p in connected.iter() {
            if !now.contains(p) {
                log.add("bluetooth_disconnects", 1.);
                st.changed = true
            }
        }
        *connected = now;
        log.health_set(
            "bluetooth",
            if st.current.available {
                if st.current.error.is_some() {
                    HealthState::Degraded
                } else {
                    HealthState::Ok
                }
            } else {
                HealthState::Unavailable
            },
            false,
            if st.current.available {
                st.current
                    .error
                    .as_deref()
                    .unwrap_or(if st.current.powered {
                        "BlueZ adapter powered"
                    } else {
                        "Bluetooth off"
                    })
            } else {
                "BlueZ unavailable"
            },
        );
        if st.changed || st.unpublished {
            match et.try_send(st.current.clone()) {
                Ok(()) => st.unpublished = false,
                Err(TrySendError::Full(_)) => st.unpublished = true,
                Err(TrySendError::Disconnected(_)) => {
                    cancel_operations(&st.c, &st.operations);
                    return ControlFlow::Break(Pass::Exit);
                }
            }
        }
        ControlFlow::Continue(())
    }
}

/// One D-Bus connection's state, from `open` to the end of its event loop.
pub(super) struct Session {
    c: Connection,
    observation_dirty: Arc<AtomicBool>,
    operations: Operations,
    completed_rx: Receiver<(Option<String>, Option<String>)>,
    pending: Arc<Mutex<Option<Pending>>>,
    agent_owner: Option<String>,
    transport_invalidations: Arc<Mutex<HashSet<String>>>,
    daemon_invalidated: Arc<AtomicBool>,
    agent_invalidated: Arc<AtomicBool>,
    transport_watches_ready: bool,
    transport_epochs: TransportEpochs,
    refresh: Instant,
    current: Status,
    adapter: String,
    discovery: Option<Discovery>,
    operation_error: Option<String>,
    codec_retry: Option<CodecRetry>,
    busy_retry: Option<BusyRetry>,
    busy_window: Option<Instant>,
    unpublished: bool,
    commands_pending: bool,
    changed: bool,
}

/// How a pass of the event loop ends early.
pub(super) enum Pass {
    /// Start the next pass at once.
    Again,
    /// The connection is gone: connect again.
    Reconnect,
    /// The command channel is gone: the thread ends.
    Exit,
}

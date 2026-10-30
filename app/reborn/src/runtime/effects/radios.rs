//! Wi-Fi and Bluetooth scans, devices and forgetting.
use crate::*;

impl Runtime {
    /// `Effect::WifiScan`.
    pub(crate) fn on_wifi_scan(&mut self) -> Result<(), String> {
        if self.ui.wifi.scan.active() {
            return Ok(());
        }
        self.wifi
            .as_ref()
            .ok_or("Wi-Fi unavailable")?
            .commands
            .try_send(wifi::Command::Scan)
            .map_err(|_| "Wi-Fi busy")?;
        self.ui.wifi.scan = reborn_core::RadioScan::Starting;
        self.ui.wifi.problem = None;
        self.wifi_state.scan = reborn_core::RadioScan::Starting;
        Ok(())
    }

    /// `Effect::BluetoothScan`.
    pub(crate) fn on_bluetooth_scan(&mut self) -> Result<(), String> {
        if self.ui.bluetooth.scan.active() {
            return Ok(());
        }
        self.bluetooth
            .as_ref()
            .ok_or("Bluetooth unavailable")?
            .commands
            .try_send(bluetooth::Command::Scan(true))
            .map_err(|_| "Bluetooth busy")?;
        self.ui.bluetooth.scan = reborn_core::RadioScan::Starting;
        self.ui.bluetooth.problem = None;
        self.bt_state.scan = reborn_core::RadioScan::Starting;
        Ok(())
    }

    /// `Effect::BluetoothDevice`.
    pub(crate) fn on_bluetooth_device(
        &mut self,
        path: String,
        operation: String,
    ) -> Result<(), String> {
        if operation == "output" {
            let address = self
                .bt_state
                .devices
                .iter()
                .find(|d| d.path == path)
                .ok_or("device not found")?
                .address
                .clone();
            self.switch(AudioOutput::Bluetooth(address))?;
        } else {
            let cmd = match operation.as_str() {
                "pair" => bluetooth::Command::Pair(path),
                "connect" => bluetooth::Command::Connect(path),
                "disconnect" => bluetooth::Command::Disconnect(path),
                "forget" => bluetooth::Command::Forget(path),
                _ => return Err("unsupported Bluetooth action".into()),
            };
            self.bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(cmd)
                .map_err(|_| "Bluetooth busy")?;
        }
        Ok(())
    }

    /// `Effect::ForgetAllWifi`.
    pub(crate) fn on_forget_all_wifi(&mut self) -> Result<(), String> {
        let worker = self.wifi.as_ref().ok_or("Wi-Fi unavailable")?;
        let ids: Vec<u32> = self
            .wifi_state
            .saved
            .iter()
            .filter_map(|n| n.saved_id)
            .collect();
        let queued = ids
            .iter()
            .filter(|id| {
                worker
                    .commands
                    .try_send(wifi::Command::Forget(**id))
                    .is_ok()
            })
            .count();
        self.ui.flash(if queued == ids.len() {
            "Wi-Fi networks forgotten"
        } else {
            "Some networks are still saved. Try again."
        });
        Ok(())
    }

    /// `Effect::ForgetAllBluetooth`.
    pub(crate) fn on_forget_all_bluetooth(&mut self) -> Result<(), String> {
        let worker = self.bluetooth.as_ref().ok_or("Bluetooth unavailable")?;
        let paths: Vec<String> = self
            .bt_state
            .devices
            .iter()
            .filter(|d| d.paired)
            .map(|d| d.path.clone())
            .collect();
        let queued = paths
            .iter()
            .filter(|path| {
                worker
                    .commands
                    .try_send(bluetooth::Command::Forget((*path).clone()))
                    .is_ok()
            })
            .count();
        self.ui.flash(if queued == paths.len() {
            "Bluetooth pairings removed"
        } else {
            "Some pairings remain. Try again."
        });
        Ok(())
    }
}

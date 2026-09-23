#![forbid(unsafe_code)]
use reborn_control::{Command, PlaybackAction, Radio, RadioAction, Request, Test};
use reborn_observability::Level;
use serde_json::json;
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};
fn arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|s| s == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn number(args: &[String], flag: &str, default: u64) -> Result<u64, String> {
    arg(args, flag)
        .map(|s| {
            s.trim_end_matches('s')
                .parse()
                .map_err(|_| format!("invalid {flag}"))
        })
        .unwrap_or(Ok(default))
}
fn command(a: &[String]) -> Result<Command, String> {
    let first = a.first().map(String::as_str).unwrap_or("status");
    let next = a.get(1).map(String::as_str);
    if matches!(first, "wifi" | "bluetooth") {
        if a.iter().any(|s| s == "--follow") {
            return Err("Radio operations cannot use --follow; poll status instead".into());
        }
        if first == "bluetooth" && next == Some("codec") {
            let preference = match a.get(2).map(String::as_str) {
                Some("Auto") => reborn_core::CodecPreference::Auto,
                Some("SBC") => reborn_core::CodecPreference::Sbc,
                _ => return Err("Use bluetooth codec Auto|SBC ADDRESS".into()),
            };
            return Ok(Command::BluetoothCodec {
                address: a.get(3).ok_or("Bluetooth address required")?.clone(),
                preference,
            });
        }
        let action = match next {
            Some("scan") => RadioAction::Scan,
            Some("on") => RadioAction::On,
            Some("off") => RadioAction::Off,
            _ => return Err("Use wifi|bluetooth scan|on|off".into()),
        };
        return Ok(Command::Radio {
            radio: if first == "wifi" {
                Radio::Wifi
            } else {
                Radio::Bluetooth
            },
            action,
        });
    }
    Ok(match first{
 "status"=>Command::Status,"audio"=>Command::Audio,"health"=>Command::Health,"metrics"=>Command::Metrics,"snapshot"=>Command::Snapshot,"diagnose"=>Command::Diagnose,"scan"=>Command::Scan,
 "logs"=>Command::Logs{last:number(a,"--last",100)? as usize,subsystem:arg(a,"--subsystem").map(str::to_owned),level:arg(a,"--level").map(|s|Level::parse(s).ok_or("invalid log level")).transpose()?,since_ms:arg(a,"--since").map(|s|s.trim_end_matches('s').parse::<u64>().map(|n|n.saturating_mul(1000)).map_err(|_|"invalid --since")).transpose()?},
 "events"=>Command::Events{last:number(a,"--last",100)? as usize},"log-level"=>{if next==Some("reset"){Command::LogLevel{subsystem:Some("reset".into()),level:None}}else{Command::LogLevel{subsystem:next.filter(|s|!s.starts_with("--")).map(str::to_owned),level:a.get(2).filter(|_|next.is_some_and(|s|!s.starts_with("--"))).filter(|s|!s.starts_with("--")).map(|s|Level::parse(s).ok_or("invalid level")).transpose()?}}},
 "input" if next==Some("monitor")=>Command::InputMonitor{seconds:number(a,"--seconds",10)?},
 "test"=>{if next==Some("list"){Command::Tests}else{let name:Test=serde_json::from_value(json!(next.unwrap_or("baseline"))).map_err(|_|"unknown safe test")?;Command::Test{name,seconds:arg(a,"--seconds").map(|s|s.parse().map_err(|_|"invalid seconds")).transpose()?,saved:arg(a,"--saved").map(|s|s.parse().map_err(|_|"invalid saved id")).transpose()?}}},
 "play"=>Command::Playback{action:PlaybackAction::Play(next.ok_or("track id required")?.parse().map_err(|_|"invalid track id")?)},"pause"=>Command::Playback{action:PlaybackAction::Pause},"resume"=>Command::Playback{action:PlaybackAction::Resume},"stop"=>Command::Playback{action:PlaybackAction::Stop},"next"=>Command::Playback{action:PlaybackAction::Next},"previous"=>Command::Playback{action:PlaybackAction::Previous},"seek"=>Command::Playback{action:PlaybackAction::Seek(next.ok_or("milliseconds required")?.parse().map_err(|_|"invalid seek")?)},"volume"=>Command::Playback{action:PlaybackAction::Volume(next.ok_or("0..100 required")?.parse::<u8>().map_err(|_|"invalid volume")?.min(100))},"output"=>Command::Playback{action:if next==Some("wired"){PlaybackAction::Wired}else{PlaybackAction::Bluetooth(next.ok_or("wired or Bluetooth address required")?.into())}},_=>return Err("Commands: status health metrics snapshot logs events log-level diagnose test list|NAME scan incremental input monitor play ID pause resume stop next previous seek MS volume N output wired|ADDRESS".into())})
}
fn run() -> Result<i32, String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|s| s == "--help") {
        io::stdout().write_all(b"rebornctl status|audio|health|metrics|snapshot|logs|events|log-level|diagnose|test|scan|input|play|pause|resume|stop|seek|output [--json] [--socket PATH]\nrebornctl wifi|bluetooth scan|on|off [--json]\nrebornctl bluetooth codec Auto|SBC ADDRESS (playback must be stopped)\nRadio scan enables that radio and reports progress through status.\n").map_err(|e|e.to_string())?;
        return Ok(0);
    }
    let path = PathBuf::from(arg(&args, "--socket").unwrap_or("/run/reborn/control.sock"));
    let cmd = command(&args)?;
    let follow = args.iter().any(|s| s == "--follow");
    let mut last = None;
    let mut last_session = String::new();
    loop {
        let response = reborn_control::call(
            &path,
            &Request {
                version: 1,
                id: 1,
                command: cmd.clone(),
            },
        )?;
        if !response.ok {
            return Err(response.error.unwrap_or("request failed".into()));
        }
        let result = &response.result;
        let mut out = io::stdout().lock();
        if follow {
            if let Some(v) = result.as_array() {
                for e in v {
                    let session = e["reborn_session_id"].as_str().unwrap_or("");
                    if session != last_session {
                        last = None;
                        last_session = session.into();
                    }
                    let n = e["sequence"].as_u64().unwrap_or(0);
                    if last.is_none_or(|p| n > p) {
                        serde_json::to_writer(&mut out, e).map_err(|e| e.to_string())?;
                        out.write_all(b"\n").map_err(|e| e.to_string())?;
                        last = Some(n);
                    }
                }
            }
        } else {
            serde_json::to_writer(&mut out, result).map_err(|e| e.to_string())?;
            out.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())?;
        if !follow {
            return Ok(
                if result["overall"] == "failed" || result["passed"] == false {
                    2
                } else if result["overall"] == "degraded" {
                    1
                } else {
                    0
                },
            );
        }
        drop(out);
        std::thread::sleep(Duration::from_millis(250));
    }
}
fn main() {
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            let _ = serde_json::to_writer(io::stdout(), &json!({"ok":false,"error":e}));
            let _ = io::stdout().write_all(b"\n");
            2
        }
    };
    std::process::exit(code);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn radio_commands_use_worker_protocol() {
        for name in ["wifi", "bluetooth"] {
            for action in ["scan", "on", "off"] {
                let cmd = command(&[name.into(), action.into(), "--json".into()]).unwrap();
                let value = serde_json::to_value(cmd).unwrap();
                assert_eq!(value["op"], "radio");
                assert_eq!(value["radio"], name);
                assert_eq!(value["action"], action);
            }
        }
        assert!(command(&["wifi".into(), "exec".into()]).is_err());
        assert!(command(&["bluetooth".into(), "pair".into()]).is_err());
        assert!(command(&["wifi".into(), "scan".into(), "--follow".into()]).is_err());
    }
}

# Platform v1 application integration

Started 2026-09-23 from Reborn `d9ba0549e6b34eff029bd13f7c33a491fee09e66`;
Y2Linux starts at `5f6b4468fb43605ca1da679420823afa72cea73f`. Existing untracked
review/audit/codec documents in the original checkout are preserved. Work is in
an isolated local branch while the previous committed pair builds. No device
access, flashing, push or identity change. Later correctness-closure fixes are
retained; no older repaired defect is reintroduced as an outstanding issue.

`reborn-bench` measures the actual library schema/open/list/upsert/scan code at
1k, 10k and 20k tracks plus current UI catalog/wheel and queue operations. It
requires an inherited empty scratch directory FD and refuses an existing DB.
The platform creates and pins that directory. No real media/DB/state is used.
SQLite WAL/NORMAL and 64-track batching stay unchanged. First-connection and
warm reopen are labelled without claiming a cold OS cache or cold device.
Synthetic short-WAV scanning is metadata work, not playback throughput.

`cargo test -p reborn-library --locked benchmark::tests -- --nocapture`: passed
the real-schema/count/existing-DB refusal case. `cargo build -p reborn --bin
reborn-bench --release --locked`: passed fresh host release build. ARM, target
performance and physical qualification remain pending. Host workload results
and subsequent integration changes are recorded below as they are completed.

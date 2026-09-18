# REBORN-SPLASH-01

The owner installed the startup update and reports that it works. SSH confirms
the expected source, hardware renderer and explicit splash handoff. See the
[physical inspection](2026-09-18-radio-inspection.md) for evidence and limits.

Built Reborn source: `6209f48402a00a077759df042916f0ae2c89783a`.
Built Y2Linux integration: `1e0888795008cc57de92c47a3a241e45cb91e420`.
Kernel source retained: `7e318afbffe640c6bf9da458f61ac2108f7d5bde`.

Reborn prepares its first real EGL/GLES frame before asking the early KMS splash
to release DRM master. It presents that buffer, acknowledges presentation, and
logs `startup.ready`. The splash retains the previous scanout until presentation;
neither participant switches the console back to text during handoff. The native
boundary is limited to `crates/reborn-graphics/native/splash-handoff.h` and the
existing renderer. The previous no-splash startup path remains supported.

The platform helper, exact package/fallback hashes, validation receipts, failure
behavior and manual installation procedure are documented in
`/home/luca/Dokumente/Code/Y2Linux/docs/build/reborn-splash-01-deployment.md`.
The package is `/home/luca/Dokumente/Code/Y2Linux/out/REBORN-SPLASH-01`.
It is a matched BOOTIMG + Y2ROOT update: do not combine the new early splash with
an older Reborn lacking the handoff. There is no Reborn 02 feature expansion.

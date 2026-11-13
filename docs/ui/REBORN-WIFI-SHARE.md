# Wi-Fi Share: plan

Owner request 2026-11-12 (after Y2Linux #115: an iPhone cannot open the player over a cable): copy music onto
the player over Wi-Fi, from any phone or computer, through a web page that works like a file browser for
Internal Storage and the SD card.

## What the listener gets

1. **Settings → Library → Wi-Fi Share.** The page shows an address (`http://192.168.x.y`), a QR code of it and
   a six-digit code. Sharing is on only while this page is open.
2. On the phone or computer (same Wi-Fi): scan the QR code or type the address, enter the code once.
3. A file browser: Internal Storage and SD Card, folders, sizes, free space. **Upload** files or whole
   folders (also by dropping them on the page), **download**, **new folder**, **rename**, **delete**.
4. Leaving the page on the player (Back, Home, or **Stop Sharing**) ends sharing. If anything changed,
   Reborn scans for music by itself, as it does after a USB copy.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Where the server runs | Inside Reborn (`reborn_platform::share`, its own threads) | Reborn owns the page, the code and the scan afterwards; no new service, no platform contract change |
| Protocol | Plain HTTP on port 80, our own small server on `std::net` | No HTTP crate is vendored; FFmpeg and curl stay without server code. A home network has no certificate for HTTPS |
| Who may connect | Anyone on the same network who knows the six-digit code shown on the player's screen; five wrong codes lock for 30 s; a new code every time sharing starts | The traffic is not encrypted, so sharing is off unless the listener opens the page, and never runs unattended |
| What is shared | The two music roots only (`/media/internal/music`, `/media/sd`); paths are resolved inside them, links are not followed, names starting with a dot are hidden | The same view a computer gets over USB |
| SD card writes | Inside the platform's write window (`media write --holder reborn`), renewed while an upload runs, released after | The card stays read-only between writes (Beta 8.27) |
| Uploads | One request per file (`PUT`), written to `.name.part` and renamed when complete; never over an existing file unless the page asked to replace it | A broken transfer leaves no half song in the library |
| Sleep | The player stays awake while the page is open | Wi-Fi is off in sleep |
| Settings place | Library ("Where is my music…"), a sub-page of `Screen::Library` with the filter `share` | Settings map rule: no new `Screen` value |
| Without Wi-Fi | The page says so and leads to Wi-Fi | — |

## Parts

- `crates/reborn-platform/src/share/`: `http.rs` (request line, headers, bodies with a length), `files.rs`
  (safe paths, list, make, rename, delete), `mod.rs` (the server: accept loop, sessions, routes, write windows,
  a change counter), `page.html` (the browser page: one file, no outside resources).
- `app/reborn/src/share.rs`: starts and stops the server with the page (checked on the runtime's tick, state
  beside the runtime), keeps the player awake, scans after changes.
- `crates/reborn-ui`: the Library row, the page (address, QR, code, activity), reusing the QR drawing of the
  Last.fm sign-in.
- Y2Linux: `docs/WIFI-SHARE.md` for owners, the settings map, an audit note (a network service that listens
  only while its page is open).

## Routes

| Route | What |
|---|---|
| `GET /` | the page |
| `POST /api/enter?code=` | sets the session cookie |
| `GET /api/list?root=&path=` | entries (name, folder, size, time), free and total space; without `root`: the storages |
| `GET /api/file?root=&path=` | download |
| `PUT /api/file?root=&path=[&replace=1]` | upload (body = the file) |
| `POST /api/folder?root=&path=` | new folder |
| `POST /api/rename?root=&path=&to=` | rename within its folder |
| `POST /api/delete?root=&path=` | delete a file, or a folder with what is in it |

## Tests

Host: path resolution (no escape through `..`, links or absolute paths), request parsing, the code and its
lockout, and the routes over a loopback connection against a temporary folder (list, upload with `.part`,
replace refused, rename, delete, download). Player: the page from a computer and a phone on the owner's
Wi-Fi; an upload to Internal Storage and to the SD card; the scan afterwards; sharing ends with the page.

## Not in the first version

HTTPS; resuming a broken upload; playing music in the browser; moving between folders or storages; several
people's accounts. A listener who wants none of this simply never opens the page.

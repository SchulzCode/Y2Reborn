# Reborn skins

A skin gives Reborn a different look: its own colours, fonts, pictures and
screen layouts, for example the shipped **Classic** skin with light menus and
blue highlights. A skin only draws. What the buttons do, the menus, playback,
settings and the built-in parts (Cover Flow, the equalizer, text entry, the
update screens) stay Reborn's. Reborn's own look is not a skin and is always
available as **Reborn** in the picker.

## Installing and choosing

1. Copy the skin's **folder** (the one holding `skin.json`) into a folder
   named `Skins` on **Internal Storage** or on the **SD card**, for example
   `Internal Storage/Skins/Neon/skin.json`. Any letter case works for
   `Skins`; a `Themes` folder (its name before Beta 7.5) is read too.
2. On the player: **Settings → Skin**. The list is read again every
   time it opens. Select a skin; it applies at once and is remembered.

The folder name is the skin's id. A skin in a `Skins` folder with the same
folder name as a shipped skin (in `/usr/share/reborn/skins`) is hidden by the
shipped one. A skin that cannot be used appears greyed out with the reason;
choosing a broken skin keeps the current look and shows why in a message.
If a chosen skin is missing at start (for example the SD card was removed),
Reborn uses its own look for that session and keeps the choice.

## The package

```
Neon/
  skin.json        the description (at most 256 KB)
  images/…         PNG, JPEG or WebP pictures, each at most 480 × 360
  fonts/…          TrueType/OpenType fonts (at most 8 MB each)
  LICENSE…         licences of fonts and pictures you ship
```

Paths in `skin.json` are relative to the folder, use `/`, and must stay
inside it (no `..`, no absolute paths, no links that point outside).

## `skin.json`

```json
{
  "format": 1,
  "name": "Neon",
  "author": "You",
  "version": "1.0",
  "description": "One line for the picker.",
  "palette": { "bg": "#101018", "text": "#F0F0FF", "accent": "cover" },
  "colors":  { "pink": "#FF3EA5" },
  "fonts":   { "menu": { "file": "fonts/MyFont-Bold.ttf", "size": 18 } },
  "images":  { "bar": "images/bar.png" },
  "parts":   { "titlebar": [ … elements … ] },
  "screens": { "list": [ … elements … ], "now_playing": [ … ] }
}
```

Only `format` (always `1`) and `name` are required. Unknown fields are errors,
so a typo is reported instead of silently ignored.

### Palette

The palette has fixed names. Your layouts use them, and every screen you do not
describe is drawn by Reborn in these colours:

| Token | Used for | Reborn's own |
| --- | --- | --- |
| `bg` | the background (always opaque) | `#06070A` |
| `text` | main text | `#F6F6F8` |
| `muted` | secondary text | `#A0A4AE` |
| `dim` | hints, disabled rows | `#848894` |
| `accent` | highlights; `"cover"` follows the playing cover | `cover` |
| `glass` | translucent raised areas (keeps Reborn's opacities) | `#FFFFFF` |
| `scrim` | dimming behind sheets | `#000000` |
| `sheet` | sheet and dialog surfaces | `#0E1014` |
| `track` | empty part of rings and bars | `#1E2023` |
| `danger`, `success`, `warning` | states | `#FF7864`, `#3CC878`, `#FFBE3C` |

`text` on `bg` must reach a contrast of 3:1 or the skin is refused; below 4.5:1,
and for `muted`/`dim` below 3:1, Reborn loads it with a warning.

### Colours

Anywhere a colour is asked for you can write `#RRGGBB`, `#RRGGBBAA`, a palette
token, a name from `colors`, or `accent`. Add `/NN` for an opacity in percent:
`"accent/40"`, `"text/60"`.

### Fonts

`fonts` names a font file and a pixel size (8 to 72). Each entry is drawn once
when the skin loads, for Latin text (Latin‑1, Latin Extended‑A and common
punctuation). Large display sizes may not fit the texture with all of Latin;
add `"charset": "ascii"` to carry only plain ASCII (other letters then come
from Reborn's font). Characters the font lacks fall back to Reborn's own font. Text
elements may also use Reborn's fonts by name: `R11`, `R13`, `R15`, `R17`
(regular), `L23`, `L44` (light), `S10`, `S13`, `S15`, `S17`, `S20`, `S25`,
`S28`, `S36` (semibold). Ship only fonts whose licence allows it (for example
the SIL Open Font License).

### Pictures

`images` names picture files (PNG, JPEG or WebP, at most 480 × 360 each, at
most 64). They are drawn at any size; give `slice: [left, top, right, bottom]`
to nine-slice them so corners and edges keep their size while the middle
stretches. All pictures and font glyphs share one 1024 × 1024 texture; a skin
that does not fit is refused.

## Screens

`screens` maps a screen name to a list of elements, drawn in order (later on
top). The screen is 480 × 360 pixels, origin top-left. Reborn looks for the
most specific name first:

| Name | Shown for |
| --- | --- |
| `home` | the main menu (else `list`) |
| `now_playing` | Now Playing |
| `list` | every list: library lists, settings pages, pickers, diagnostics |
| `page` | pages with a heading or facts (About, Storage, Song Info, …) |
| `artists`, `albums`, `genres`, `songs`, `folders`, `album`, `artist`, `queue` | one library screen (else `list`) |
| `cover_flow` | Albums in Cover Flow (only this name; else Reborn's) |
| `music`, `song_info`, `settings`, `wifi`, `bluetooth`, `computer`, `sound`, `playback`, `crossfade`, `screen_off`, `theme`, `library`, `display`, `service`, `battery`, `sleep`, `storage`, `updates`, `about`, `maintenance`, `diagnostics`, `diagnostic`, `value` | one settings or system screen (else `page` or `list`); `theme` is the Skin picker (Settings → Skin since Beta 7.5; the key keeps its name for existing skins) |
| `time_zone` | the time zone picker (else `page` or `list`) |
| `date_time` | Date & Time, the date and time set by hand (else `page` or `list`) |
| `search`, `playlists`, `playlist`, `radio`, `radio_stations`, `radio_categories` | search results, Playlists, one playlist, Radio, a station list, countries or genres (else `list`) |
| `radio_info` | Station Info: the station as heading, its facts and two actions (else `page`) |
| `equalizer` | the equalizer (only this name; else Reborn's) |
| `sheet` | option sheets and pickers that slide over a screen |
| `dialog` | questions such as “Reset settings?” (else `sheet`) |
| `toast` | short messages |
| `volume` | the volume indicator (drawn fading out) |
| `keyboard` | the full-screen keyboard (search, Wi-Fi password, playlist names): draw your title bar and `{sheet_title}` over a `native` `screen` part, which is Reborn's field and keys in your palette without its own header (else all Reborn's) |

Pairing, installing an update, the Power hold and
the start/shut-down screens are always Reborn's, in the skin's palette.

## Elements

Every element has `"type"` and may have `"if"` (see Conditions). Positions
`x`, `y` default to 0. Inside a list row and inside `group`/`part` they are
relative to the row or group.

| Type | Fields |
| --- | --- |
| `rect` | `x y w h color` · `radius` (rounded) · `stroke` (1–3: outline only) |
| `gradient` | `x y w h top bottom` (vertical) |
| `image` | `x y w h image` · `slice` · `tint` (colour multiplied in) |
| `text` | `x y w h text` · `font` (default `R15`) · `color` (default `text`) · `align` `left`/`center`/`right` · `lines` 1–6 · `line_height` · `tracking` · `case` `upper`/`lower` |
| `icon` | `x y size icon` · `color` |
| `bar` | `x y w h value` · `track` `fill` colours or `track_image` `fill_image` (+ `slice`) · `radius` · `vertical` · `knob` (colour) `knob_radius` |
| `art` | `x y size` · `source` `current` (playing cover; a station's logo or initials) or `row` (inside rows: the song's cover, a station's logo) · `radius` (corners take `bg`) |
| `list` | `x y w rows row_height row` · `focused` · `source` `rows`/`facts`/`sheet` · `scrollbar` `{x, w, track, thumb}` |
| `group` | `elements` · `x y` offset |
| `part` | `name` (from `parts`) · `x y` offset |
| `native` | `part` `screen` (Reborn's drawing of this screen) or `status` (Reborn's battery and radio icons) · `x y` · `recolor` (default true) |

Text is cut with “…” to `w`. One line is centred vertically in `h`; several
lines are centred as a block in `h`, or start at `y` when `h` is 0.

A `list` draws `rows` rows of `row_height` from the list it shows: `rows` is
the screen's own list (songs, settings…), `facts` the label/value pairs of a
page, `sheet` the choices of a sheet or dialog. `row` is drawn for each row,
`focused` (default: `row`) for the selected one. The list keeps the selection
visible and scrolls by your row count.

### Text values

Write `{name}` inside `text`:

| Value | |
| --- | --- |
| `title` `artist` `album` `album_artist` `genre` `track_number` | the playing song; for a station the song it announces, its artist and the station with its country, as Reborn's own Now Playing |
| `station` | the station playing (empty for a song) |
| `elapsed` `remaining` `duration` | `1:23`, `-2:05`, `3:28` |
| `queue_index` `queue_length` | for “3 of 12” |
| `quality` | `LOSSLESS · 96 kHz`, `MP3 · 44.1 kHz` |
| `output` | `Headphone Jack` or the Bluetooth device |
| `playback` | `Playing`, `Paused`, `Stopped` |
| `sleep` | a running sleep timer as Now Playing says it: `Sleeps in 23 min`, `Sleeps after song`; empty without one (Beta 8.37) |
| `screen` | the screen's title (empty on Home) |
| `battery` `volume` | numbers 0–100 |
| `clock` | the time in the chosen time zone (`21:41`, or `9:41` where a 12-hour clock is usual); empty until the player's time is set |
| `status` | a page's status line (Wi-Fi, Bluetooth, pickers) |
| `page_title` `page_body` | a page's heading and explanation |
| `empty_title` `empty_body` `empty_action` | what an empty list says |
| `message` | a toast's text |
| `sheet_title` `sheet_body` | a sheet's or dialog's heading and text |
| `skin` | this skin's name |
| `label` `detail` `number` `count` | inside list rows: the row's text, its value, its position, the list length |

### Conditions

`"if"` takes names joined with `&` (and), `|` (or, lower precedence) and `!`
(not), e.g. `"playing & !shuffle | paused"`.

Anywhere: `playing`, `paused`, `has_track`, `has_art`, `shuffle` (Songs or
Albums), `shuffle_albums` (Shuffle Albums only), `repeat`,
`repeat_one`, `charging`, `battery_low`, `bluetooth`, `bluetooth_connected`,
`wifi`, `wifi_connected`, `back` (not Home), `empty`, `error` (toast),
`danger` (dialog), `eq`, `gapless`, `crossfade`, `normalise`, `sleep` (a sleep
timer runs), `has_status`,
`has_clock` (the time is known; give the title less room), `live` (a
station plays: `remaining` and `duration` read LIVE, the `progress` bar is full;
hide the queue position), `spotify` (Spotify plays: show where the sound comes
from; `quality` already reads `SPOTIFY · 320 KBPS`), `favorite` (the song playing is in Favorites),
`has_body`, `has_title`, `has_action`, `has_facts`, `interactive`.

Inside list rows: `focused`, `disabled`, `toggle`, `on`, `check`, `checked`,
`chevron`, `has_detail`, `row_art`, `current` (the playing song), `first`,
`last`, `odd`.

### Icons

`back chevron check close play pause prev next play_large pause_large
prev_large next_large shuffle repeat repeat_one gapless eq normalise bluetooth
wifi lock bolt folder note speaker headphones sd usb warning info sun drive
power download refresh search vinyl person crossfade heart palette moon` (`palette` since
Beta 7.5, `moon` since Beta 8.37), and three that follow
the state: `play_pause`, `play_pause_large`, `repeat_mode`.

## Limits and checks

`skin.json` 256 KB; 64 pictures of at most 480 × 360; 8 fonts of at most 8 MB;
4000 elements; one 1024 × 1024 texture for pictures and glyphs. Loading names
the first problem with its place, e.g.
`screens.list[2] (text): unknown value "{lyrics}"`.

## Trying a skin on a computer

From a Y2Reborn checkout (needs FFmpeg development files):

```
cargo run --bin reborn-preview -- out/preview --skin path/to/Neon
python3 tools/preview/render_previews.py out/preview
```

`out/preview/png/` then holds every screen in your skin and a contact sheet.
On the player, `rebornctl theme` lists the installed skins with their problems
and `rebornctl theme Neon` (or `theme reborn`) switches.

Reborn ships 16 skins in `skins/` (installed to `/usr/share/reborn/skins`):
Classic (pictures from `tools/assets/build_classic_skin.py`) and, generated by
`tools/assets/build_skins.py`, Nano Glass, Walkman, Terminal, Winamp, Paper,
Big Type, Cassette, Neon Night, Zune, Bunny Bow (pixel art and the Pixelify
Sans pixel font) and Mini Silver, Pink, Blue, Green and Gold. Each is a
complete example to copy and change. `charset` and
`case: lower` (used by Zune) need Reborn after candidate 33.

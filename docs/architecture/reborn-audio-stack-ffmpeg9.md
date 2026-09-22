# Reborn FFmpeg 9 audio stack

Reborn has one media path. `reborn-media` opens the local file with FFmpeg
9.0.2 `libavformat` and `libavcodec`, converts decoder output once to stereo
`AV_SAMPLE_FMT_FLTP`, runs an optional `libavfilter` graph, and uses
`libswresample` for the final sink rate and sample format. Rust owns only the
opaque context, cancellation, bounded scheduling and diagnostics; it has no
decoder, software resampler, integer volume stage or competing DSP path.

The cold startup path does not link the FFmpeg membrane into the `reborn`
executable. Buildroot installs the one shared membrane at
`/usr/lib/reborn/libreborn_media.so`; `reborn-media` resolves its `rb_*` API on
the first media operation and keeps that handle resident for the process
lifetime. This preserves one FFmpeg implementation while keeping the six large
FFmpeg libraries out of the dynamic loader's pre-`main` work. The startup log
records the pinned version immediately and obtains the actual component
manifest when the membrane is opened.

The normal graph is:

```text
demux -> decode -> stereo FLTP -> aformat -> ReplayGain/user volume
      -> optional equalizer -> alimiter -> final libswresample -> ALSA
```

ReplayGain reads track and album gain/peak tags from the container metadata.
`Off`, `Track` and `Album` are distinct settings. Peak-aware headroom is applied
before the FFmpeg volume filter and the limiter protects the integer sink. The
source file is never changed. EQ bands are passed into `equalizer` and a graph
is rebuilt when the playback settings change.

Gapless playback is the default queue behavior. The next decoder is opened
before the current decoder reaches EOF, and the bounded PCM channel carries a
boundary marker between the last valid sample of one track and the first valid
sample of the next. Codec skip-sample side data is honored when FFmpeg exposes
it; there is no fixed silence trim. Crossfade is independent. When enabled,
the worker retains only the configured transition window, feeds both windows
through FFmpeg `acrossfade` in canonical FLTP, then sends the single overlap
window and continues with the unconsumed head of the next track.

The preferred wired sink is S32_LE at the source-native qualified rate. The
current real-device qualification only proves stereo S16_LE at 44.1 kHz, so
the Y2Linux qualification profile selects that mode explicitly until the owner
qualifies S32_LE and additional rates. Reborn logs the requested rate, the
selected ALSA format, and every fallback reason. Bluetooth receives the same
processed PCM stream and differs only at the final BlueALSA sink boundary.

The production source matrix covers FLAC, MP3, AAC/ADTS, AAC/M4A, ALAC, Ogg
Vorbis, Opus, WAV, AIFF, APE and WavPack, including PCM 16/24/32-bit integer
and 32-bit float inputs. JPEG, PNG and WebP attached artwork remains in the
same media membrane; `libswscale` is used only for image conversion.

`rebornctl audio --json` exposes the active source metadata, decoder and
internal format, filter description, ReplayGain values, resampling decision,
final format conversion, ALSA parameters, transition timing and fixed-cardinality
audio metrics. A rate or channel change is reported as resampling; the
intentional FLTP-to-packed-sink conversion is reported separately. Embedded and
bounded local sidecar cover images use the same FFmpeg image decoders and
libswscale path. `status --json` and `audio --json` report the runtime component
manifest once the membrane has been loaded; they do not trigger a cold FFmpeg
load on the UI thread. The decoder diagnostic loads it on its worker when an
early manifest is required. Build verification rejects missing required
components, any encoder or muxer, network protocols, the FFmpeg command-line
tools, `libavdevice`, and unrelated codec families.

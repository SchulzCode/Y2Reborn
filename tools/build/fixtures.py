#!/usr/bin/env python3
"""Generate deterministic local Reborn media fixtures with a host FFmpeg.

The APE and WavPack specimens are checked-in, silent, public test inputs. The
host FFmpeg build cannot encode either format, so this generator validates and
copies those two small decoder fixtures instead of silently substituting a
different codec.
"""
from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import tempfile
import wave
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "assets" / "fixtures"
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")
TARGET_FFMPEG_VERSION = (ROOT / "FFMPEG_VERSION").read_text().strip()


def ffmpeg_version() -> str:
    output = subprocess.check_output([FFMPEG, "-version"], text=True, stderr=subprocess.STDOUT)
    match = re.search(r"\bversion\s+n?([0-9]+(?:\.[0-9]+){1,2})", output.splitlines()[0])
    if not match:
        raise RuntimeError(f"cannot read FFmpeg generator version from {FFMPEG!r}")
    return match.group(1)


def run(*args: str) -> None:
    subprocess.run(
        [FFMPEG, "-hide_banner", "-loglevel", "error", "-y", *args],
        check=True,
    )


def sine(path: Path, rate: int, codec: str, *, sample_fmt: str | None = None,
         fmt: str | None = None, metadata: dict[str, str] | None = None,
         duration: float = 0.25) -> None:
    args = [
        "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate={rate}:duration={duration}",
        "-af", "volume=0.04", "-ac", "2", "-ar", str(rate), "-c:a", codec,
    ]
    if sample_fmt:
        args += ["-sample_fmt", sample_fmt]
    if metadata:
        for key, value in metadata.items():
            args += ["-metadata", f"{key}={value}"]
    if fmt:
        args += ["-f", fmt]
    run(*args, str(path))


def png(path: Path) -> None:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack("!I", len(data))
            + kind
            + data
            + struct.pack("!I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    raw = b"".join(b"\0" + bytes([40, 160, 90]) * 8 for _ in range(8))
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack("!IIBBBBB", 8, 8, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def attach_art(path: Path, base: Path, image: Path, mime: str) -> None:
    run(
        "-i", str(base), "-i", str(image), "-map", "0:a", "-map", "1:v",
        "-c:a", "copy", "-c:v", "copy", "-metadata:s:v", f"mimetype={mime}",
        "-disposition:v", "attached_pic", str(path)
    )


def write_gapless_wav(path: Path, rate: int, first: int, count: int, signed_seed: int) -> None:
    with wave.open(str(path), "wb") as f:
        f.setnchannels(2)
        f.setsampwidth(2)
        f.setframerate(rate)
        data = bytearray()
        for i in range(count):
            value = signed_seed + first + i
            value = max(-30000, min(30000, value))
            data += struct.pack("<hh", value, value)
        f.writeframes(data)


def make_gapless(temp: Path) -> dict[str, int]:
    rate = 44100
    count = 4096
    write_gapless_wav(temp / "gapless-a.wav", rate, 0, count, -12000)
    write_gapless_wav(temp / "gapless-b.wav", rate, count, count, -12000)
    run("-i", str(temp / "gapless-a.wav"), "-c:a", "flac", str(OUT / "gapless-a.flac"))
    run("-i", str(temp / "gapless-b.wav"), "-c:a", "flac", str(OUT / "gapless-b.flac"))
    return {"rate": rate, "frames_per_track": count, "boundary_sample": -12000 + count - 1}


def make_precision_flac(temp: Path) -> None:
    # Same exact reference sequence as the native membrane low-bit test.
    pattern = (0, 1, -1, 127, -127, 255, -255, 256, -256, 257, -257,
               1023, -1023, 32767, -32768, 0, 0x234500, 0x234501,
               0x23457f, 0x234580, 0x2345ff, -0x234600, -0x2345ff,
               -0x234581, -0x234580, -0x234501)
    data = b"".join((value & 0xffffff).to_bytes(3, "little")
                    for frame in range(1024)
                    for value in (pattern[frame % len(pattern)],
                                  pattern[(frame + 5) % len(pattern)]))
    for rate in (44100, 48000):
        source = temp / f"precision-24-{rate}.wav"
        with wave.open(str(source), "wb") as stream:
            stream.setparams((2, 3, rate, 1024, "NONE", "not compressed"))
            stream.writeframes(data)
        run("-i", str(source), "-c:a", "flac", "-sample_fmt", "s32",
            "-map_metadata", "-1", "-fflags", "+bitexact",
            str(OUT / f"precision-24-{rate}.flac"))


def make_loud_flac(path: Path) -> None:
    # Near full scale at 40 Hz and 8.8 kHz (where common headphone profiles
    # boost), 24/96, ReplayGain +6 dB without a peak tag: the graph's net
    # gain is positive, so only the limiter keeps it below full scale.
    run("-f", "lavfi", "-i",
        "aevalsrc=0.6*sin(2*PI*40*t)+0.39*sin(2*PI*8800*t):s=96000:d=1",
        "-ac", "2", "-c:a", "flac", "-sample_fmt", "s32",
        "-metadata", "REPLAYGAIN_TRACK_GAIN=+6.00 dB",
        "-fflags", "+bitexact", str(path))


BOOK_CHAPTERS = [(0, 2000, "Opening"), (2000, 4000, "Chapter 1: The Start"),
                 (4000, 6000, "Kapitel 2: Über Umwege")]


def make_books(temp: Path) -> list[str]:
    """Audiobook specimens (2026-10-21): 6 s with three chapters as M4B (MP4
    chapter track) and MP3 (ID3 CHAP), and an M4B carrying its cover as an
    MPEG-4 video track, which Reborn must play without reading the video."""
    meta = temp / "chapters.txt"
    lines = [";FFMETADATA1", "title=Fixture Book", "artist=Y2 fixture",
             "album=Fixture Book", "genre=Audiobook"]
    for start, end, title in BOOK_CHAPTERS:
        lines += ["[CHAPTER]", "TIMEBASE=1/1000", f"START={start}", f"END={end}", f"title={title}"]
    meta.write_text("\n".join(lines) + "\n", encoding="utf-8")
    source = ["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=22050:duration=6",
              "-i", str(meta), "-map_metadata", "1", "-map_chapters", "1",
              "-af", "volume=0.04", "-ac", "1"]
    run(*source, "-c:a", "aac", "-b:a", "24k", "-fflags", "+bitexact", str(OUT / "book-chapters.m4b"))
    run(*source, "-c:a", "libmp3lame", "-b:a", "24k", "-id3v2_version", "3",
        "-fflags", "+bitexact", str(OUT / "book-chapters.mp3"))
    cover = temp / "cover.png"
    png(cover)
    run("-i", str(OUT / "book-chapters.m4b"), "-loop", "1", "-framerate", "1", "-t", "6",
        "-i", str(cover), "-map", "0:a", "-map", "1:v", "-c:a", "copy", "-c:v", "mpeg4",
        "-pix_fmt", "yuv420p", "-map_metadata", "0", "-map_chapters", "0",
        "-fflags", "+bitexact", "-f", "mp4", str(OUT / "book-cover-track.m4b"))
    return ["book-chapters.m4b", "book-chapters.mp3", "book-cover-track.m4b"]


COVER_FILES = {
    "cover-3s.mp3": ["-c:a", "libmp3lame", "-b:a", "128k", "-id3v2_version", "3"],
    "cover-3s.m4a": ["-c:a", "aac", "-b:a", "128k"],
    "cover-3s.flac": ["-c:a", "flac"],
    "cover-3s.ogg": ["-c:a", "libvorbis"],
    "cover-3s.opus": ["-c:a", "libopus", "-b:a", "96k"],
}


def make_covers(temp: Path) -> list[str]:
    """Songs of 3 s with an embedded JPEG cover, one per codec (2026-11-08): MP3 decodes in
    1152-frame blocks, AAC in 1024, Opus in 960, Vorbis in up to 2048 and FLAC in 4096, and
    Now Playing must get every song's cover early in the song whatever the block size
    (Y2Reborn #102, Y2Linux #98: 9.3.16 published MP3 and AAC covers only at the song's end)."""
    cover = temp / "cover.png"
    png(cover)
    jpeg = temp / "cover.jpg"
    run("-i", str(cover), "-frames:v", "1", "-c:v", "mjpeg", str(jpeg))
    # Ogg carries a cover as a METADATA_BLOCK_PICTURE comment (a FLAC picture block), not as a
    # picture stream; FFmpeg reads either as the attached picture.
    data = jpeg.read_bytes()
    mime = b"image/jpeg"
    block = (struct.pack(">II", 3, len(mime)) + mime + struct.pack(">IIIIII", 0, 8, 8, 24, 0, len(data))
             + data)
    comment = "METADATA_BLOCK_PICTURE=" + base64.b64encode(block).decode()
    names = []
    for name, codec in COVER_FILES.items():
        source = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=3"]
        if name.endswith((".ogg", ".opus")):
            picture = ["-map", "0:a", "-metadata", comment]
        else:
            picture = ["-i", str(jpeg), "-map", "0:a", "-map", "1:v", "-c:v", "copy",
                       "-disposition:v", "attached_pic", "-metadata:s:v", "mimetype=image/jpeg"]
        run(*source, *picture, "-af", "volume=0.04", "-ac", "2", *codec,
            "-metadata", "title=Cover fixture", "-fflags", "+bitexact", str(OUT / name))
        names.append(name)
    return names


def update_covers() -> None:
    """Write only the cover specimens and record them in the manifest."""
    with tempfile.TemporaryDirectory(prefix="reborn-covers-") as directory:
        names = make_covers(Path(directory))
    path = OUT / "manifest.json"
    manifest = json.loads(path.read_text())
    for name in names:
        data = (OUT / name).read_bytes()
        manifest["files"][name] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    manifest["cover_cases"] = {"generator_ffmpeg_version": ffmpeg_version(), "files": names}
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


FILM_FILES = {
    # A real moving picture the player cannot show (HEVC): listed in Videos as unplayable, never
    # as a song (Reddit 2026-11-22: HEVC anime episodes showed up as music).
    "film-hevc.mp4": ["-c:v", "libx265", "-x265-params", "log-level=error", "-tag:v", "hvc1"],
    "film-h264.mp4": ["-c:v", "libx264", "-profile:v", "baseline"],
    # A film remuxed from a TV recording starts its clock at 1.4 s, not 0: the sound and the picture
    # must still agree.
    "film-h264-offset.mkv": ["-c:v", "libx264", "-profile:v", "baseline", "-output_ts_offset", "1.4",
                             "-f", "matroska"],
}


def make_films() -> list[str]:
    names = []
    for name, codec in FILM_FILES.items():
        run("-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1",
            *codec, "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "48k", "-ac", "2", "-shortest",
            "-metadata", "title=Film fixture", "-fflags", "+bitexact", str(OUT / name))
        names.append(name)
    return names


def update_films() -> None:
    """Write only the film specimens and record them in the manifest."""
    names = make_films()
    path = OUT / "manifest.json"
    manifest = json.loads(path.read_text())
    for name in names:
        data = (OUT / name).read_bytes()
        manifest["files"][name] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    manifest["film_cases"] = {"generator_ffmpeg_version": ffmpeg_version(), "files": names}
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def make_series() -> list[str]:
    """Series-tagged books (Y2Linux #144, 2026-11-26): an MP3 with ID3 TXXX
    SERIES and SERIES-PART, and an M4B whose series is its grouping (©grp; the
    MP4 muxer keeps no free-form SERIES)."""
    source = ["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=22050:duration=1",
              "-af", "volume=0.04", "-ac", "1", "-metadata", "title=Series Book",
              "-metadata", "album=Series Book", "-metadata", "genre=Audiobook",
              "-metadata", "grouping=Fixture Group", "-fflags", "+bitexact"]
    run(*source, "-metadata", "SERIES=Fixture Series", "-metadata", "SERIES-PART=2.5",
        "-c:a", "libmp3lame", "-b:a", "24k", "-id3v2_version", "3", str(OUT / "book-series.mp3"))
    run(*source, "-c:a", "aac", "-b:a", "24k", str(OUT / "book-series.m4b"))
    return ["book-series.mp3", "book-series.m4b"]


def update_series() -> None:
    """Write only the series specimens and record them in the manifest."""
    names = make_series()
    path = OUT / "manifest.json"
    manifest = json.loads(path.read_text())
    for name in names:
        data = (OUT / name).read_bytes()
        manifest["files"][name] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    manifest["series_cases"] = {"generator_ffmpeg_version": ffmpeg_version(), "files": names}
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def update_books() -> None:
    """Write only the audiobook specimens and record them in the manifest,
    leaving every other fixture (and its hash) as it is."""
    with tempfile.TemporaryDirectory(prefix="reborn-books-") as directory:
        names = make_books(Path(directory))
    path = OUT / "manifest.json"
    manifest = json.loads(path.read_text())
    for name in names:
        data = (OUT / name).read_bytes()
        manifest["files"][name] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    manifest["book_cases"] = {
        "generator_ffmpeg_version": ffmpeg_version(),
        "chapters_ms": [[s, e, t] for s, e, t in BOOK_CHAPTERS],
        "files": names,
    }
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="reborn-fixtures-") as directory:
        temp = Path(directory)
        tags = {
            "title": "Reborn 440 Hz",
            "artist": "Y2 fixture",
            "album": "Reborn audio stack",
            "album_artist": "Y2 fixture",
            "track": "1/1",
            "disc": "1/1",
            "REPLAYGAIN_TRACK_GAIN": "-6.00 dB",
            "REPLAYGAIN_ALBUM_GAIN": "-8.00 dB",
            "REPLAYGAIN_TRACK_PEAK": "0.500000",
            "REPLAYGAIN_ALBUM_PEAK": "0.600000",
        }

        # The six explicitly qualified FLAC source combinations.
        for bits, rate in [(16, 44100), (16, 48000), (24, 44100), (24, 48000),
                           (24, 88200), (24, 96000)]:
            fmt = "s16" if bits == 16 else "s32"
            sine(
                OUT / f"flac-{bits}-{rate}.flac", rate, "flac",
                sample_fmt=fmt,
                metadata=tags if (bits, rate) == (16, 44100) else None,
            )

        sine(OUT / "source-no-metadata.flac", 44100, "flac", sample_fmt="s16")
        sine(OUT / "wav-pcm16.wav", 44100, "pcm_s16le", sample_fmt="s16")
        sine(OUT / "wav-pcm24.wav", 48000, "pcm_s24le", sample_fmt="s32")
        sine(OUT / "wav-pcm32.wav", 48000, "pcm_s32le", sample_fmt="s32")
        sine(OUT / "wav-float32.wav", 48000, "pcm_f32le", sample_fmt="flt")
        sine(OUT / "aiff-pcm24.aiff", 48000, "pcm_s24be", sample_fmt="s32", fmt="aiff")

        sine(OUT / "mp3.mp3", 44100, "libmp3lame", metadata=tags)
        sine(OUT / "aac.aac", 44100, "aac", fmt="adts", metadata=tags)
        sine(OUT / "m4a-aac.m4a", 44100, "aac", metadata=tags)
        sine(OUT / "m4a-alac.m4a", 48000, "alac", sample_fmt="s32p", metadata=tags)
        sine(OUT / "vorbis.ogg", 44100, "libvorbis", metadata=tags)
        sine(OUT / "opus.opus", 48000, "libopus", metadata=tags)

        # Keep the original short diagnostic names stable for existing device
        # commands while making their provenance deterministic too.
        sine(OUT / "tone.wav", 48000, "pcm_s16le", sample_fmt="s16", duration=1.0)
        sine(OUT / "tone.flac", 48000, "flac", sample_fmt="s16", duration=1.0)
        sine(OUT / "tone.mp3", 48000, "libmp3lame", duration=1.0)
        sine(OUT / "tone.m4a", 48000, "aac", duration=1.0)
        sine(OUT / "tone.ogg", 48000, "libvorbis", duration=1.0)
        sine(OUT / "tone.opus", 48000, "libopus", duration=1.0)

        ape = OUT / "ape-silence.ape"
        wavpack = OUT / "wavpack-silence.wv"
        if not ape.is_file() or not wavpack.is_file():
            raise FileNotFoundError(
                "checked-in ape-silence.ape and wavpack-silence.wv are required; "
                "the host FFmpeg build has no encoders for these decoder fixtures"
            )

        with tempfile.TemporaryDirectory(prefix="reborn-art-", dir=temp) as art_dir:
            art = Path(art_dir)
            png_path = art / "cover.png"
            png(png_path)
            jpeg_path = art / "cover.jpg"
            webp_path = art / "cover.webp"
            run("-i", str(png_path), "-frames:v", "1", "-c:v", "mjpeg", str(jpeg_path))
            run("-i", str(png_path), "-frames:v", "1", "-c:v", "libwebp", str(webp_path))
            shutil.copyfile(jpeg_path, OUT / "artwork-external.jpg")
            shutil.copyfile(png_path, OUT / "artwork-external.png")
            shutil.copyfile(webp_path, OUT / "artwork-external.webp")
            attach_art(OUT / "artwork-png.flac", OUT / "flac-16-44100.flac", png_path, "image/png")
            attach_art(OUT / "artwork-jpeg.flac", OUT / "flac-16-44100.flac", jpeg_path, "image/jpeg")
            attach_art(OUT / "artwork-webp.flac", OUT / "flac-16-44100.flac", webp_path, "image/webp")
            shutil.copyfile(OUT / "artwork-png.flac", OUT / "artwork.flac")

        gapless = make_gapless(temp)
        make_precision_flac(temp)
        make_loud_flac(OUT / "loud-24-96000-rg.flac")
        make_books(temp)
        covers = make_covers(temp)

    # An intentionally malformed ID3 size precedes valid MP3 frames. FFmpeg
    # must preserve the useful stream or return a clean error, never crash.
    mp3 = (OUT / "mp3.mp3").read_bytes()
    (OUT / "corrupt-metadata.mp3").write_bytes(
        b"ID3\x04\x00\x00\x00\x00\x00\x7f" + b"\0" * 4 + mp3
    )
    (OUT / "truncated-flac.flac").write_bytes((OUT / "flac-24-96000.flac").read_bytes()[:128])
    (OUT / "truncated-wav.wav").write_bytes((OUT / "wav-pcm16.wav").read_bytes()[:96])
    (OUT / "malformed.mp3").write_bytes(b"not a media file\x00\xff" * 10)

    files = {}
    for path in sorted(OUT.iterdir()):
        if path.is_file() and path.name != "manifest.json":
            files[path.name] = {
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "bytes": path.stat().st_size,
            }
    generator_version = ffmpeg_version()
    manifest = {
        "schema": "org.y2reborn.audio-fixtures/v2",
        "license": "CC0-1.0 for generated files; silent APE/WavPack decoder specimens are checked-in test inputs",
        "origin": f"deterministic 440 Hz and ramp signals generated locally by FFmpeg {generator_version}",
        "generator_ffmpeg_version": generator_version,
        "ffmpeg_required": TARGET_FFMPEG_VERSION,
        "source_combinations": [
            "FLAC 16/44.1", "FLAC 16/48", "FLAC 24/44.1", "FLAC 24/48",
            "FLAC 24/88.2", "FLAC 24/96", "WAV PCM16", "WAV PCM24",
            "WAV PCM32", "WAV float32", "MP3", "AAC ADTS", "M4A AAC",
            "ALAC", "Vorbis", "Opus", "AIFF PCM24", "APE", "WavPack",
        ],
        "metadata_cases": ["tagged ReplayGain", "no metadata", "corrupt metadata", "truncated audio",
                           "positive ReplayGain without a peak (near full scale)"],
        "artwork_cases": ["embedded JPEG", "embedded PNG", "embedded WebP",
                           "external JPEG", "external PNG", "external WebP"],
        "cover_cases": {"generator_ffmpeg_version": generator_version, "files": covers},
        "gapless": gapless,
        "precision": {"frames": 1024, "source_bits": 24,
                      "rates_hz": [44100, 48000],
                      "generator_ffmpeg_version": generator_version,
                      "reference": "native.rs packed24 low-bit test; positive/negative pairs share upper16 bits"},
        "files": files,
    }
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    import sys

    if sys.argv[1:] == ["--series"]:
        update_series()
    elif sys.argv[1:] == ["--books"]:
        update_books()
    elif sys.argv[1:] == ["--covers"]:
        update_covers()
    elif sys.argv[1:] == ["--films"]:
        update_films()
    else:
        main()

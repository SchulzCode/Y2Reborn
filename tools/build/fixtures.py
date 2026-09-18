#!/usr/bin/env python3
"""Generate deterministic local Reborn media fixtures with host FFmpeg 9.0.1.

The APE and WavPack specimens are checked-in, silent, public test inputs. The
host FFmpeg build cannot encode either format, so this generator validates and
copies those two small decoder fixtures instead of silently substituting a
different codec.
"""
from __future__ import annotations

import hashlib
import json
import os
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
    manifest = {
        "schema": "org.y2reborn.audio-fixtures/v2",
        "license": "CC0-1.0 for generated files; silent APE/WavPack decoder specimens are checked-in test inputs",
        "origin": "deterministic 440 Hz and ramp signals generated locally by FFmpeg 9.0.1",
        "ffmpeg_required": "9.0.1",
        "source_combinations": [
            "FLAC 16/44.1", "FLAC 16/48", "FLAC 24/44.1", "FLAC 24/48",
            "FLAC 24/88.2", "FLAC 24/96", "WAV PCM16", "WAV PCM24",
            "WAV PCM32", "WAV float32", "MP3", "AAC ADTS", "M4A AAC",
            "ALAC", "Vorbis", "Opus", "AIFF PCM24", "APE", "WavPack",
        ],
        "metadata_cases": ["tagged ReplayGain", "no metadata", "corrupt metadata", "truncated audio"],
        "artwork_cases": ["embedded JPEG", "embedded PNG", "embedded WebP",
                           "external JPEG", "external PNG", "external WebP"],
        "gapless": gapless,
        "files": files,
    }
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()

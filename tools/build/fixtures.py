#!/usr/bin/env python3
"""Generate original, 1 s, -46 dBFS stereo fixtures; host-only FFmpeg CLI."""
from pathlib import Path
import subprocess, json, hashlib
out=Path(__file__).resolve().parents[2]/'assets/fixtures'
out.mkdir(parents=True,exist_ok=True)
for ext,codec in [('wav','pcm_s16le'),('flac','flac'),('mp3','libmp3lame'),('m4a','aac'),('ogg','libvorbis'),('opus','libopus')]:
 subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-f','lavfi','-i','sine=frequency=440:sample_rate=48000:duration=1','-af','volume=0.04','-ac','2','-c:a',codec,str(out/('tone.'+ext))],check=True)
(out/'malformed.mp3').write_bytes(b'not a media file\x00\xff'*10)
(out/'manifest.json').write_text(json.dumps({'license':'CC0-1.0','origin':'Original deterministic synthesized sine; no third-party media','duration_seconds':1,'peak_dbfs':-46,'files':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(out.glob('tone.*'))}},indent=2)+'\n')

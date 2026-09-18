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
# Original solid-color PNG embedded in FLAC exercises the artwork decoder.
import struct,zlib,tempfile
with tempfile.TemporaryDirectory(prefix='reborn-cover-')as temp:
 cover=Path(temp)/'cover.png'
 def chunk(kind,data):return struct.pack('!I',len(data))+kind+data+struct.pack('!I',zlib.crc32(kind+data)&0xffffffff)
 raw=b''.join(b'\0'+bytes([40,160,90])*8 for _ in range(8))
 cover.write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!IIBBBBB',8,8,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b''))
 subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-i',str(out/'tone.flac'),'-i',str(cover),'-map','0:a','-map','1:v','-c','copy','-disposition:v','attached_pic',str(out/'artwork.flac')],check=True)
manifest=json.loads((out/'manifest.json').read_text());manifest['files']['artwork.flac']=hashlib.sha256((out/'artwork.flac').read_bytes()).hexdigest();manifest['artwork_origin']='Original 8x8 RGB (40,160,90) test pattern; CC0-1.0';(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')

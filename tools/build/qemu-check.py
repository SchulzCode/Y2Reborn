#!/usr/bin/env python3
"""Run installed ARM binaries in the platform's network/device-isolated build shell."""
import json,pathlib,subprocess,time,os
base=pathlib.Path('/build/buildroot/target');temp=pathlib.Path('/build/reborn-arm-check');temp.mkdir(exist_ok=True)
q=['qemu-arm','-cpu','cortex-a7','-L',str(base)]
sock=temp/'run/control.sock';data=temp/'data'
fixtures=base/'usr/share/reborn/fixtures'
cmd=q+[str(base/'usr/bin/reborn'),'--headless','--data-dir',str(data),'--socket',str(sock),'--music-dir',str(fixtures),'--fixtures',str(fixtures)]
proc=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
results=[]
try:
 for _ in range(100):
  if sock.exists():break
  if proc.poll()is not None:raise RuntimeError(proc.communicate())
  time.sleep(.1)
 for args in [['status'],['audio'],['test','decoder'],['test','database'],['test','library'],['metrics'],['diagnose']]:
  p=subprocess.run(q+[str(base/'usr/bin/rebornctl'),*args,'--socket',str(sock),'--json'],capture_output=True,timeout=60)
  value=json.loads(p.stdout);assert p.returncode==0,(args,p.returncode,value,p.stderr)
  results.append({'command':args,'result':value})
 runtime=results[0]['result']['decoder']['runtime']
 assert '9.0.1' in runtime['version'],runtime
 assert runtime['libraries']=={'libavutil':'61.1.101','libavcodec':'63.1.101','libavformat':'63.1.101','libavfilter':'12.1.101','libswresample':'7.1.101','libswscale':'10.1.101'},runtime
 required={
  'demuxers':{'flac','mp3','mov','ogg','wav','aac','aiff','ape','wv'},
  'audio_decoders':{'flac','mp3','mp3float','aac','alac','vorbis','opus','ape','wavpack','pcm_s16le','pcm_s16be','pcm_s24le','pcm_s24be','pcm_s32le','pcm_s32be','pcm_f32le','pcm_f32be'},
  'artwork_decoders':{'mjpeg','png','webp'},
  'parsers':{'flac','mpegaudio','aac','opus','vorbis'},
  'filters':{'abuffer','abuffersink','aformat','aresample','volume','equalizer','alimiter','acrossfade','amix','atrim','afade','asetnsamples'},
 }
 for field,names in required.items(): assert names.issubset(set(runtime[field])),(field,names-set(runtime[field]))
 assert set(runtime['protocols'])=={'file'},runtime['protocols']
 assert not runtime['encoders'] and not runtime['muxers'],runtime
 assert '--disable-network' in runtime['configuration'] and '--disable-avdevice' in runtime['configuration'],runtime['configuration']
 for cli in ['ffmpeg','ffplay','ffprobe']:
  assert not (base/'usr/bin'/cli).exists(),cli
 assert not list((base/'usr/lib').glob('libavdevice.so*'))
 for script in ['etc/init.d/S05reborn','usr/libexec/reborn-supervise']:
  subprocess.run(q+[str(base/'bin/busybox'),'sh','-n',str(base/script)],check=True)
 print(json.dumps({'passed':True,'hardware_validation':False,'checks':8,'results':results},indent=2))
finally:
 proc.terminate()
 try:stdout,stderr=proc.communicate(timeout=10)
 except subprocess.TimeoutExpired:proc.kill();stdout,stderr=proc.communicate();raise
 if proc.returncode not in [0,-15]:raise RuntimeError(stderr.decode(errors='replace'))

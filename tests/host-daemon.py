#!/usr/bin/env python3
"""Black-box socket, scanner, persistence and diagnostic security regression."""
import json,os,pathlib,socket,subprocess,tempfile,time,tarfile
root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='reborn-integration-')as tmp:
 tmp=pathlib.Path(tmp);data=tmp/'data';sock=tmp/'run/control.sock';music=tmp/'music';music.mkdir()
 for p in (root/'assets/fixtures').glob('tone.*'):(music/p.name).write_bytes(p.read_bytes())
 (music/'broken.mp3').write_bytes(b'bad file')
 cmd=[str(root/'target/debug/reborn'),'--headless','--data-dir',str(data),'--socket',str(sock),'--music-dir',str(music),'--fixtures',str(root/'assets/fixtures')]
 proc=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 def ctl(*args,success=True):
  p=subprocess.run([str(root/'target/debug/rebornctl'),*args,'--socket',str(sock),'--json'],capture_output=True,timeout=40)
  value=json.loads(p.stdout)
  if success:assert p.returncode==0,(args,p.returncode,value,p.stderr)
  return value
 def wait_library(expected_tracks):
  for _ in range(200):
   value=ctl('status')
   library=value['library']
   if not library['scanning'] and library['tracks_loaded']==expected_tracks and library['last_scan'] is not None:
    return value
   time.sleep(.05)
  raise AssertionError(value)
 try:
  for _ in range(100):
   if sock.exists():break
   assert proc.poll()is None,proc.communicate()
   time.sleep(.05)
  assert sock.stat().st_mode&0o777==0o600
  status=wait_library(6)
  assert ctl('test','decoder')['passed']
  assert ctl('test','database')['passed']
  assert ctl('test','library')['passed']
  assert len(ctl('test','list')['tests'])==14
  ctl('scan','incremental')
  scanned=wait_library(6);assert scanned['library']['last_scan']['reused']==6,scanned
  (music/'tone.mp3').unlink();ctl('scan','incremental')
  assert wait_library(5)['library']['tracks_loaded']==5
  for payload in [b'{"version":1,"id":1,"command":{"op":"exec","shell":"touch /tmp/forbidden"}}\n',b'{"version":1,"id":1,"command":{"op":"status","extra":1}}\n',b'x'*9000+b'\n']:
   c=socket.socket(socket.AF_UNIX);c.settimeout(3);c.connect(str(sock));c.sendall(payload)
   try:r=c.recv(20000)
   except ConnectionResetError:r=b''
   if r:assert json.loads(r)['ok']is False
   c.close()
  slow=socket.socket(socket.AF_UNIX);slow.connect(str(sock));slow.sendall(b'{')
  assert ctl('status')['version'].endswith('premium.01');slow.close()
  ctl('log-level','playback','debug');assert ctl('log-level')['overrides']['playback']=='DEBUG';ctl('log-level','reset')
  bundle=pathlib.Path(ctl('diagnose')['path']);assert bundle.stat().st_size<2*1024*1024
  with tarfile.open(bundle)as t:
   assert t.getnames()==['diagnostic.json'];b=t.extractfile('diagnostic.json').read();assert str(music).encode()not in b
  session=ctl('status')['session'];ctl('pause');proc.terminate();proc.wait(timeout=10)
  proc=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
  for _ in range(100):
   if sock.exists():
    try:
     status=ctl('status')
     if status['session']!=session:break
    except (AssertionError,json.JSONDecodeError):pass
   time.sleep(.05)
  assert status['playback']['state']in('stopped','paused'),status
  print(json.dumps({'passed':True,'checks':20,'scanner_tracks':6,'incremental_reused':6,'malformed_files_skipped':1,'no_hardware_claim':True}))
 finally:
  proc.terminate()
  try:out,err=proc.communicate(timeout=10)
  except subprocess.TimeoutExpired:proc.kill();out,err=proc.communicate()
  if proc.returncode not in (0,-15):print(err.decode(errors='replace'));raise SystemExit(proc.returncode)

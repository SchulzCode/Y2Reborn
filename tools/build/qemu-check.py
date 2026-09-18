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
 for args in [['status'],['test','decoder'],['test','database'],['test','library'],['metrics'],['diagnose']]:
  p=subprocess.run(q+[str(base/'usr/bin/rebornctl'),*args,'--socket',str(sock),'--json'],capture_output=True,timeout=60)
  value=json.loads(p.stdout);assert p.returncode==0,(args,p.returncode,value,p.stderr)
  results.append({'command':args,'result':value})
 assert results[0]['result']['decoder']['ffmpeg'].startswith('9.0.1'),results[0]
 for script in ['etc/init.d/S60reborn','usr/libexec/reborn-supervise']:
  subprocess.run(q+[str(base/'bin/busybox'),'sh','-n',str(base/script)],check=True)
 print(json.dumps({'passed':True,'hardware_validation':False,'checks':8,'results':results},indent=2))
finally:
 proc.terminate()
 try:stdout,stderr=proc.communicate(timeout=10)
 except subprocess.TimeoutExpired:proc.kill();stdout,stderr=proc.communicate();raise
 if proc.returncode not in [0,-15]:raise RuntimeError(stderr.decode(errors='replace'))

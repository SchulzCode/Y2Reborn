#!/usr/bin/env python3
"""Bounded owner-SSH qualification. Reads no private key bytes; never installs/flashes."""
import argparse,datetime,json,pathlib,subprocess,shlex,sys,hashlib

def classify(status_before,status_after,health,before,after,tests,kernel,expected_build=None):
 failures=[];warnings=[]
 if expected_build and any(s.get('build_id')!=expected_build for s in (status_before,status_after)): failures.append('unexpected Reborn build identity')
 if status_before.get('session')!=status_after.get('session'): failures.append('Reborn restarted during qualification')
 if health.get('overall')=='failed': failures.append('subsystem health failed')
 if health.get('overall')=='degraded': warnings.append('subsystem health degraded')
 if not tests.get('passed',False): failures.append('baseline test failed')
 renderer=status_after.get('graphics',{}).get('renderer') or ''
 if 'Mali400' not in renderer or any(s in renderer.lower() for s in ('llvmpipe','softpipe','swrast')): failures.append('hardware renderer unavailable')
 if after.get('audio_xruns',0)>before.get('audio_xruns',0): failures.append('audio XRUN count increased')
 if after.get('playback_errors',0)>before.get('playback_errors',0): failures.append('playback errors increased')
 if any(any(word in str(e).lower() for word in ('gpu hang','lima', 'drm')) for e in kernel): failures.append('relevant kernel GPU fault in snapshot; inspect timing')
 if not status_after.get('wifi',{}).get('saved'): warnings.append('Wi-Fi network not configured')
 if not any(d.get('connected') for d in status_after.get('bluetooth',{}).get('devices',[])): warnings.append('Bluetooth peer not connected')
 if not any(s.get('online') and s.get('kind',{}).get('kind')=='sd_card' for s in status_after.get('storage',[])): warnings.append('SD absent')
 return failures,warnings

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--host',default='root@10.42.0.1');p.add_argument('--identity',type=pathlib.Path,default=pathlib.Path.home()/'.ssh/y2linux_ed25519');p.add_argument('--known-hosts',type=pathlib.Path,required=True,help='Existing owner-approved pinned host key file');p.add_argument('--expected-build',help='Require this Reborn source commit before and after tests');p.add_argument('--output',type=pathlib.Path,default=pathlib.Path('out/qualification'));p.add_argument('--radio-scans',action='store_true',help='Explicit bounded Wi-Fi and Bluetooth discovery; restore prior power state');p.add_argument('--wired-audio',action='store_true',help='Explicit 1-second low-level wired signal, requires paused playback');a=p.parse_args()
 if not a.known_hosts.is_file():p.error('approved known-hosts pin is required')
 out=a.output/datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ');out.mkdir(parents=True,exist_ok=False)
 ssh=['ssh','-i',str(a.identity),'-o','BatchMode=yes','-o','StrictHostKeyChecking=yes','-o',f'UserKnownHostsFile={a.known_hosts}','-o','ConnectTimeout=5',a.host]
 records={};errors=[]
 def call(name,args,timeout=100):
  r=subprocess.run(ssh+[shlex.join(['rebornctl',*args,'--json'])],capture_output=True,timeout=timeout)
  (out/(name+'.stderr.txt')).write_bytes(r.stderr[:65536])
  try:value=json.loads(r.stdout)
  except (json.JSONDecodeError,UnicodeDecodeError):value={'ok':False,'error':'invalid or missing JSON','returncode':r.returncode}
  (out/(name+'.json')).write_text(json.dumps(value,indent=2)+'\n');records[name]=value
  if r.returncode>1 or isinstance(value,dict) and value.get('ok') is False:errors.append(name+' failed')
  return value
 try:
  status=call('status',['status']);health=call('health',['health']);before=call('metrics-before',['metrics']);snapshot=call('snapshot',['snapshot']);tests=call('test-results',['test','baseline'])
  if a.wired_audio:call('test-audio-wired',['test','audio-wired'])
  if a.radio_scans:
   call('test-wifi-scan',['test','wifi-scan']);call('test-bluetooth-scan',['test','bluetooth-scan','--seconds','10'])
  after=call('metrics-after',['metrics']);final=call('status-after',['status']);health_after=call('health-after',['health']);final_snapshot=call('snapshot-after',['snapshot']);events=call('logs',['logs','--last','200'])
  with (out/'selected-logs.jsonl').open('w')as f:
   if isinstance(events,list):
    for event in events:f.write(json.dumps(event)+'\n')
  failures,warnings=classify(status,final,health_after,before,after,tests,[e for e in final_snapshot.get('kernel_events',[]) if e not in snapshot.get('kernel_events',[])],a.expected_build);failures+=errors
  bundle=call('diagnostic',['diagnose']);name=bundle.get('path','');safe=pathlib.PurePosixPath(name)
  if safe.parent==pathlib.PurePosixPath('/data/reborn/diagnostics') and safe.name.startswith('reborn-diagnostic-') and safe.name.endswith('.tar.gz'):
   result=subprocess.run(ssh+[shlex.join(['cat',name])],capture_output=True,timeout=30)
   if result.returncode==0 and len(result.stdout)<=2*1024*1024:(out/safe.name).write_bytes(result.stdout)
   else:failures.append('diagnostic bundle transfer failed or exceeded bound')
  else:failures.append('diagnostic path rejected')
 except (subprocess.TimeoutExpired,OSError)as e:failures=[str(e)];warnings=[]
 summary={'schema':1,'passed':not failures,'failures':failures,'warnings':warnings,'physical_acceptance':False,'manual_items':['audible confirmation','visible rendering','wheel/buttons','SD insert/remove','pairing confirmation','charger changes','screen-off/wake and same-session suspend qualification']}
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');(out/'report.txt').write_text(('PASS (software checks only)'if summary['passed']else'FAIL')+'\n'+'\n'.join(failures+warnings)+'\n')
 (out/'SHA256SUMS').write_text(''.join(hashlib.sha256(f.read_bytes()).hexdigest()+'  '+f.name+'\n'for f in sorted(out.iterdir())if f.is_file()))
 print(json.dumps({'directory':str(out),**summary}));return 0 if summary['passed']else 1
if __name__=='__main__':sys.exit(main())

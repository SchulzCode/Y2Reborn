#!/usr/bin/env python3
"""Root-only preserving Reborn candidate. No device access. Source must be committed."""
import argparse,hashlib,json,pathlib,subprocess,shutil,tarfile,posixpath,re,os

def sha(path):
 h=hashlib.sha256()
 with open(path,'rb')as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def run(*args):return subprocess.check_output(args,stderr=subprocess.DEVNULL)
def root_only(scatter):
 chunks=scatter.split('- partition_index:')
 for i in range(1,len(chunks)):
  name=re.search(r'partition_name: (\S+)',chunks[i]).group(1)
  chunks[i]=re.sub(r'is_download: \S+','is_download: '+('true'if name=='ANDROID'else'false'),chunks[i])
  chunks[i]=re.sub(r'file_name: \S+','file_name: '+('Y2ROOT.img'if name=='ANDROID'else'NONE'),chunks[i])
 return '- partition_index:'.join(chunks)
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--platform',type=pathlib.Path,required=True);p.add_argument('--build',type=pathlib.Path,required=True);p.add_argument('--base',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();repo=pathlib.Path(__file__).resolve().parents[2]
 for source in [repo,a.platform]:
  if run('git','-C',str(source),'status','--porcelain').strip():raise SystemExit('Commit reviewed source before packaging: '+str(source))
 if a.output.exists():raise SystemExit('Fresh output directory required')
 image=a.build/'buildroot/images/rootfs.ext4';base=a.base/'Y2ROOT.img';boot=a.base/'BOOTIMG.img'
 base_manifest=json.loads((a.base/'manifest.json').read_text())
 for name,path in [('ANDROID',base),('BOOTIMG',boot)]:
  entry=next(v for v in base_manifest['payloads'] if v['target_partition']==name)
  assert sha(path)==entry['raw']['sha256'] and path.stat().st_size==entry['raw']['size_bytes'], 'base image identity mismatch' 
 assert image.stat().st_size<=536870912
 assert sha(a.build/'BOOTIMG.img')==sha(boot),'BOOTIMG unexpectedly changed'
 a.output.mkdir(parents=True);(a.output/'fallback').mkdir();(a.output/'metadata').mkdir()
 subprocess.run(['cp','--reflink=auto','--sparse=always',str(image),str(a.output/'Y2ROOT.img')],check=True)
 subprocess.run(['cp','--reflink=auto','--sparse=always',str(base),str(a.output/'fallback/Y2ROOT.img')],check=True)
 scatter=root_only((a.base/'MT6582_preserve_data_scatter.txt').read_text())
 for directory in [a.output,a.output/'fallback']:(directory/'MT6582_reborn_root_only_scatter.txt').write_text(scatter)
 for name in ['buildroot/.config','versions.json','owner-firmware.json']:
  shutil.copyfile(a.build/name,a.output/'metadata'/pathlib.Path(name).name)
 shutil.copyfile(repo/'Cargo.lock',a.output/'metadata/Cargo.lock');shutil.copyfile(repo/'docs/architecture/dependencies.json',a.output/'metadata/dependencies.json')
 root=a.output/'Y2ROOT.img'
 header=run('dumpe2fs','-h',str(root)).decode()
 assert re.search(r'Filesystem volume name:\s+Y2ROOT',header)
 assert '79324c69-6e75-4801-8000-000000000101' in header
 subprocess.run(['e2fsck','-fn',str(root)],check=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
 expected={'usr/bin/reborn','usr/bin/rebornctl','etc/init.d/S05reborn','usr/libexec/reborn-supervise','usr/share/reborn/fixtures/tone.flac'}
 with tarfile.open(a.build/'buildroot/images/rootfs.tar')as t:
  members={m.name.removeprefix('./'):m for m in t.getmembers()};assert expected<=members.keys()
  assert not any(n.startswith('data/reborn/')for n in members)
  for name in expected:
   content=t.extractfile(members[name]).read();assert run('debugfs','-R','cat /'+name,str(root))==content,name
   if name.startswith('usr/bin/'):
    assert content[:6]==b'\x7fELF\x01\x01'and int.from_bytes(content[18:20],'little')==40
    assert int.from_bytes(content[36:40],'little')&0x400
  for name,m in members.items():
   assert not name.startswith(('root/.ssh/','etc/dropbear/','data/network/','data/bluetooth/','data/connectivity/'))
   if m.isfile()and m.size<65536:assert b'PRIVATE KEY-----'not in t.extractfile(m).read(),name
  assert not any(n in members for n in ['usr/bin/ffmpeg','usr/bin/python3','usr/bin/node','usr/lib/dri/swrast_dri.so','usr/bin/Xorg'])
 assert run('debugfs','-R','cat /etc/y2linux/build-id',str(root)).strip()==b'Y2LINUX-REBORN-BASELINE-01'
 versions=json.loads(run('debugfs','-R','cat /etc/y2linux/versions.json',str(root)))
 def usage(p):
  out=run('dumpe2fs','-h',str(p)).decode();fields=dict(re.findall(r'^([^:\n]+):\s+(.*)$',out,re.M));return (int(fields['Block count'])-int(fields['Free blocks']))*int(fields['Block size'])
 manifest={'schema':'org.reborn.baseline-01/v1','status':'HOST_VALIDATED_PHYSICAL_PENDING','reborn_commit':run('git','-C',str(repo),'rev-parse','HEAD').decode().strip(),'integration_commit':run('git','-C',str(a.platform),'rev-parse','HEAD').decode().strip(),'kernel_source_commit':versions['kernel_source_commit'],'rust':'1.90.0','target':'armv7-unknown-linux-gnueabihf','root':{'file':'Y2ROOT.img','sha256':sha(root),'bytes':root.stat().st_size,'used_bytes':usage(root),'used_bytes_delta':usage(root)-usage(base)},'fallback':{'file':'fallback/Y2ROOT.img','sha256':sha(a.output/'fallback/Y2ROOT.img'),'bytes':base.stat().st_size},'bootimg_changed':False,'required_installed_bootimg':{'version':'6.18.0-y2linux-gpu-02','sha256':sha(boot)},'data_policy':'preserve Y2DATA, no payload or format','binary_sizes':{n:(a.build/'buildroot/target/usr/bin'/n).stat().st_size for n in ['reborn','rebornctl']}}
 (a.output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
 (a.output/'SHA256SUMS').write_text(''.join(sha(f)+'  '+str(f.relative_to(a.output))+'\n'for f in sorted(a.output.rglob('*'))if f.is_file()))
 print(json.dumps(manifest,indent=2))
if __name__=='__main__':main()

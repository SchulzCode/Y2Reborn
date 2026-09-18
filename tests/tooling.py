#!/usr/bin/env python3
import importlib.util,pathlib,unittest
root=pathlib.Path(__file__).resolve().parents[1]
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
qualification=module('qualification',root/'tools/qualification/reborn-baseline.py')
package=module('package',root/'tools/build/package.py')
class Tools(unittest.TestCase):
 def good(self):return {'session':'same','graphics':{'renderer':'Mali400'},'storage':[{},{}],'wifi':{'saved':[{}]},'bluetooth':{'devices':[{'connected':True}]}}
 def test_qualification_pass_and_optional_warnings(self):
  s=self.good();fails,warns=qualification.classify(s,s,{'overall':'ok'},{},{},{'passed':True},[]);self.assertEqual((fails,warns),([],[]))
  s['wifi']={};s['bluetooth']={};s['storage']=[];fails,warns=qualification.classify(s,s,{'overall':'ok'},{},{},{'passed':True},[]);self.assertFalse(fails);self.assertEqual(len(warns),3)
 def test_restart_xrun_and_software_renderer_fail(self):
  s=self.good();other={**s,'session':'new','graphics':{'renderer':'llvmpipe'}};fail,_=qualification.classify(s,other,{'overall':'failed'},{'audio_xruns':1},{'audio_xruns':2},{'passed':False},['lima timeout']);self.assertEqual(len(fail),6)
 def test_root_only_preserves_all_other_rows(self):
  source='header\n'+''.join(f'- partition_index: SYS{i}\n  partition_name: {n}\n  file_name: old.img\n  is_download: true\n  physical_start_addr: 0x123\n'for i,n in enumerate(['BOOTIMG','ANDROID','USRDATA','NVRAM','PROTECT_F']))
  out=package.root_only(source);self.assertEqual(out.count('is_download: true'),1);self.assertEqual(out.count('file_name: Y2ROOT.img'),1);self.assertEqual(out.count('file_name: NONE'),4);self.assertEqual(out.count('physical_start_addr: 0x123'),5)
if __name__=='__main__':unittest.main()

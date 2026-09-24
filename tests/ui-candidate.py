#!/usr/bin/env python3
"""Package selection and preview provenance checks; never accesses a device."""
import importlib.util,json,pathlib,unittest
from PIL import Image
root=pathlib.Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('package_ui',root/'tools/build/package_ui_v1.py');package=importlib.util.module_from_spec(spec);spec.loader.exec_module(package)
class Candidate(unittest.TestCase):
 def test_only_root_is_selected_and_geometry_is_preserved(self):
  names=['PRELOADER','LK','BOOTIMG','ANDROID','USRDATA','NVRAM','PROTECT_F','PROTECT_S','SECCFG']
  source='header\n'+''.join(f'- partition_index: SYS{i}\n  partition_name: {n}\n  file_name: old.img\n  is_download: true\n  physical_start_addr: 0x123\n  region: EMMC_USER\n'for i,n in enumerate(names))
  result=package.root_only(source)
  self.assertEqual(result.count('is_download: true'),1)
  self.assertEqual(result.count('file_name: Y2ROOT.img'),1)
  self.assertEqual(result.count('physical_start_addr: 0x123'),len(names))
  for malformed in [source.replace('ANDROID','ROOT'),source+source]:
   with self.assertRaises(ValueError):package.root_only(malformed)
 def test_debugfs_paths_cannot_add_commands(self):
  for path in ['a\nrm /etc','a" x','a\\b','a\x00']:
   with self.assertRaises(ValueError):package.quoted(path)
 def test_each_preview_is_native_resolution(self):
  previews=root/'docs/ui/previews/v1'
  images=[p for p in previews.glob('*.png') if p.stem!='contact-sheet']
  self.assertGreaterEqual(len(images),60)
  for path in images:
   with Image.open(path) as image:self.assertEqual(image.size,(480,360),path.name)
 def test_atlas_provenance_and_safe_bounds(self):
  import hashlib
  fonts=json.loads((root/'assets/fonts/provenance.json').read_text())
  icons=json.loads((root/'assets/icons/provenance.json').read_text())
  self.assertEqual(fonts['glyphs'],1001)
  self.assertEqual((root/'assets/fonts/reborn-ui.rgba').stat().st_size,1024*1024*4)
  for name,digest in icons['svg_sha256'].items():self.assertEqual(hashlib.sha256((root/'assets/icons/src'/f'{name}.svg').read_bytes()).hexdigest(),digest)
if __name__=='__main__':unittest.main()

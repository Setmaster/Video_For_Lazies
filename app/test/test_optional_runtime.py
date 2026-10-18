"""Isolated stdlib fixtures for the portable runtime wheel assembler."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import struct
import tempfile
import unittest
import zipfile
SOURCE = Path(__file__).resolve().parent.parent / 'optional-interpolation' / 'install_runtime.py'
spec = importlib.util.spec_from_file_location('vfl_runtime_helper', SOURCE)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)

class Assembly(unittest.TestCase):

    def fixture(self, entries, existing=None, mutate=None):
        temp = tempfile.TemporaryDirectory(prefix='vfl-wheel-fixture-')
        self.addCleanup(temp.cleanup)
        base = Path(temp.name)
        cache = base / 'cache'
        root = base / 'runtime'
        cache.mkdir()
        root.mkdir()
        (root / 'python.exe').write_bytes(b'inert fixture interpreter; never executed')
        if existing:
            existing(root)
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, 'w', zipfile.ZIP_DEFLATED) as z:
            for name, data, mode in entries:
                entry = zipfile.ZipInfo(name)
                entry.external_attr = (mode or stat.S_IFREG | 0o644) << 16
                entry.compress_type = zipfile.ZIP_DEFLATED
                z.writestr(entry, data)
        data = buf.getvalue()
        if mutate:
            data = mutate(data)
        digest = hashlib.sha256(data).hexdigest()
        wheel = {'id': 'fixture', 'name': 'fixture', 'version': '1', 'filename': 'fixture-1-py3-none-any.whl', 'sha256': digest, 'bytes': len(data)}
        (cache / (digest + '-' + wheel['filename'])).write_bytes(data)
        lock = {'schema': 1, 'id': 'fixture-runtime', 'platforms': {'windows-x86_64': {'pythonExecutable': 'python.exe', 'sitePackages': 'Lib/site-packages', 'wheels': [wheel]}}}
        lockfile = base / 'lock.json'
        lockfile.write_text(json.dumps(lock))
        return (cache, root, lockfile, wheel)

    def rejects(self, entries, existing=None, mutate=None):
        c, r, l, w = self.fixture(entries, existing, mutate)
        with self.assertRaises((ValueError, OSError, zipfile.BadZipFile)):
            helper.assemble(l, 'windows-x86_64', c, r)
        self.assertFalse((r / 'runtime-files.json').exists())

    def test_valid_library_data_and_inventory(self):
        c, r, l, w = self.fixture([('example/__init__.py', b'x=1\n', None), ('fixture.data/purelib/other.py', b'y=2\n', None), ('fixture.data/platlib/native.dat', b'no code', None), ('fixture.data/data/share/license', b'license', None), ('fixture.data/headers/item.h', b'header', None)])
        output = helper.assemble(l, 'windows-x86_64', c, r)
        self.assertEqual(output['status'], 'assembled')
        inv = json.loads((r / 'runtime-files.json').read_text())
        self.assertEqual(len(inv['files']), 6)
        self.assertEqual([x['path'] for x in inv['files']], sorted((x['path'] for x in inv['files'])))
        for x in inv['files']:
            self.assertEqual(x['sha256'], hashlib.sha256((r / x['path']).read_bytes()).hexdigest())
        self.assertEqual((r / 'python.exe').read_bytes(), b'inert fixture interpreter; never executed')

    def test_traversal(self):
        self.rejects([('../outside.py', b'x', None)])

    def test_absolute(self):
        self.rejects([('/outside.py', b'x', None)])

    def test_backslash(self):
        self.rejects([('pkg\\escape.py', b'x', None)])

    def test_drive(self):
        self.rejects([('C:/escape.py', b'x', None)])

    def test_duplicate(self):
        self.rejects([('pkg/x.py', b'x', None), ('pkg/x.py', b'x', None)])

    def test_case_collision(self):
        self.rejects([('pkg/X.py', b'x', None), ('pkg/x.py', b'y', None)])

    def test_archive_symlink(self):
        self.rejects([('pkg/link', b'../../outside', stat.S_IFLNK | 0o777)])

    def test_existing_path_symlink(self):

        def existing(root):
            (root / 'Lib').mkdir()
            (root / 'outside').mkdir()
            try:
                (root / 'Lib/site-packages').symlink_to(root / 'outside', target_is_directory=True)
            except OSError as error:
                if getattr(error, 'winerror', None) == 1314:
                    self.skipTest('This Windows account cannot create test symlinks')
                raise
        self.rejects([('pkg/x.py', b'x', None)], existing)

    def test_existing_file_collision(self):

        def existing(root):
            (root / 'Lib/site-packages/pkg').mkdir(parents=True)
            (root / 'Lib/site-packages/pkg/x.py').write_bytes(b'original')
        self.rejects([('pkg/x.py', b'x', None)], existing)

    def test_declared_entry_size(self):

        def mutate(data):
            out = bytearray(data)
            i = out.index(b'PK\x01\x02')
            struct.pack_into('<L', out, i + 24, 0xFFFFFFFF)
            return bytes(out)
        self.rejects([('pkg/x.py', b'x', None)], mutate=mutate)

    def test_hash_mismatch(self):
        c, r, l, w = self.fixture([('pkg/x.py', b'x', None)])
        (c / (w['sha256'] + '-' + w['filename'])).write_bytes(b'x' * w['bytes'])
        with self.assertRaises(ValueError):
            helper.assemble(l, 'windows-x86_64', c, r)
        self.assertFalse((r / 'Lib').exists())

    def test_scripts_rejected(self):
        self.rejects([('fixture.data/scripts/entry', b'code', None)])

    def test_windows_ambiguous(self):
        self.rejects([('pkg/CON.txt', b'x', None)])

    def namespace_pair(self, second_payload=b'', same_wheel_duplicate=False):
        c, r, l, w = self.fixture([('nvidia/__init__.py', b'', None)])
        o = json.loads(l.read_text())
        o['platforms']['windows-x86_64']['wheels'][0]['name'] = 'nvidia-fixture-a'
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, 'w') as z:
            z.writestr('nvidia/__init__.py', second_payload)
            z.writestr('nvidia_fixture_b.dist-info/LICENSE', b'fixture')
            if same_wheel_duplicate:
                z.writestr('nvidia/__init__.py', second_payload)
        data = buf.getvalue()
        sha = hashlib.sha256(data).hexdigest()
        second = {'id': 'nvidia-fixture-b', 'name': 'nvidia-fixture-b', 'version': '1', 'filename': 'nvidia_fixture_b-1-py3-none-any.whl', 'sha256': sha, 'bytes': len(data)}
        (c / (sha + '-' + second['filename'])).write_bytes(data)
        o['platforms']['windows-x86_64']['wheels'].append(second)
        l.write_text(json.dumps(o))
        return (c, r, l)

    def test_identical_empty_nvidia_namespace(self):
        c, r, l = self.namespace_pair()
        helper.assemble(l, 'windows-x86_64', c, r)
        self.assertEqual((r / 'Lib/site-packages/nvidia/__init__.py').read_bytes(), b'')

    def test_conflicting_nvidia_namespace(self):
        c, r, l = self.namespace_pair(b'executable code')
        with self.assertRaises(ValueError):
            helper.assemble(l, 'windows-x86_64', c, r)

    def test_same_wheel_namespace_duplicate(self):
        c, r, l = self.namespace_pair(same_wheel_duplicate=True)
        with self.assertRaises(ValueError):
            helper.assemble(l, 'windows-x86_64', c, r)

    def compiler_normalize(self):
        namespace = {'__name__': 'fixture_only'}
        exec(compile(helper.COMPILER_WRAPPERS['bin/vfl-compiler.py'], 'owned-compiler-fixture', 'exec'), namespace)
        return namespace['normalize']

    def test_compiler_exact_library_flag_forms(self):
        with tempfile.TemporaryDirectory(prefix='vfl-library-fixture-') as state:
            root = Path(state)
            (root / 'libcuda.so.1').write_bytes(b'inert library; never loaded')
            args = ['source.c', '-shared', '-l:libcuda.so.1', '-L' + state, '-O3']
            expected = ['source.c', '-shared', str(root / 'libcuda.so.1'), '-L' + state, '-O3']
            self.assertEqual(self.compiler_normalize()(args), expected)
            self.assertEqual(self.compiler_normalize()(['-L', state, '-l:libcuda.so.1']), ['-L', state, str(root / 'libcuda.so.1')])

    def test_compiler_other_flags_unchanged(self):
        args = ['-lcuda', '-O3', '-shared', '-fPIC', '-I/owned/include']
        self.assertEqual(self.compiler_normalize()(args), args)

    def test_compiler_missing_exact_library_rejected(self):
        with self.assertRaises(ValueError):
            self.compiler_normalize()(['-l:unavailable-runtime-library.so'])

    def test_compiler_library_path_escape_rejected(self):
        for name in ['../outside.so', '/outside.so', '..', 'a\\b']:
            with self.assertRaises(ValueError):
                self.compiler_normalize()(['-l:' + name])

    def test_owned_compiler_wrappers(self):
        c, r, l, w = self.fixture([('ziglang/zig', b'inert compiler fixture; never executed', None)])
        o = json.loads(l.read_text())
        cfg = o['platforms'].pop('windows-x86_64')
        cfg['compiler'] = 'zig'
        cfg['wheels'][0]['name'] = 'ziglang'
        o['platforms']['linux-x86_64'] = cfg
        l.write_text(json.dumps(o))
        helper.assemble(l, 'linux-x86_64', c, r)
        for name, data in helper.COMPILER_WRAPPERS.items():
            self.assertEqual((r / name).read_bytes(), data)
        if os.name != 'nt':
            for name in helper.COMPILER_WRAPPERS:
                self.assertEqual((r / name).stat().st_mode & 0o777, 0o755)

    def test_existing_compiler_wrapper_rejected(self):

        def existing(root):
            (root / 'bin').mkdir()
            (root / 'bin/vfl-cc').write_bytes(b'original')
        c, r, l, w = self.fixture([('ziglang/zig', b'inert compiler', None)], existing)
        o = json.loads(l.read_text())
        cfg = o['platforms'].pop('windows-x86_64')
        cfg['compiler'] = 'zig'
        cfg['wheels'][0]['name'] = 'ziglang'
        o['platforms']['linux-x86_64'] = cfg
        l.write_text(json.dumps(o))
        with self.assertRaises(ValueError):
            helper.assemble(l, 'linux-x86_64', c, r)

    def test_linux_native_case_distinct(self):
        if os.name == 'nt':
            self.skipTest('This fixture requires a case-sensitive filesystem')
        c, r, l, w = self.fixture([('pkg/x.py', b'x', None)])
        (r / 'share/terminfo/E').mkdir(parents=True)
        (r / 'share/terminfo/e').mkdir()
        (r / 'share/terminfo/E/Eterm').write_bytes(b'a')
        (r / 'share/terminfo/e/eterm').write_bytes(b'b')
        o = json.loads(l.read_text())
        o['platforms']['linux-x86_64'] = o['platforms'].pop('windows-x86_64')
        l.write_text(json.dumps(o))
        helper.assemble(l, 'linux-x86_64', c, r)
        inv = json.loads((r / 'runtime-files.json').read_text())
        self.assertIn('share/terminfo/E', inv['directories'])
        self.assertIn('share/terminfo/e', inv['directories'])
if __name__ == '__main__':
    unittest.main(verbosity=2)

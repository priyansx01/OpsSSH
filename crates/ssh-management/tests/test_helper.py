"""Linux filesystem tests use temporary homes; account commands are always mocked."""
import base64
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('helper', Path(__file__).parents[1] / 'src/helper.py')
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
KEY = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB test'


@unittest.skipUnless(sys.platform == 'linux', 'Requires Linux descriptor-relative filesystem operations')
class FilesystemTests(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.TemporaryDirectory(prefix='opsssh-helper-test-')
        self.addCleanup(self.home.cleanup)
        self.account = SimpleNamespace(pw_name='fixture', pw_dir=self.home.name, pw_uid=os.geteuid(), pw_gid=os.getegid())
        self.keys = Path(self.home.name) / '.ssh/authorized_keys'

    def install(self, data, expected=None):
        return helper.replace(self.account, helper.revision(b'') if expected is None else expected, base64.b64encode(data).decode())

    def test_atomic_replace_backup_modes_and_revision(self):
        original = b'# keep\r\nopaque\xff\n'
        first = self.install(original)
        self.assertEqual(base64.b64decode(first['content']), original)
        new = original + KEY.encode() + b'\n'
        self.install(new, first['revision'])
        self.assertEqual(self.keys.read_bytes(), new)
        self.assertEqual((self.keys.parent / '.authorized_keys.opsssh-backup').read_bytes(), original)
        for path in [self.keys, self.keys.parent / '.authorized_keys.opsssh-backup', self.keys.parent / '.opsssh-authorized-keys.lock']:
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(path.stat().st_uid, self.account.pw_uid)
        self.assertEqual(stat.S_IMODE(self.keys.parent.stat().st_mode), 0o700)

    def test_stale_revision_never_overwrites(self):
        self.install(b'original\n')
        with self.assertRaisesRegex(ValueError, 'changed on the server'):
            self.install(b'replacement\n')
        self.assertEqual(self.keys.read_bytes(), b'original\n')

    def test_same_content_file_replacement_during_backup_is_detected(self):
        first = self.install(b'original\n')
        atomic = helper.atomic_file
        def raced(directory, name, data, uid, gid):
            atomic(directory, name, data, uid, gid)
            if name == '.authorized_keys.opsssh-backup':
                other = self.keys.parent / 'external-edit'
                other.write_bytes(b'original\n')
                other.replace(self.keys)
        with patch.object(helper, 'atomic_file', side_effect=raced):
            with self.assertRaisesRegex(ValueError, 'changed during'):
                self.install(b'new\n', first['revision'])
        self.assertEqual(self.keys.read_bytes(), b'original\n')

    @unittest.skipUnless(os.geteuid() == 0, 'Root required to verify installation ownership')
    def test_root_installation_assigns_key_and_lock_to_target_account(self):
        self.account.pw_uid = 12345
        self.account.pw_gid = 12345
        self.install(KEY.encode() + b'\n')
        for path in [self.keys, self.keys.parent, self.keys.parent / '.opsssh-authorized-keys.lock']:
            self.assertEqual(path.stat().st_uid, 12345)
            self.assertEqual(path.stat().st_gid, 12345)

    def test_symlink_targets_are_refused(self):
        self.install(b'original\n')
        external = Path(self.home.name) / 'external'
        external.write_bytes(b'untouched')
        self.keys.unlink()
        self.keys.symlink_to(external)
        with self.assertRaises(OSError):
            helper.snapshot(self.account)
        self.assertEqual(external.read_bytes(), b'untouched')

    def test_fifo_key_file_is_rejected_without_waiting_for_a_writer(self):
        self.install(b'original\n')
        self.keys.unlink()
        os.mkfifo(self.keys)
        with self.assertRaisesRegex(ValueError, 'regular file'):
            helper.snapshot(self.account)

    def test_backup_symlink_and_unsafe_directory_are_refused(self):
        first = self.install(b'original\n')
        backup = self.keys.parent / '.authorized_keys.opsssh-backup'
        backup.unlink()
        backup.symlink_to(self.keys)
        with self.assertRaisesRegex(ValueError, 'nonregular'):
            self.install(b'new\n', first['revision'])
        self.assertEqual(self.keys.read_bytes(), b'original\n')
        self.keys.parent.chmod(0o777)
        with self.assertRaisesRegex(ValueError, 'permissions are unsafe'):
            helper.snapshot(self.account)

    def test_partial_creation_and_repair_never_delete_or_recreate(self):
        payload = dict(op='create', username='fixture', key=KEY, display='Fixture', shell='/bin/sh')
        import pwd
        with patch.object(os, 'geteuid', return_value=0), patch('shutil.which', return_value='/mock/tool'), patch.object(pwd, 'getpwnam', side_effect=[KeyError(), self.account]), patch.object(helper.subprocess, 'run', side_effect=[SimpleNamespace(returncode=0), SimpleNamespace(returncode=1)]) as run:
            result = helper.create_user(payload)
            self.assertFalse(result['ok'])
            self.assertEqual(result['created'], 'fixture')
            self.assertEqual(result['phase'], 'account activation')
            self.assertEqual([call.args[0][0] for call in run.call_args_list], ['useradd', 'chpasswd'])
        payload.update(op='repair', uid=self.account.pw_uid)
        with patch.object(os, 'geteuid', return_value=0), patch('shutil.which', return_value='/mock/tool'), patch.object(pwd, 'getpwnam', return_value=self.account), patch.object(helper.subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as run:
            self.assertTrue(helper.create_user(payload)['ok'])
            self.assertTrue(helper.create_user(payload)['ok'])
            self.assertEqual([call.args[0][0] for call in run.call_args_list], ['chpasswd', 'chpasswd'])
            self.assertEqual(self.keys.read_text().count(KEY), 1)

    def test_existing_account_and_repair_uid_mismatch_do_nothing(self):
        import pwd
        payload = dict(op='create', username='fixture', key=KEY, display='', shell='/bin/sh')
        with patch.object(os, 'geteuid', return_value=0), patch('shutil.which', return_value='/mock/tool'), patch.object(pwd, 'getpwnam', return_value=self.account), patch.object(helper.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'already exists'):
                helper.create_user(payload)
            payload.update(op='repair', uid=self.account.pw_uid+1)
            with self.assertRaisesRegex(ValueError, 'identity changed'):
                helper.create_user(payload)
            run.assert_not_called()


class FramingTests(unittest.TestCase):
    def invoke(self, data, result):
        stdin = SimpleNamespace(buffer=io.BytesIO(data))
        stdout = io.StringIO()
        with patch.object(sys, 'stdin', stdin), patch.object(sys, 'stdout', stdout), patch.object(helper, 'dispatch', return_value=result) as dispatch:
            code = helper.main()
        return code, json.loads(stdout.getvalue()), dispatch

    def test_sudo_consumed_or_unconsumed_password(self):
        frame = b'OPSSSH-MANAGEMENT-1\n{"op":"read"}'
        for data in [frame, b'secret-marker\n' + frame]:
            code, output, dispatch = self.invoke(data, dict(ok=True))
            self.assertEqual(code, 0)
            self.assertNotIn('secret-marker', json.dumps(output))
            dispatch.assert_called_once_with(dict(op='read'))

    def test_bad_framing_and_oversized_payload_never_dispatch(self):
        for data in [b'secret-marker\ninvalid\n', b'OPSSSH-MANAGEMENT-1\n' + b'x' * (2*helper.MAX_DOCUMENT+1)]:
            code, output, dispatch = self.invoke(data, dict(ok=True))
            self.assertEqual(code, 1)
            self.assertNotIn('secret-marker', json.dumps(output))
            dispatch.assert_not_called()


if __name__ == '__main__':
    unittest.main()

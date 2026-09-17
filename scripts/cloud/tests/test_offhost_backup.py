import contextlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('backup', Path(__file__).parents[1] / 'offhost_backup.py')
b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)

class BackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve() / 'source'; self.root.mkdir()
        (self.root / 'instances/id/data').mkdir(parents=True)
        (self.root / 'instances/id/data/value').write_text('preserved')
        (self.root / 'sessions.sqlite').write_text('session secret')
        with contextlib.closing(sqlite3.connect(self.root / 'registry.sqlite')) as db:
            db.execute('PRAGMA user_version=2'); db.execute('CREATE TABLE permissions (id TEXT)')
            db.execute("INSERT INTO permissions VALUES ('original-id')"); db.commit()
        self.env = patch.dict(os.environ, {'BOG_BACKUP_BUCKET':'test-bucket', 'BOG_BACKUP_PREFIX':'host-a/', 'BOG_BACKUP_AGE_RECIPIENT':'age1test', 'BOG_BACKUP_AGE_IDENTITY':'PRIVATE-SECRET'})
        self.env.start(); self.addCleanup(self.env.stop)
        self.calls = []; self.archive = Path(self.temp.name).resolve() / 'saved.age'
    def command(self, args, **kwargs):
        self.assertNotIn('PRIVATE-SECRET', ' '.join(args))
        if args[0] == 'age':
            import shutil
            shutil.copyfile(args[-1], args[args.index('-o')+1]); return b''
    def store(self, args):
        self.calls.append(args)
        if args[0] == 'put-object':
            import shutil
            shutil.copyfile(args[args.index('--body')+1], self.archive)
        if args[0] == 'list-objects-v2':
            return json.dumps({'Contents':[
                {'Key':'host-a/20000101T000000Z-00000000-0000-0000-0000-000000000000.tar.age', 'LastModified':'2000-01-01T00:00:00Z'},
                {'Key':'host-a/unrelated', 'LastModified':'2000-01-01T00:00:00Z'}]}).encode()
        return b'{}'
    def test_roundtrip_permissions_and_retention(self):
        with patch.object(b, 'command', self.command), patch.object(b, 's3', self.store):
            b.backup(self.root)
            target = Path(self.temp.name).resolve() / 'restored'; b.restore(self.archive, target)
        self.assertEqual((target/'instances/id/data/value').read_text(), 'preserved')
        self.assertFalse((target/'sessions.sqlite').exists())
        self.assertFalse((target/'instances/id/worker.lock').exists())
        with contextlib.closing(sqlite3.connect(target/'registry.sqlite')) as db:
            self.assertEqual(db.execute('SELECT id FROM permissions').fetchone()[0], 'original-id')
        self.assertEqual(sum(c[0]=='delete-object' for c in self.calls), 1)
    def test_failed_upload_never_deletes(self):
        def fail(args):
            self.calls.append(args); raise b.BackupError('failed')
        with patch.object(b, 'command', self.command), patch.object(b, 's3', fail):
            with self.assertRaises(b.BackupError): b.backup(self.root)
        self.assertEqual([c[0] for c in self.calls], ['put-object'])
    def test_symlink_refused(self):
        (self.root/'instances/id/data/link').symlink_to('/etc/passwd')
        stage = Path(self.temp.name).resolve()/'stage'; stage.mkdir()
        with self.assertRaises(b.BackupError): b.snapshot(self.root, stage)
    def test_running_manager_refused(self):
        with b.closed(self.root):
            with self.assertRaises(BlockingIOError):
                with b.closed(self.root): pass
    def test_surviving_worker_and_fjall_lock_refused(self):
        import fcntl
        for name in ('worker.lock', 'data/lock'):
            with (self.root/'instances/id'/name).open('a') as held:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(BlockingIOError):
                    with b.closed(self.root): pass
    def test_checksum_tamper_refused(self):
        import io, tarfile
        with patch.object(b, 'command', self.command), patch.object(b, 's3', self.store):
            b.backup(self.root)
        modified = self.archive.with_suffix('.tampered')
        with tarfile.open(self.archive) as source, tarfile.open(modified, 'w') as dest:
            for member in source:
                data = source.extractfile(member).read()
                if member.name.endswith('/value'): data = b'altered!!'
                member.size = len(data); dest.addfile(member, io.BytesIO(data))
        target = Path(self.temp.name).resolve()/'restored'
        with patch.object(b, 'command', self.command):
            with self.assertRaisesRegex(b.BackupError, 'checksum mismatch'): b.restore(modified, target)
        self.assertFalse(target.exists())
    def test_tamper_and_traversal_refused(self):
        import io, tarfile
        for name in ('../escape', 'registry.sqlite'):
            with tarfile.open(self.archive, 'w') as tar:
                info = tarfile.TarInfo(name); info.size = 4; tar.addfile(info, io.BytesIO(b'evil'))
            target = Path(self.temp.name).resolve()/'restored'
            with patch.object(b, 'command', self.command):
                with self.assertRaises(Exception): b.restore(self.archive, target)
            self.assertFalse(target.exists())
    def test_child_error_redacted(self):
        with self.assertRaisesRegex(b.BackupError, 'output suppressed') as error:
            b.command(['sh','-c','echo PRIVATE-SECRET >&2; exit 1'])
        self.assertNotIn('PRIVATE-SECRET', str(error.exception))

if __name__ == '__main__': unittest.main()

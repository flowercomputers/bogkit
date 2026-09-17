#!/usr/bin/env python3
"""Closed-host, whole-registry encrypted recovery archives. See docs/bog-cloud-backups.md."""
import argparse
import contextlib
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import sqlite3
import stat
import subprocess
import tarfile
import tempfile
import uuid

class BackupError(Exception):
    pass

def command(args, **kwargs):
    # Child stderr can contain credentials or endpoint URLs. Never echo it.
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
    if result.returncode:
        raise BackupError('external command failed (output suppressed)')
    return result.stdout

def regular_tree(root):
    for path in sorted(root.rglob('*')):
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise BackupError('symlinks are forbidden')
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISSOCK(mode)):
            raise BackupError('special files are forbidden')
        if stat.S_ISREG(mode) and path.name not in ('lock', 'worker.lock'):
            yield path

def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()

@contextlib.contextmanager
def closed(root):
    if root.is_symlink() or not root.is_dir():
        raise BackupError('source must be a real directory')
    handles = []
    try:
        paths = [root / 'manager.lock']
        instances = root / 'instances'
        if instances.is_symlink():
            raise BackupError('symlinks are forbidden')
        # Manager lock is first: blocks new workers before enumerating instances.
        def acquire(path):
            fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            handles.append(fd)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        acquire(paths[0])
        for instance in sorted(instances.iterdir()):
            if instance.is_symlink() or not instance.is_dir():
                raise BackupError('invalid instance directory')
            acquire(instance / 'worker.lock')
            data = instance / 'data'
            if data.is_symlink():
                raise BackupError('symlinks are forbidden')
            if data.exists():
                acquire(data / 'lock')
        yield
    finally:
        for fd in reversed(handles):
            os.close(fd)

def snapshot(root, stage):
    with closed(root):
        registry = root / 'registry.sqlite'
        if registry.is_symlink() or not registry.is_file():
            raise BackupError('invalid registry')
        with contextlib.closing(sqlite3.connect(registry.as_uri() + '?mode=ro', uri=True)) as source:
            with contextlib.closing(sqlite3.connect(stage / 'registry.sqlite')) as dest:
                source.backup(dest)
                version = dest.execute('PRAGMA user_version').fetchone()[0]
                if dest.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                    raise BackupError('registry integrity failed')
        (stage / 'instances').mkdir()
        for path in regular_tree(root / 'instances'):
            relative = path.relative_to(root)
            target = stage / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
        files = {str(p.relative_to(stage)): {'sha256': digest(p), 'size': p.stat().st_size}
                 for p in regular_tree(stage)}
        (stage / 'manifest.json').write_text(json.dumps({
            'format': 1, 'registry_version': version, 'files': files}, sort_keys=True))

def sync_file(path):
    with path.open('rb') as stream:
        os.fsync(stream.fileno())

def s3(args):
    endpoint = os.environ.get('BOG_BACKUP_ENDPOINT')
    base = ['aws']
    if endpoint:
        if not endpoint.startswith('https://') or '@' in endpoint or '?' in endpoint:
            raise BackupError('endpoint must be HTTPS without credentials/query')
        base += ['--endpoint-url', endpoint]
    return command(base + ['s3api'] + args)

def destination():
    bucket = os.environ['BOG_BACKUP_BUCKET']
    prefix = os.environ['BOG_BACKUP_PREFIX']
    if not re.fullmatch(r'[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]', bucket):
        raise BackupError('invalid bucket')
    if not re.fullmatch(r'[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*/', prefix):
        raise BackupError('dedicated nonempty prefix required')
    return bucket, prefix

def backup(root):
    bucket, prefix = destination()
    recipient = os.environ['BOG_BACKUP_AGE_RECIPIENT']
    if not re.fullmatch(r'age1[a-z0-9]+', recipient):
        raise BackupError('age public recipient required')
    with tempfile.TemporaryDirectory(prefix='bog-offhost-') as temp:
        work = Path(temp)
        stage = work / 'snapshot'
        stage.mkdir(mode=0o700)
        snapshot(root, stage)
        archive = work / 'archive.tar'
        with tarfile.open(archive, 'w') as tar:
            for path in sorted(stage.rglob('*')):
                if path.is_file():
                    tar.add(path, arcname=str(path.relative_to(stage)), recursive=False)
        encrypted = work / 'backup.age'
        command(['age', '-r', recipient, '-o', str(encrypted), str(archive)])
        sync_file(encrypted)
        key = prefix + dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + str(uuid.uuid4()) + '.tar.age'
        s3(['put-object', '--bucket', bucket, '--key', key, '--body', str(encrypted), '--acl', 'private'])
        # Retention runs ONLY after successful upload; CLI automatically paginates.
        listing = json.loads(s3(['list-objects-v2', '--bucket', bucket, '--prefix', prefix, '--output', 'json']))
        cutoff = dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=7)
        pattern = re.compile(re.escape(prefix) + r'\d{8}T\d{6}Z-[0-9a-f-]{36}\.tar\.age')
        for item in listing.get('Contents', []):
            if pattern.fullmatch(item['Key']) and dt.datetime.fromisoformat(item['LastModified'].replace('Z', '+00:00')) < cutoff:
                s3(['delete-object', '--bucket', bucket, '--key', item['Key']])
        return key

def restore(archive, target):
    if target.exists() or target.is_symlink():
        raise BackupError('restore destination must not exist')
    if target.parent.resolve() != target.parent.absolute():
        raise BackupError('destination parent must not contain symlinks')
    with tempfile.TemporaryDirectory(prefix='.bog-restore-', dir=target.parent) as temp:
        work = Path(temp)
        decrypted = work / 'archive.tar'
        # Identity secret is stdin, never argv or logs. Environment is inherited only by operator-owned tools.
        identity = os.environ['BOG_BACKUP_AGE_IDENTITY']
        command(['age', '-d', '-i', '/dev/stdin', '-o', str(decrypted), str(archive)], input=identity.encode() + b'\n')
        stage = work / 'root'
        stage.mkdir(mode=0o700)
        with tarfile.open(decrypted) as tar:
            seen = set()
            for member in tar:
                name = PurePosixPath(member.name)
                if not member.isfile() or name.is_absolute() or '..' in name.parts or str(name) != member.name or member.name in seen:
                    raise BackupError('unsafe archive member')
                if member.name != 'manifest.json' and member.name != 'registry.sqlite' and not member.name.startswith('instances/'):
                    raise BackupError('unexpected archive member')
                seen.add(member.name)
                path = stage / member.name
                path.parent.mkdir(parents=True, exist_ok=True)
                with tar.extractfile(member) as source, path.open('xb') as dest:
                    shutil.copyfileobj(source, dest)
        manifest = json.loads((stage / 'manifest.json').read_text())
        if manifest['format'] != 1 or set(manifest['files']) != seen - {'manifest.json'}:
            raise BackupError('manifest mismatch')
        for name, info in manifest['files'].items():
            path = stage / name
            if path.stat().st_size != info['size'] or digest(path) != info['sha256']:
                raise BackupError('checksum mismatch')
        with contextlib.closing(sqlite3.connect((stage / 'registry.sqlite').as_uri() + '?mode=ro', uri=True)) as db:
            if db.execute('PRAGMA integrity_check').fetchone()[0] != 'ok' or db.execute('PRAGMA user_version').fetchone()[0] != manifest['registry_version']:
                raise BackupError('registry mismatch')
        (stage / 'instances').mkdir(exist_ok=True)
        for path in stage.rglob('*'):
            path.chmod(0o700 if path.is_dir() else 0o600)
            if path.is_file():
                sync_file(path)
        for directory in sorted((p for p in stage.rglob('*') if p.is_dir()), reverse=True) + [stage]:
            fd = os.open(directory, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        # Same filesystem rename publishes only a fully verified snapshot.
        if target.exists():
            raise BackupError('destination appeared during restore')
        os.rename(stage, target)
        fd = os.open(target.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

def drill(key):
    """Download one encrypted object and validate isolated whole-host restoration."""
    bucket, prefix = destination()
    if not re.fullmatch(re.escape(prefix) + r'\d{8}T\d{6}Z-[0-9a-f-]{36}\.tar\.age', key):
        raise BackupError('object outside owned archive namespace')
    with tempfile.TemporaryDirectory(prefix='bog-drill-') as temp:
        work = Path(temp).resolve()
        archive = work / 'backup.age'
        s3(['get-object', '--bucket', bucket, '--key', key, str(archive)])
        restore(archive, work / 'restored')


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    b = sub.add_parser('backup'); b.add_argument('root', type=Path)
    d = sub.add_parser('drill'); d.add_argument('key')
    r = sub.add_parser('restore'); r.add_argument('archive', type=Path); r.add_argument('target', type=Path)
    args = parser.parse_args()
    try:
        if args.action == 'backup':
            print(backup(args.root.absolute()))
        elif args.action == 'drill':
            drill(args.key)
        else:
            restore(args.archive.resolve(), args.target.absolute())
    except Exception:
        parser.exit(1, 'Backup operation failed; no child output or credentials disclosed.\n')
    print('Backup operation completed.')

if __name__ == '__main__':
    main()

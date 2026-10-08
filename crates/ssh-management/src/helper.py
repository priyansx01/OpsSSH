"""One-shot Linux management helper. Payloads and passwords never enter diagnostics."""
import base64
import hashlib
import json
import os
import re
import secrets
import stat
import subprocess
import sys

MAX_DOCUMENT = 1024 * 1024

def revision(data):
    return hashlib.sha256(data).hexdigest()

def file_identity(metadata):
    if metadata is None:
        return None
    return (metadata.st_dev, metadata.st_ino, metadata.st_uid, metadata.st_gid,
        metadata.st_mode, metadata.st_ctime_ns)

def read_file(directory, name):
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    except FileNotFoundError:
        return b'', None
    with os.fdopen(fd, 'rb') as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError('Authorized keys must be a regular file')
        data = stream.read(MAX_DOCUMENT + 1)
        if len(data) > MAX_DOCUMENT:
            raise ValueError('Authorized keys file exceeds 1 MiB')
        return data, metadata

def ssh_directory(account, create=False):
    home = os.open(account.pw_dir, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        if create:
            try:
                os.mkdir('.ssh', 0o700, dir_fd=home)
                os.chown('.ssh', account.pw_uid, account.pw_gid, dir_fd=home, follow_symlinks=False)
            except FileExistsError:
                pass
        directory = os.open('.ssh', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=home)
        metadata = os.fstat(directory)
        if metadata.st_uid not in (account.pw_uid, 0) or metadata.st_mode & 0o022:
            os.close(directory)
            raise ValueError('.ssh ownership or permissions are unsafe; review them on the server')
        return directory
    finally:
        os.close(home)

def atomic_file(directory, name, data, uid, gid):
    # Existing symlinks are refused even though rename would replace their directory entry.
    try:
        metadata = os.stat(name, dir_fd=directory, follow_symlinks=False)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError('Refusing a nonregular key or backup file')
    except FileNotFoundError:
        pass
    temporary = '.opsssh-' + secrets.token_hex(16)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchown(stream.fileno(), uid, gid)
            os.fchmod(stream.fileno(), 0o600)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(temporary, name, src_dir_fd=directory, dst_dir_fd=directory)
        os.fsync(directory)
    finally:
        try:
            os.unlink(temporary, dir_fd=directory)
        except FileNotFoundError:
            pass

def load_account(name):
    import pwd
    account = pwd.getpwnam(name)
    if os.geteuid() not in (0, account.pw_uid):
        raise PermissionError('This account requires administrator authorization')
    return account

def snapshot(account):
    try:
        directory = ssh_directory(account)
    except FileNotFoundError:
        data, metadata = b'', None
    else:
        try:
            data, metadata = read_file(directory, 'authorized_keys')
        finally:
            os.close(directory)
    import shutil
    shells = ['/bin/bash', '/bin/sh']
    shells = [shell for shell in shells if os.path.isfile(shell) and os.access(shell, os.X_OK)]
    return dict(ok=True, account=account.pw_name, uid=account.pw_uid,
        path=os.path.join(account.pw_dir, '.ssh', 'authorized_keys'), content=base64.b64encode(data).decode(),
        revision=revision(data), writable=(os.geteuid() == 0 or (metadata is None or metadata.st_uid == os.geteuid())),
        can_create=bool(shutil.which('useradd') and shutil.which('chpasswd') and (os.geteuid() == 0 or shutil.which('sudo'))), shells=shells)

def replace(account, expected, content):
    import fcntl
    data = base64.b64decode(content, validate=True)
    if len(data) > MAX_DOCUMENT or b'\0' in data:
        raise ValueError('Invalid authorized keys document')
    directory = ssh_directory(account, True)
    try:
        lock = os.open('.opsssh-authorized-keys.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600, dir_fd=directory)
        with os.fdopen(lock, 'rb') as stream:
            lock_metadata = os.fstat(stream.fileno())
            if not stat.S_ISREG(lock_metadata.st_mode) or lock_metadata.st_uid not in (0, account.pw_uid):
                raise ValueError('Unsafe authorized keys lock file')
            if os.geteuid() == 0:
                os.fchown(stream.fileno(), account.pw_uid, account.pw_gid)
            os.fchmod(stream.fileno(), 0o600)
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            original, metadata = read_file(directory, 'authorized_keys')
            if revision(original) != expected:
                raise ValueError('Authorized keys changed on the server. Refresh and review your edit again.')
            uid, gid = (metadata.st_uid, metadata.st_gid) if metadata else (account.pw_uid, account.pw_gid)
            if metadata and metadata.st_uid not in (0, account.pw_uid):
                raise ValueError('Authorized keys owner does not match this account')
            atomic_file(directory, '.authorized_keys.opsssh-backup', original, uid, gid)
            current, current_metadata = read_file(directory, 'authorized_keys')
            if revision(current) != expected or file_identity(current_metadata) != file_identity(metadata):
                raise ValueError('Authorized keys changed during the operation. Refresh before retrying.')
            atomic_file(directory, 'authorized_keys', data, uid, gid)
    finally:
        os.close(directory)
    return snapshot(account)

def valid_user(name):
    return isinstance(name, str) and re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', name) is not None

def create_user(payload):
    import pwd
    if os.geteuid() != 0:
        raise PermissionError('Creating Linux users requires root or explicit sudo authorization')
    name = payload.get('username', '')
    if not valid_user(name):
        raise ValueError('Use a lowercase Linux username of up to 32 characters')
    key = payload.get('key', '')
    if not isinstance(key, str) or len(key) > 8192 or '\n' in key or '\r' in key or '\0' in key or not re.match(r'^(ssh-|ecdsa-|sk-)', key):
        raise ValueError('A valid single-line public key is required')
    shell = payload.get('shell', '/bin/sh')
    if shell not in ('/bin/bash', '/bin/sh') or not os.path.isfile(shell) or not os.access(shell, os.X_OK):
        raise ValueError('Select an available login shell')
    display = payload.get('display', '')
    if not isinstance(display, str) or len(display) > 128 or any(ord(c) < 32 for c in display) or ':' in display:
        raise ValueError('Invalid display name')
    import shutil
    if not shutil.which('useradd') or not shutil.which('chpasswd'):
        raise ValueError('User creation requires useradd and chpasswd on this server')
    repair = payload.get('op') == 'repair'
    try:
        existing = pwd.getpwnam(name)
    except KeyError:
        existing = None
    if repair:
        if existing is None or existing.pw_uid != payload.get('uid'):
            raise ValueError('Created account identity changed; review it on the server')
    elif existing is not None:
        raise ValueError('This username already exists; no account was modified')
    created = existing if repair else None
    phase = 'account creation'
    try:
        if not repair:
            result = subprocess.run(['useradd', '-m', '-s', shell, '-c', display, '--', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            if result.returncode:
                raise ValueError('useradd failed; check account policy and administrator permissions')
            created = pwd.getpwnam(name)
        phase = 'account activation'
        # Unissued random credential prevents locked-account behavior blocking key authentication.
        password = secrets.token_urlsafe(48)
        result = subprocess.run(['chpasswd'], input=(name + ':' + password + '\n').encode(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        del password
        if result.returncode:
            raise ValueError('Could not activate the new account for key authentication')
        phase = 'public key installation'
        before = snapshot(created)
        existing_data = base64.b64decode(before['content'])
        line = key.encode() + b'\n'
        already_present = any(row.split()[:2] == key.encode().split()[:2] for row in existing_data.splitlines())
        data = existing_data if already_present else existing_data + (b'\n' if existing_data and not existing_data.endswith(b'\n') else b'') + line
        replace(created, before['revision'], base64.b64encode(data).decode())
        return dict(ok=True, created=name, uid=created.pw_uid, installed=True, phase='complete')
    except Exception:
        if created is not None:
            return dict(ok=False, error='Account exists, but ' + phase + ' did not complete. Review or repair this account; it was not deleted.', created=name, uid=created.pw_uid, phase=phase)
        raise

def dispatch(payload):
    if sys.platform != 'linux':
        raise ValueError('SSH Management currently supports Linux with Python 3')
    if payload.get('op') in ('create', 'repair'):
        return create_user(payload)
    account = load_account(payload['account'])
    if payload.get('op') == 'read':
        return snapshot(account)
    if payload.get('op') == 'replace':
        return replace(account, payload['expected'], payload['content'])
    raise ValueError('Unsupported management operation')

def main():
    try:
        # sudo may consume a credential line or leave it untouched for NOPASSWD.
        first = sys.stdin.buffer.readline(8194)
        if first != b'OPSSSH-MANAGEMENT-1\n':
            first = sys.stdin.buffer.readline(64)
        if first != b'OPSSSH-MANAGEMENT-1\n':
            raise ValueError('Invalid management payload framing')
        raw = sys.stdin.buffer.read(2 * MAX_DOCUMENT + 1)
        if len(raw) > 2 * MAX_DOCUMENT:
            raise ValueError('Management payload exceeds its limit')
        result = dispatch(json.loads(raw))
    except PermissionError:
        result = dict(ok=False, error='Permission denied. Use an account that owns this key file, or authorize account creation with sudo.')
    except (ValueError, KeyError) as error:
        result = dict(ok=False, error=str(error))
    except Exception:
        result = dict(ok=False, error='Management operation failed. Refresh to verify server state before retrying.')
    print(json.dumps(result))
    return 0 if result.get('ok') else 1

if __name__ == '__main__':
    sys.exit(main())

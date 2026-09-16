#!/usr/bin/env bash
# Standalone package installer; Python is used for safe paths and rollback.
set -euo pipefail
command -v python3 >/dev/null || { printf '%s\n' 'python3 is required' >&2; exit 1; }
exec python3 - "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)" "$@" <<'PY'
import argparse, ctypes, hashlib, os, pathlib, re, shutil, signal, subprocess, sys, tempfile, time
P = pathlib.Path

def fail(message):
    raise RuntimeError(message)

def run(*args, check=True):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=check)

def safe(path):
    if not path.is_absolute() or any(c in str(path) for c in '\n\r\0%$"\\`'):
        fail(f'Unsupported path: {path}')
    for p in (path, *path.parents):
        if p.is_symlink():
            fail(f'Symlink not allowed: {p}')
        if p.exists() and p.stat().st_uid not in (0, os.getuid()):
            fail(f'Foreign-owned path: {p}')
        if p.exists() and p.stat().st_mode & 0o022 and p != P('/tmp'):
            fail(f'Group/world writable path: {p}')
    if path.exists() and path.stat().st_uid != os.getuid():
        fail(f'Not owned by current user: {path}')

def owned_file(path):
    safe(path)
    if path.exists() and (not path.is_file() or path.stat().st_nlink != 1):
        fail(f'Not a regular single-link file: {path}')

def main():
    parser = argparse.ArgumentParser(description='Reinstall Yihu without changing feature choices or configuration')
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--restart-nautilus', action='store_true', help='Explicitly allow Nautilus to quit after an extension update')
    args = parser.parse_args(sys.argv[2:])
    if os.getuid() == 0:
        fail('Do not run as root or with sudo')
    payload = P(sys.argv[1])
    for cmd in ('ldd', 'pgrep', 'systemctl'):
        if not shutil.which(cmd):
            fail(f'Missing prerequisite: {cmd}')
    required = ['yihu', 'autodark-agent', 'ARCH', 'VERSION', 'BUILD_EXE', 'reinstall.sh'] + [f'icons/{s}.png' for s in ('32x32', '128x128', '256x256', '512x512')]
    manifest = {}
    for line in (payload / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        if name not in required + ['copy_absolute_path.py'] or name in manifest:
            fail('Unexpected manifest entry')
        manifest[name] = digest
    for name in required + (['copy_absolute_path.py'] if (payload / 'copy_absolute_path.py').exists() else []):
        f = payload / name
        if f.is_symlink() or not f.is_file() or hashlib.sha256(f.read_bytes()).hexdigest() != manifest.get(name):
            fail(f'Invalid/missing payload: {name}')
    if (payload / 'ARCH').read_text().strip() != os.uname().machine:
        fail('Package architecture does not match this machine')
    for name in ('yihu', 'autodark-agent'):
        f = payload / name
        if not os.access(f, os.X_OK) or f.read_bytes()[:4] != b'\x7fELF':
            fail(f'Not an executable ELF: {name}')
        result = run('ldd', str(f), check=False)
        if result.returncode or 'not found' in result.stdout + result.stderr:
            fail(f'Unresolved runtime libraries for {name}: {result.stdout}{result.stderr}')
    # Check actual runtime libraries, not development pkg-config metadata.
    for lib, prefix, minimum in [('libgtk-4.so.1', 'gtk', (4, 12)), ('libadwaita-1.so.0', 'adw', (1, 4))]:
        dll = ctypes.CDLL(lib)
        version = tuple(getattr(dll, f'{prefix}_get_{part}_version')() for part in ('major', 'minor'))
        if version < minimum:
            fail(f'{lib} requires >= {minimum}, found {version}')
    home = P(os.environ['HOME'])
    def xdg(key, default):
        value = os.environ.get(key, '')
        return P(value) if value.startswith('/') else default
    data = xdg('XDG_DATA_HOME', home / '.local/share')
    config = xdg('XDG_CONFIG_HOME', home / '.config')
    # XDG has no library directory; use ~/.local/lib with the default data home.
    dest = home / '.local/lib/yihu' if data == home / '.local/share' else data / 'yihu/lib'
    safe(dest)
    if dest.exists():
        if not dest.is_dir() or {p.name for p in dest.iterdir()} != {'yihu', 'autodark-agent', '.yihu-owned'}:
            fail(f'Unexpected installation contents: {dest}')
        if (dest / '.yihu-owned').read_text() != 'yihu-package-v1\n':
            fail('Missing installation ownership marker')
        for f in dest.iterdir():
            owned_file(f)
    old_exes = {str(dest / 'yihu')}
    build_exe = (payload / 'BUILD_EXE').read_text().strip()
    if build_exe.endswith('/target/release/yihu') and P(build_exe).is_absolute():
        old_exes.add(build_exe)
    updates = {}
    def desktop(path, autostart=False):
        owned_file(path)
        if path.exists():
            text = path.read_text()
            if 'Name=一呼\n' not in text or 'Icon=tools.yihu.desktop\n' not in text:
                fail(f'Unrecognized desktop file: {path}')
            lines = re.findall(r'^Exec=(.*)$', text, re.M)
            if len(lines) != 1:
                fail(f'Unexpected Exec in {path}')
            old = lines[0].strip('"')
            if old not in old_exes and not (old.startswith('/') and old.endswith('/target/release/yihu')):
                fail(f'Unrecognized executable: {old}')
            old_exes.add(old)
            text = re.sub(r'^Exec=.*$', f'Exec="{dest}/yihu"', text, flags=re.M)
        elif autostart:
            return
        else:
            text = f'[Desktop Entry]\nType=Application\nName=一呼\nName[en]=Yihu\nExec="{dest}/yihu"\nIcon=tools.yihu.desktop\nTerminal=false\nCategories=Settings;Utility;\nStartupNotify=true\n'
        updates[path] = text.encode()
    desktop(data / 'applications/tools.yihu.desktop.desktop')
    # Migrate both old hardcoded and XDG autostarts, preserving all flags.
    for base in {config, home / '.config'}:
        desktop(base / 'autostart/yihu.desktop', True)
    for size in ('32x32', '128x128', '256x256', '512x512'):
        path = data / f'icons/hicolor/{size}/apps/tools.yihu.desktop.png'
        owned_file(path)
        if path.exists() and path.read_bytes() != (payload / f'icons/{size}.png').read_bytes():
            fail(f'Unknown existing icon (inspect manually): {path}')
        updates[path] = (payload / f'icons/{size}.png').read_bytes()
    # Presence is the application's enabled choice; never install a disabled extension.
    extension_updated = False
    for base in {data, home / '.local/share'}:
        ext = base / 'nautilus-python/extensions/copy_absolute_path.py'
        owned_file(ext)
        if ext.exists() and (payload / 'copy_absolute_path.py').exists():
            if ext.read_bytes() != (payload / 'copy_absolute_path.py').read_bytes():
                fail(f'Unknown/custom extension; preserve and inspect manually: {ext}')
            updates[ext] = (payload / 'copy_absolute_path.py').read_bytes()
            extension_updated = True
    units = {}
    unitdir = config / 'systemd/user'
    for name in ('yihu-autodark.timer', 'yihu-autodark.service'):
        path = unitdir / name
        owned_file(path)
        result = run('systemctl', '--user', 'show', name, '--property=LoadState,FragmentPath,ActiveState,UnitFileState', check=False)
        if result.returncode:
            fail('Cannot inspect user systemd manager; run in a user login session')
        state = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
        if state.get('LoadState') == 'not-found' and not path.exists():
            continue
        if state.get('LoadState') != 'loaded' or state.get('FragmentPath') != str(path) or not path.exists():
            fail(f'Unknown/masked/overridden unit: {name}')
        if (unitdir / (name + '.d')).exists():
            fail(f'Unit drop-ins require manual inspection: {name}')
        # Effective drop-ins from other systemd search directories are also unsafe.
        if run('systemctl', '--user', 'show', name, '--property=DropInPaths', '--value').stdout.strip():
            fail(f'Unit has drop-ins: {name}')
        if state.get('ActiveState') not in ('active', 'inactive') or state.get('UnitFileState') not in ('enabled', 'disabled', 'static'):
            fail(f'Unsupported/transitional unit state: {state}')
        text = path.read_text()
        if name.endswith('.service'):
            matches = re.findall(r'^ExecStart=(.*) apply$', text, re.M)
            expected = {str(P(e).with_name('autodark-agent')) for e in old_exes}
            if len(matches) != 1 or matches[0].strip('"') not in expected or 'Description=一呼 AutoDark 应用主题切换\n' not in text:
                fail(f'Unknown service: {path}')
            # Only accept the generated service, not arbitrary additional directives.
            canonical = f'[Unit]\nDescription=一呼 AutoDark 应用主题切换\n\n[Service]\nType=oneshot\nExecStart={matches[0]} apply\n'
            if text != canonical:
                fail(f'Customized service requires manual inspection: {path}')
            updates[path] = canonical.replace(f'ExecStart={matches[0]}', f'ExecStart="{dest}/autodark-agent"').encode()
        else:
            expected = '[Unit]\nDescription=一呼 AutoDark 定时核对\n\n[Timer]\nOnCalendar=*-*-* *:*:00\nAccuracySec=15s\nPersistent=true\nUnit=yihu-autodark.service\n\n[Install]\nWantedBy=timers.target\n'
            if text != expected:
                fail(f'Customized timer requires manual inspection: {path}')
        units[name] = state
    if 'yihu-autodark.timer' in units and 'yihu-autodark.service' not in units:
        fail('Timer exists without its owned service')
    def processes():
        result = run('pgrep', '-u', str(os.getuid()), '-x', 'yihu|autodark-agent', check=False)
        if result.returncode not in (0, 1):
            fail('Process lookup failed')
        verified = []
        allowed = old_exes | {str(P(e).with_name('autodark-agent')) for e in old_exes}
        for pid in result.stdout.split():
            proc = P('/proc') / str(int(pid))
            try:
                exe = os.readlink(proc / 'exe')
                if proc.stat().st_uid == os.getuid() and exe in allowed:
                    if P(exe).is_file() and P(exe).stat().st_uid == os.getuid():
                        verified.append((int(pid), exe))
            except FileNotFoundError:
                pass
        return verified
    found = processes()
    if found and (not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal')):
        fail('Safe process termination requires Python/Linux pidfd support')
    print(f'Validated package. Install directory: {dest}')
    print('Verified processes to terminate:', found)
    print('Preserved unit states:', units)
    for path in updates:
        print('Write:', path)
    if args.dry_run:
        print('Dry run complete: no files, processes, services or caches changed.')
        return
    # Back up only files being replaced; configuration and unrelated files are untouched.
    backup = P(tempfile.mkdtemp(prefix='yihu-rollback-'))
    originals = {}
    created_dirs = []
    def mkdir(path):
        if not path.exists():
            mkdir(path.parent)
            path.mkdir(mode=0o755)
            created_dirs.append(path)
    def write(path, content, mode=0o644):
        owned_file(path)
        if path not in originals:
            originals[path] = (path.read_bytes(), path.stat().st_mode & 0o777) if path.exists() else None
            if originals[path] is not None:
                (backup / str(len(originals))).write_bytes(originals[path][0])
            with (backup / 'INDEX').open('a') as index:
                index.write(f'{len(originals)}\t{path}\t{originals[path][1] if originals[path] else "new"}\n')
        mkdir(path.parent)
        fd, tmp = tempfile.mkstemp(prefix='.yihu-', dir=path.parent)
        try:
            with os.fdopen(fd, 'wb') as out:
                out.write(content)
            os.chmod(tmp, mode)
            os.replace(tmp, path)
        finally:
            if os.path.exists(tmp):
                os.unlink(tmp)
    def restore_active():
        # Never enable/disable: existing enablement symlinks remain untouched.
        for name in reversed(list(units)):
            if units[name]['ActiveState'] == 'active':
                run('systemctl', '--user', 'start', name)
    try:
        if 'yihu-autodark.timer' in units:
            run('systemctl', '--user', 'stop', 'yihu-autodark.timer')
        # The generated oneshot agent exits on its own. Do not use service stop:
        # systemd could escalate that to SIGKILL after TimeoutStopSec.
        if 'yihu-autodark.service' in units:
            deadline = time.monotonic() + 15
            while run('systemctl', '--user', 'show', 'yihu-autodark.service', '--property=ActiveState', '--value').stdout.strip() != 'inactive':
                if time.monotonic() >= deadline:
                    fail('Agent service did not finish; refusing forced stop')
                time.sleep(0.2)
        for pid, exe in found:
            try:
                fd = os.pidfd_open(pid)
                try:
                    if os.readlink(f'/proc/{pid}/exe') == exe and P(f'/proc/{pid}').stat().st_uid == os.getuid():
                        signal.pidfd_send_signal(fd, signal.SIGTERM)
                finally:
                    os.close(fd)
            except ProcessLookupError:
                pass
            except FileNotFoundError:
                pass
        deadline = time.monotonic() + 15
        while processes():
            if time.monotonic() >= deadline:
                fail('App did not exit within 15 seconds; no force kill performed')
            time.sleep(0.2)
        write(dest / 'yihu', (payload / 'yihu').read_bytes(), 0o755)
        write(dest / 'autodark-agent', (payload / 'autodark-agent').read_bytes(), 0o755)
        write(dest / '.yihu-owned', b'yihu-package-v1\n')
        for path, content in updates.items():
            write(path, content)
        if units:
            run('systemctl', '--user', 'daemon-reload')
            restore_active()
    except BaseException:
        try:
            if 'yihu-autodark.timer' in units:
                run('systemctl', '--user', 'stop', 'yihu-autodark.timer')
            if 'yihu-autodark.service' in units:
                deadline = time.monotonic() + 15
                while run('systemctl', '--user', 'show', 'yihu-autodark.service', '--property=ActiveState', '--value').stdout.strip() != 'inactive':
                    if time.monotonic() >= deadline:
                        fail('Agent still running; refusing unsafe file rollback')
                    time.sleep(0.2)
            for path, original in reversed(list(originals.items())):
                if original is None:
                    path.unlink(missing_ok=True)
                else:
                    path.write_bytes(original[0])
                    path.chmod(original[1])
            for path in reversed(created_dirs):
                path.rmdir()
            if units:
                run('systemctl', '--user', 'daemon-reload')
                restore_active()
        except Exception as error:
            print(f'Rollback incomplete: {error}; backup retained at {backup}', file=sys.stderr)
            raise
        shutil.rmtree(backup)
        raise
    shutil.rmtree(backup)
    for cmd, path in [('update-desktop-database', data / 'applications'), ('gtk-update-icon-cache', data / 'icons/hicolor')]:
        if shutil.which(cmd):
            run(cmd, str(path), check=False)
    if args.restart_nautilus and extension_updated and shutil.which('nautilus'):
        run('nautilus', '-q', check=False)
    print('Installed. Launch 一呼 from Applications. App is not automatically restarted.')
    print('Configuration, third-party settings, enablement and extension choice preserved.')

try:
    main()
except Exception as error:
    print(f'Reinstall aborted: {error}', file=sys.stderr)
    sys.exit(1)
PY

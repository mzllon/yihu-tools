#!/usr/bin/env python3
"""Isolated installer integration tests; never invoke real service/process tools."""
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

os.umask(0o022)
archive = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='yihu-test-') as tmp:
    root = Path(tmp)
    with tarfile.open(archive) as tar:
        tar.extractall(root, filter='data')
    package = next(root.glob('yihu-*'))
    home = root / 'home with spaces'
    home.mkdir()
    stubs = root / 'bin'
    stubs.mkdir()
    log = root / 'calls'
    stub = '''#!/usr/bin/python3
import os,sys
from pathlib import Path
name=Path(sys.argv[0]).name
args=sys.argv[1:]
if name == 'pgrep': sys.exit(1)
if name == 'systemctl':
    if 'show' in args:
        unit=args[2]
        p=Path(os.environ['XDG_CONFIG_HOME'])/'systemd/user'/unit
        if '--value' in args:
            print('inactive' if '--property=ActiveState' in args else '')
        elif p.exists():
            print(f'LoadState=loaded\\nFragmentPath={p}\\nActiveState={"active" if unit.endswith("timer") else "inactive"}\\nUnitFileState={"enabled" if unit.endswith("timer") else "static"}')
        else: print('LoadState=not-found')
        sys.exit(0)
    with open(os.environ['TEST_LOG'],'a') as f: f.write(' '.join(args)+'\\n')
    if 'daemon-reload' in args and os.environ.get('FAIL_RELOAD') and not Path(os.environ['TEST_LOG']+'.failed').exists():
        Path(os.environ['TEST_LOG']+'.failed').touch(); sys.exit(1)
else:
    with open(os.environ['TEST_LOG'],'a') as f: f.write(name+' '+' '.join(args)+'\\n')
'''
    for name in ['pgrep', 'systemctl', 'nautilus', 'gtk-update-icon-cache', 'update-desktop-database']:
        p = stubs / name
        p.write_text(stub)
        p.chmod(0o755)
    env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / 'data'), XDG_CONFIG_HOME=str(home / 'config'), XDG_CACHE_HOME=str(home / 'cache'), XDG_STATE_HOME=str(home / 'state'), XDG_RUNTIME_DIR=str(home / 'runtime'), PATH=f'{stubs}:/usr/bin:/bin', TEST_LOG=str(log))
    def snapshot():
        return {str(p.relative_to(home)): (hashlib.sha256(p.read_bytes()).hexdigest(), p.stat().st_mode) if p.is_file() else 'directory' for p in home.rglob('*')}
    def install(*args, success=True, extra=None):
        result = subprocess.run([str(package / 'reinstall.sh'), *args], env=env | (extra or {}), text=True, capture_output=True)
        assert (result.returncode == 0) == success, result.stdout + result.stderr
        return result
    before = snapshot()
    install('--dry-run')
    assert snapshot() == before and not log.exists()
    install()
    dest = home / 'data/yihu/lib'
    assert (dest / 'yihu').read_bytes() == (package / 'yihu').read_bytes()
    assert (dest / 'yihu-panel').read_bytes() == (package / 'yihu-panel').read_bytes()
    assert not (home / 'data/nautilus-python/extensions/copy_absolute_path.py').exists()
    # Upgrade from a pre-panel install: the old 3-entry directory is accepted
    # and yihu-panel is added; idempotence after that.
    (dest / 'yihu-panel').unlink()
    install()
    assert (dest / 'yihu-panel').read_bytes() == (package / 'yihu-panel').read_bytes()
    before = snapshot()
    install('--dry-run')
    assert snapshot() == before
    install()
    assert snapshot() == before
    # Existing checkout launcher, autostart, enabled timer and extension.
    old = '/old checkout/target/release/yihu'
    launcher = home / 'data/applications/tools.yihu.desktop.desktop'
    launcher.write_text(f'[Desktop Entry]\nType=Application\nName=一呼\nIcon=tools.yihu.desktop\nExec={old}\n')
    auto = home / '.config/autostart/yihu.desktop'
    auto.parent.mkdir(parents=True)
    auto.write_text(launcher.read_text() + 'Hidden=true\nX-GNOME-Autostart-enabled=false\n')
    pauto = home / 'config/autostart/yihu-panel.desktop'
    pauto.parent.mkdir(parents=True)
    pauto.write_text(f'[Desktop Entry]\nType=Application\nName=一呼面板\nIcon=tools.yihu.desktop\nExec={Path(old).with_name("yihu-panel")}\n')
    units = home / 'config/systemd/user'
    units.mkdir(parents=True)
    service = units / 'yihu-autodark.service'
    service.write_text(f'[Unit]\nDescription=一呼 AutoDark 应用主题切换\n\n[Service]\nType=oneshot\nExecStart={Path(old).with_name("autodark-agent")} apply\n')
    (units / 'yihu-autodark.timer').write_text('[Unit]\nDescription=一呼 AutoDark 定时核对\n\n[Timer]\nOnCalendar=*-*-* *:*:00\nAccuracySec=15s\nPersistent=true\nUnit=yihu-autodark.service\n\n[Install]\nWantedBy=timers.target\n')
    ext = home / '.local/share/nautilus-python/extensions/copy_absolute_path.py'
    ext.parent.mkdir(parents=True)
    ext.write_bytes((package / 'copy_absolute_path.py').read_bytes())
    config = home / 'config/yihu/autodark.conf'
    config.parent.mkdir()
    config.write_text('preserve this feature configuration')
    third = home / 'config/Code/User/settings.json'
    third.parent.mkdir(parents=True)
    third.write_text('{"untouched": true}')
    before = snapshot()
    log.write_text('')
    install('--dry-run')
    assert snapshot() == before and log.read_text() == ''
    install(extra={'FAIL_RELOAD': '1'}, success=False)
    assert snapshot() == before, 'rollback must restore original bytes and modes'
    install()
    assert f'ExecStart="{dest}/autodark-agent" apply' in service.read_text()
    assert f'Exec="{dest}/yihu"' in auto.read_text() and 'Hidden=true' in auto.read_text()
    assert f'Exec="{dest}/yihu-panel"' in pauto.read_text()
    assert config.read_text() == 'preserve this feature configuration'
    assert third.read_text() == '{"untouched": true}'
    assert '--user stop yihu-autodark.timer' in log.read_text()
    assert '--user start yihu-autodark.timer' in log.read_text()
    assert 'nautilus' not in log.read_text() and ' enable ' not in log.read_text()
    # Unknown files, links and tampered payload fail before actions.
    (dest / 'unexpected').write_text('keep')
    before = snapshot()
    log.write_text('')
    install(success=False)
    assert snapshot() == before and log.read_text() == ''
    (dest / 'unexpected').unlink()
    launcher.unlink()
    launcher.symlink_to(third)
    install(success=False)
    assert third.read_text() == '{"untouched": true}' and log.read_text() == ''
    launcher.unlink()
    (package / 'icons/32x32.png').write_bytes(b'bad')
    install('--dry-run', success=False)
    assert log.read_text() == ''
    print('PASS: isolated fresh install, idempotence, non-mutating dry run, legacy migration, feature/config preservation, service restoration, rollback, unknown files, symlinks, payload corruption; no live commands used.')

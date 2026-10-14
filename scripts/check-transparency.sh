#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
export GISLAND_ASSET_ROOT="$PWD/assets"
binary="${1:-$PWD/target/debug/gisland}"
exec xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- python3 - "$binary" <<'PY'
import ctypes as c
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from PIL import ImageGrab

# A real opaque window gives the compositor a known background behind the overlay.
x = c.CDLL('libX11.so.6')
x.XOpenDisplay.restype = c.c_void_p
x.XDefaultRootWindow.argtypes = [c.c_void_p]
x.XDefaultRootWindow.restype = c.c_ulong
x.XCreateSimpleWindow.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_int,
    c.c_uint, c.c_uint, c.c_uint, c.c_ulong, c.c_ulong]
x.XCreateSimpleWindow.restype = c.c_ulong
x.XMapWindow.argtypes = [c.c_void_p, c.c_ulong]
x.XSync.argtypes = [c.c_void_p, c.c_int]
x.XCloseDisplay.argtypes = [c.c_void_p]
display = x.XOpenDisplay(None)
assert display, 'Cannot open test display'
background = x.XCreateSimpleWindow(display, x.XDefaultRootWindow(display),
    0, 0, 1280, 800, 0, 0, 0x335577)
x.XMapWindow(display, background)
x.XSync(display, 0)
children = []
try:
    with tempfile.TemporaryDirectory(prefix='gisland-transparency-') as tmp:
        env = dict(os.environ)
        for variable in ['XDG_RUNTIME_DIR', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME']:
            folder = Path(tmp) / variable
            folder.mkdir(mode=0o700)
            env[variable] = str(folder)
        with open(Path(tmp) / 'process.log', 'w+') as log:
            compositor = subprocess.Popen(['picom', '--config', '/dev/null', '--backend', 'xrender'],
                env=env, stdout=log, stderr=log)
            children.append(compositor)
            time.sleep(0.2)
            assert compositor.poll() is None, 'Test compositor failed to start'
            app = subprocess.Popen([sys.argv[1]], env=env, stdout=log, stderr=log)
            children.append(app)
            def ctl(*args):
                return subprocess.run([sys.argv[1], 'ctl', *args], env=env,
                    capture_output=True, text=True, timeout=12)
            for _ in range(60):
                result = ctl('status', '--json')
                if result.returncode == 0:
                    break
                assert app.poll() is None, 'Application exited during startup'
                time.sleep(0.1)
            assert result.returncode == 0, result.stderr
            assert json.loads(result.stdout)['pid'] == app.pid
            for command in [('activate', 'clock'), ('activate-open', 'clock'), ('close',)]:
                result = ctl(*command)
                assert result.returncode == 0, result.stderr
                time.sleep(0.7)
                screen = ImageGrab.grab()
                assert screen.getpixel((10, 500)) == (51, 85, 119), 'Missing test background'
                # Both samples are inside the native window but outside the island.
                for point in [(410, 400), (640, 550)]:
                    assert screen.getpixel(point) == (51, 85, 119), (
                        command, point, 'Overlay obscures the desktop', screen.getpixel(point))
            print('Composited transparency passed: compact, expanded, collapsed.')
finally:
    for child in reversed(children):
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
    x.XCloseDisplay(display)
PY

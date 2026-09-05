import os
import shutil
import subprocess
import sys
import time

src = sys.argv[1]
dst = sys.argv[2]
initial_mtime_ms = int(sys.argv[3])
marker = os.path.normcase(os.path.dirname(src))


def mtime_ms(path):
    try:
        return int(os.path.getmtime(path) * 1000)
    except OSError:
        return 0


def try_copy():
    if not os.path.isfile(src):
        return False
    dst_dir = os.path.dirname(dst)
    try:
        os.makedirs(dst_dir, exist_ok=True)
    except OSError:
        return False
    copied_exe = False
    for _ in range(20):
        try:
            shutil.copy2(src, dst)
            copied_exe = True
            break
        except OSError:
            time.sleep(0.25)
    if not copied_exe:
        return False
    src_dir = os.path.dirname(src)
    try:
        names = os.listdir(src_dir)
    except OSError:
        return True
    for name in names:
        if not name.lower().endswith(".dll"):
            continue
        s = os.path.join(src_dir, name)
        if not os.path.isfile(s):
            continue
        d = os.path.join(dst_dir, name)
        for _ in range(20):
            try:
                shutil.copy2(s, d)
                break
            except OSError:
                time.sleep(0.25)
    return True


def our_compiler_running():
    try:
        r = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | "
                "Where-Object { $_.Name -match '^(rustc|link|lld-link)\\.exe$' } | "
                "Select-Object -ExpandProperty CommandLine",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            creationflags=0x08000000,
        )
    except OSError:
        return False
    mark = marker.replace("/", "\\")
    for line in r.stdout.splitlines():
        if mark in os.path.normcase(line.replace("/", "\\")):
            return True
    return False


start = time.time()
quiet_since = None
deadline = start + 7200
while time.time() < deadline:
    current = mtime_ms(src)
    if current > initial_mtime_ms:
        time.sleep(0.4)
        if mtime_ms(src) == current:
            if try_copy():
                sys.exit(0)
    if our_compiler_running():
        quiet_since = None
    else:
        if quiet_since is None:
            quiet_since = time.time()
        elif time.time() - quiet_since >= 2 and time.time() - start >= 4:
            try_copy()
            sys.exit(0)
    time.sleep(0.4)

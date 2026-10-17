#!/usr/bin/env python3
"""Linux X11 native-window/keyboard capture; isolated home, no CLI/network calls.
Requires an already built desktop binary, Xvfb, ImageMagick and X11/XTest libraries.
Not a hardware-GPU or cross-platform/accessibility certification.
"""
import argparse
import ctypes as c
import ctypes.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def x11():
    x = c.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
    xt = c.CDLL(ctypes.util.find_library("Xtst") or "libXtst.so.6")
    pointer, window = c.c_void_p, c.c_ulong
    signatures = {
        "XOpenDisplay": ([c.c_char_p], pointer),
        "XDefaultRootWindow": ([pointer], window),
        "XQueryTree": ([pointer, window, c.POINTER(window), c.POINTER(window), c.POINTER(c.POINTER(window)), c.POINTER(c.c_uint)], c.c_int),
        "XFetchName": ([pointer, window, c.POINTER(pointer)], c.c_int),
        "XFree": ([pointer], c.c_int),
        "XKeysymToKeycode": ([pointer, c.c_ulong], c.c_ubyte),
        "XSetInputFocus": ([pointer, window, c.c_int, c.c_ulong], c.c_int),
        "XSync": ([pointer, c.c_int], c.c_int),
        "XCloseDisplay": ([pointer], c.c_int),
    }
    for name, (args, result) in signatures.items():
        function = getattr(x, name)
        function.argtypes, function.restype = args, result
    xt.XTestFakeKeyEvent.argtypes = [pointer, c.c_uint, c.c_int, c.c_ulong]
    xt.XTestFakeKeyEvent.restype = c.c_int
    display = x.XOpenDisplay(os.environ["DISPLAY"].encode())
    if not display:
        raise RuntimeError("isolated X display unavailable")
    return x, xt, display


def find_window(x, display):
    root = x.XDefaultRootWindow(display)
    returned_root, parent, count = c.c_ulong(), c.c_ulong(), c.c_uint()
    children = c.POINTER(c.c_ulong)()
    if not x.XQueryTree(display, root, c.byref(returned_root), c.byref(parent), c.byref(children), c.byref(count)):
        return None
    try:
        for index in range(count.value):
            name = c.c_void_p()
            if x.XFetchName(display, children[index], c.byref(name)) and name.value:
                try:
                    if b"Connection Desk" in c.string_at(name):
                        return children[index]
                finally:
                    x.XFree(name)
            # GTK4 may publish only the UTF-8 EWMH property, not legacy WM_NAME.
            title = subprocess.run(["xprop", "-id", str(children[index]), "_NET_WM_NAME"], capture_output=True, timeout=1, check=False)
            if title.returncode == 0 and b"Connection Desk" in title.stdout:
                return children[index]
    finally:
        if children:
            x.XFree(children)
    return None


def capture(binary, output):
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix="eavs-native-window-") as temporary:
        # Native process never inherits credentials or production config references.
        environment = {key: os.environ[key] for key in ("PATH", "DISPLAY", "XAUTHORITY", "__EGL_VENDOR_LIBRARY_FILENAMES", "LIBGL_ALWAYS_SOFTWARE") if key in os.environ}
        environment.update(HOME=temporary, XDG_CONFIG_HOME=temporary, XDG_DATA_HOME=temporary, XDG_CACHE_HOME=temporary, MYGO_ENV="production", LANG="C.UTF-8")
        with (output / "native-window.log").open("wb") as log:
            process = subprocess.Popen([str(binary), "--eavs-binary", "/nonexistent/eavs-window-proof-must-not-execute"], env=environment, stdout=log, stderr=log)
            x = display = None
            try:
                x, xt, display = x11()
                deadline = time.monotonic() + 8
                window = None
                while window is None and time.monotonic() < deadline and process.poll() is None:
                    window = find_window(x, display)
                    time.sleep(0.1)
                if window is None:
                    raise RuntimeError("native window did not become available; inspect private proof log")
                time.sleep(0.5)
                subprocess.run(["import", "-window", str(window), str(output / "connections.png")], check=True, timeout=5)
                # Keyboard access opens the first enabled control: Connect device.
                x.XSetInputFocus(display, window, 2, 0)
                for symbol in (0xFF09, 0xFF0D):  # Tab, Return
                    code = x.XKeysymToKeycode(display, symbol)
                    if not code:
                        raise RuntimeError("test key unavailable")
                    for pressed in (1, 0):
                        if not xt.XTestFakeKeyEvent(display, code, pressed, 0):
                            raise RuntimeError("native keyboard injection rejected")
                    x.XSync(display, 0)
                    time.sleep(0.1)
                time.sleep(0.4)
                subprocess.run(["import", "-window", str(window), str(output / "enrollment.png")], check=True, timeout=5)
                if process.poll() is not None:
                    raise RuntimeError("native window exited during proof")
            finally:
                if x and display:
                    x.XCloseDisplay(display)
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
    print("Native X11 window + keyboard captures written; no inference/CLI calls. Inspect enrollment.png; software rendering is not GPU certification.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    binary, output = args.binary.resolve(strict=True), args.output.resolve()
    if args.inside:
        capture(binary, output)
    else:
        environment = os.environ.copy()
        mesa = Path("/usr/share/glvnd/egl_vendor.d/50_mesa.json")
        if mesa.is_file():
            environment.update(__EGL_VENDOR_LIBRARY_FILENAMES=str(mesa), LIBGL_ALWAYS_SOFTWARE="1")
        subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1280x960x24", sys.executable, str(Path(__file__).resolve()), "--inside", "--binary", str(binary), "--output", str(output)], env=environment, check=True, timeout=25)


if __name__ == "__main__":
    main()

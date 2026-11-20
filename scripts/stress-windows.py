"""Exercise a real Windows reader window and report process memory in MiB."""

import argparse
import contextlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time

from PIL import ImageGrab
from pywinauto import Application, keyboard, mouse
import win32api
import win32con
import win32file
import win32gui
import win32process


def wait_for(label, read, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = read()
        if result:
            return result
        time.sleep(0.04)
    raise AssertionError(f"Timed out waiting for {label}")


@contextlib.contextmanager
def launch(executable, documents, theme, extra_env=None):
    with tempfile.TemporaryDirectory(prefix="tinypdf-stress-") as profile:
        settings = Path(profile) / "session.json"
        settings.write_text(f'{{"version":1,"dark":{str(theme == "dark").lower()}}}', encoding="utf-8")
        env = {**os.environ, "TINYPDF_DATA_DIR": profile, **(extra_env or {})}
        process = subprocess.Popen([executable, *documents], env=env)
        try:
            app = Application(backend="uia").connect(process=process.pid, timeout=15)
            window = app.window(title="TinyPDF")
            window.wait("visible", timeout=15)
            handle = window.handle
            work = win32api.GetMonitorInfo(win32api.MonitorFromWindow(handle))["Work"]
            x, y, right, bottom = work
            win32gui.MoveWindow(handle, x + 16, y + 16, min(1024, right - x - 32),
                                min(720, bottom - y - 32), True)
            window.set_focus()
            wait_for("foreground TinyPDF", lambda: win32gui.GetForegroundWindow() == handle)
            win32api.SetCursorPos(viewport_center(handle))
            yield window, process
        finally:
            if process.poll() is None:
                with contextlib.suppress(Exception):
                    win32gui.PostMessage(window.handle, win32con.WM_CLOSE, 0, 0)
                    process.wait(timeout=4)
            if process.poll() is None:
                process.kill()
                process.wait(timeout=4)


def measure(process):
    handle = win32api.OpenProcess(win32con.PROCESS_QUERY_INFORMATION | win32con.PROCESS_VM_READ,
                                 False, process.pid)
    try:
        info = win32process.GetProcessMemoryInfo(handle)
        return {key: info[key] / (1024 * 1024) for key in
                ("WorkingSetSize", "PeakWorkingSetSize", "PagefileUsage", "PeakPagefileUsage")}
    finally:
        handle.Close()


def cpu_seconds(process):
    handle = win32api.OpenProcess(win32con.PROCESS_QUERY_INFORMATION, False, process.pid)
    try:
        times = win32process.GetProcessTimes(handle)
        return (times["UserTime"] + times["KernelTime"]) / 10_000_000
    finally:
        handle.Close()


def page_number(window):
    return window.child_window(title="Go to page", control_type="Edit").get_value()


def paper(window, number):
    page = next((item for item in window.descendants(control_type="Image")
                 if item.window_text().startswith(f"Page {number} of ")), None)
    if page is None:
        return None
    box = page.rectangle()
    frame = window.rectangle()
    left, top = max(box.left + 8, frame.left + 12), max(box.top + 8, frame.top + 116)
    right, bottom = min(box.right - 8, frame.right - 16), min(box.bottom - 8, frame.bottom - 50)
    if left >= right or top >= bottom:
        return None
    return ImageGrab.grab((left, top, right, bottom), all_screens=True).convert("L")


def rendered_paper(window, number):
    image = paper(window, number)
    if image is not None:
        histogram = image.histogram()
        low, high = image.getextrema()
        if high - low > 30 and sum(histogram[:high - 24]) > 10:
            return image
    return None


def viewport_center(handle):
    _, _, width, height = win32gui.GetClientRect(handle)
    return win32gui.ClientToScreen(handle, (width // 2, height // 2))


def wheel(delta):
    win32api.mouse_event(win32con.MOUSEEVENTF_WHEEL, 0, 0, delta, 0)


def report(label, values):
    print(label, " ".join(f"{key}={value:.2f}" for key, value in values.items()), flush=True)


def invoke(window, name):
    window.child_window(title=name, control_type="Button").wait("exists enabled", timeout=10)
    window.child_window(title=name, control_type="Button").wrapper_object().invoke()


def text_exists(window, name):
    return window.child_window(title=name, control_type="Text").exists(timeout=0)


def check_recovery(executable, fixture):
    with tempfile.TemporaryDirectory(prefix="tinypdf-recovery-") as folder:
        scratch = Path(folder)
        broken = scratch / "recoverable.pdf"
        broken.write_bytes(b"not a PDF")
        with launch(executable, [broken], "dark") as (window, _):
            wait_for("corrupt document error", lambda: text_exists(window, "Could not open PDF"))
            shutil.copyfile(fixture, broken)
            invoke(window, "Try again")
            wait_for("recovered PDF", lambda: rendered_paper(window, 1))
            invoke(window, "Next page")
            wait_for("next page", lambda: page_number(window) == "2")
            invoke(window, "Previous page")
            wait_for("previous page", lambda: page_number(window) == "1")
            check_save_with_reader_lock(window, broken)
        print("PASS corrupt PDF recovery and reader navigation", flush=True)

        dll = scratch / "pdfium.dll"
        with launch(executable, [fixture], "dark", {"PDFIUM_LIBRARY_PATH": str(dll)}) as (window, _):
            wait_for("missing PDFium error", lambda: text_exists(window, "Could not open PDF"))
            shutil.copyfile(executable.with_name("pdfium.dll"), dll)
            invoke(window, "Try again")
            wait_for("PDFium retry succeeded", lambda: rendered_paper(window, 1))
        print("PASS missing PDFium recovery without restarting", flush=True)

        protected = Path(__file__).resolve().parent.parent / "tests/fixtures/protected-graphics.pdf"
        with launch(executable, [protected], "dark") as (window, _):
            wait_for("password dialog", lambda: text_exists(window, "Password required"))
            keyboard.send_keys("incorrect{ENTER}", vk_packet=False)
            wait_for("password rejection", lambda: text_exists(window, "Incorrect password. Try again."))
            keyboard.send_keys("classroom{ENTER}", vk_packet=False)
            wait_for("correct password opens PDF", lambda: rendered_paper(window, 1))
        print("PASS PDF password rejection and retry", flush=True)

        check_pdf_links(executable, fixture, scratch)


def check_save_with_reader_lock(window, path):
    invoke(window, "Toggle annotation tools")
    invoke(window, "Note")
    page = next(item for item in window.descendants(control_type="Image")
                if item.window_text().startswith("Page 1 of "))
    box = page.rectangle()
    mouse.click(coords=((box.left + box.right) // 2, (box.top + box.bottom) // 2))
    window.child_window(title="Annotation text", control_type="Edit").wait("visible", timeout=8)
    keyboard.send_keys("TinyPDF save regression{ENTER}", vk_packet=False)
    wait_for("unsaved note", lambda: window.child_window(
        title=f"{path.name}, unsaved changes", control_type="Button").exists(timeout=0))
    original = path.read_bytes()
    lock = win32file.CreateFile(str(path), win32con.GENERIC_READ,
                               win32con.FILE_SHARE_READ | win32con.FILE_SHARE_WRITE, None,
                               win32con.OPEN_EXISTING, win32con.FILE_ATTRIBUTE_NORMAL, None)
    try:
        keyboard.send_keys("^s", vk_packet=False)
        wait_for("save started", lambda: text_exists(window, "Saving document…"), 5)
        time.sleep(0.10)
    finally:
        lock.Close()
    wait_for("save after reader releases the file", lambda: text_exists(window, "Document saved."))
    assert path.read_bytes() != original, "saved PDF bytes were unchanged"
    print("PASS annotation save while another reader briefly holds the file", flush=True)


def check_pdf_links(executable, fixture, scratch):
    pdf = fixture.read_bytes()

    class FixtureServer(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path == "/redirect":
                self.send_response(302)
                self.send_header("Location", "/reading.pdf")
                self.end_headers()
                return
            body = pdf if self.path == "/reading.pdf" else b"<html>Sign in</html>"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    root = scratch / "downloads"
    root.mkdir()
    with ThreadingHTTPServer(("127.0.0.1", 0), FixtureServer) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            env = {"TEMP": str(root), "TMP": str(root), "NO_PROXY": "127.0.0.1"}
            address = f"http://127.0.0.1:{server.server_port}"
            with launch(executable, [], "dark", env) as (window, _):
                invoke(window, "Open PDF link")
                wait_for("link entry", lambda: text_exists(window, "Open PDF link"))
                keyboard.send_keys("file:///bad.pdf{ENTER}", vk_packet=False)
                wait_for("invalid link rejected", lambda: text_exists(
                    window, "Enter a complete link beginning with https:// or http://."))
                keyboard.send_keys(f"^a{address}/redirect{{ENTER}}", vk_packet=False)
                wait_for("redirected PDF rendered", lambda: rendered_paper(window, 1))
                assert list(root.glob("TinyPDF-link-*")), "no temporary download was created"
                keyboard.send_keys("^l", vk_packet=False)
                wait_for("link dialog via shortcut", lambda: text_exists(window, "Open PDF link"))
                keyboard.send_keys(f"{address}/not-pdf{{ENTER}}", vk_packet=False)
                wait_for("HTML response rejected", lambda: text_exists(window, "Could not open PDF link"))
                keyboard.send_keys("{ESC}")
                wait_for("original PDF remains", lambda: rendered_paper(window, 1))
                invoke(window, "Close Web - reading.pdf")
                wait_for("download cleared after closing tab", lambda: not list(root.glob("TinyPDF-link-*")))
            print("PASS link validation, HTTP redirect, rendered web PDF, HTML rejection, and download cleanup", flush=True)
        finally:
            server.shutdown()
            thread.join(timeout=4)


def run(args):
    with launch(args.exe, [], args.theme) as (_, process):
        time.sleep(1)
        report("empty", measure(process))
    with launch(args.exe, args.pdf, args.theme) as (window, process):
        handle = window.handle
        wait_for("first page selected", lambda: page_number(window) == "1")
        reference = wait_for("first page rendered", lambda: rendered_paper(window, 1))
        time.sleep(1)
        report("reading", measure(process))
        start_cpu = cpu_seconds(process)
        start = time.monotonic()
        updates = 0
        extremes = []
        for direction in [-1, 1] * args.cycles:
            assert win32gui.GetForegroundWindow() == handle, "TinyPDF lost foreground during scroll"
            for _ in range(args.burst):
                wheel(direction * 12_000)
                updates += 1
                time.sleep(0.01)
            if direction < 0:
                extremes.append(int(wait_for("scroll away from page one", lambda: (
                    number if (number := page_number(window)) != "1" else None
                ))))
        elapsed = time.monotonic() - start
        cpu = cpu_seconds(process) - start_cpu
        window.set_focus()
        keyboard.send_keys("{HOME}")
        wait_for("return to page one", lambda: page_number(window) == "1")
        start_recovery = time.monotonic()
        def recovered():
            image = paper(window, 1)
            return image is not None and image.size == reference.size and image.tobytes() == reference.tobytes()

        wait_for("pixel-identical return to page one", recovered)
        recovery = (time.monotonic() - start_recovery) * 1000
        report("after_stress", measure(process))
        print(f"PASS {updates} wheel events in {elapsed:.2f}s ({cpu:.2f}s reader CPU), "
              f"{args.cycles * 2 - 1} direction reversals, farthest page {max(extremes)}, "
              f"reference restored within {recovery:.1f} ms of observing page one", flush=True)
        if args.screenshot:
            frame = window.rectangle()
            ImageGrab.grab((frame.left, frame.top, frame.right, frame.bottom),
                           all_screens=True).save(args.screenshot)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=lambda name: Path(name).resolve(strict=True))
    parser.add_argument("pdf", nargs="+", type=lambda name: Path(name).resolve(strict=True))
    parser.add_argument("--cycles", type=int, choices=range(1, 501), default=8)
    parser.add_argument("--burst", type=int, choices=range(1, 101), default=25)
    parser.add_argument("--theme", choices=("light", "dark"), default="light")
    parser.add_argument("--screenshot", type=Path)
    parser.add_argument("--recovery-only", action="store_true")
    args = parser.parse_args()
    if args.recovery_only:
        check_recovery(args.exe, args.pdf[0])
    else:
        run(args)

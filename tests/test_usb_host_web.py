"""Playwright test: upload a disk image through the web UI, verify the firmware mounts it.

Serves web/ with Python's HTTP server, opens the page in headless Chromium,
uploads a filebrowser firmware and a FAT32 disk image, then checks that
the firmware boots and the disk picker accepted the file.

Requires: playwright (python), chromium browser, a built fm1_web.wasm in web/,
and a filebrowser firmware .fwsc.

Run:
  FM1_FWSC=.../modd_dev_filebrowser/modd.fwsc python -m pytest tests/test_usb_host_web.py -v
"""

import http.server
import os
import shutil
import subprocess
import tempfile
import threading
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
WEB_DIR = ROOT / "web"
GENERATOR = ROOT / "tools" / "usb-host" / "create_test_disk.sh"
WASM_RELEASE = ROOT / "target" / "wasm32-unknown-unknown" / "release" / "fm1_web.wasm"
WASM_TARGET = WEB_DIR / "fm1_web.wasm"

FWSC = os.environ.get("FM1_FWSC") or os.environ.get("MODD_FWSC", "")


def has_export(wasm_path, name):
    data = wasm_path.read_bytes()
    return name.encode() in data


@pytest.fixture(scope="module")
def disk_image(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("disk")
    img = tmp / "test.img"
    result = subprocess.run(
        ["sh", str(GENERATOR), str(img), "32"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        pytest.skip(f"create_test_disk.sh failed: {result.stderr}")
    return img


@pytest.fixture(scope="module")
def fwsc_path():
    if not FWSC or not os.path.exists(FWSC):
        pytest.skip("no filebrowser firmware: set FM1_FWSC=.../modd.fwsc")
    return Path(FWSC)


@pytest.fixture(scope="module")
def wasm_ready():
    if not WASM_TARGET.exists() and WASM_RELEASE.exists():
        shutil.copy2(WASM_RELEASE, WASM_TARGET)
    if not WASM_TARGET.exists():
        pytest.skip(f"no wasm: build with cargo build -p fm1-web --target wasm32-unknown-unknown --release")
    if not has_export(WASM_TARGET, "fm1_usb_disk"):
        pytest.skip("wasm missing fm1_usb_disk export: rebuild fm1-web")


@pytest.fixture(scope="module")
def http_server(wasm_ready):
    handler = http.server.SimpleHTTPRequestHandler
    server = http.server.HTTPServer(("127.0.0.1", 0), handler)
    server.timeout = 0.5
    os.chdir(WEB_DIR)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{port}"
    server.shutdown()


@pytest.fixture(scope="module")
def browser_page(http_server):
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        pytest.skip("playwright not installed")
    pw = sync_playwright().start()
    chromium_path = os.environ.get("CHROMIUM_PATH") or None
    try:
        browser = pw.chromium.launch(headless=True, executable_path=chromium_path)
    except Exception as e:
        pw.stop()
        pytest.skip(f"cannot launch chromium: {e}")
    video_dir = ROOT / ".scratch" / "videos"
    video_dir.mkdir(parents=True, exist_ok=True)
    context = browser.new_context(
        record_video_dir=str(video_dir),
        record_video_size={"width": 1280, "height": 720},
        viewport={"width": 1280, "height": 720},
    )
    page = context.new_page()
    page.goto(http_server)
    yield page
    video_path = page.video.path()
    context.close()
    browser.close()
    pw.stop()
    if video_path:
        final = video_dir / "test_filebrowser_mount_image.webm"
        Path(video_path).rename(final)
        print(f"\nvideo: {final}")


def test_filebrowser_mount_image(browser_page, fwsc_path, disk_image):
    """Upload firmware + disk image, verify the firmware boots with the disk attached."""
    page = browser_page

    page.wait_for_selector("#disk", state="attached", timeout=5000)

    page.locator("#open").set_input_files(str(fwsc_path))
    page.locator("#start").click()

    page.wait_for_function(
        "document.getElementById('status').textContent.includes('MHz')",
        timeout=30000,
    )
    status = page.text_content("#status")
    assert "MHz" in status, f"firmware should be running: {status}"

    page.locator("#disk").set_input_files(str(disk_image))

    page.wait_for_function(
        "document.getElementById('diskname')?.textContent?.includes('.img')",
        timeout=5000,
    )
    disk_label = page.text_content("#diskname")
    assert ".img" in disk_label, f"disk name should show the uploaded file: {disk_label}"

    page.wait_for_function(
        "document.getElementById('status').textContent.includes('MHz')",
        timeout=30000,
    )
    status = page.text_content("#status")
    assert "MHz" in status, f"firmware should still be running after disk attach: {status}"

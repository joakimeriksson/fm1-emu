"""Playwright smoke tests for fm1-live --cdp.

Spawns fm1-live with --cdp PORT, opens the thin page in a browser, and
drives it with Playwright. Requires: reference/firmwares/felucca.fwsc,
playwright, chromium browser.

Run:  python -m pytest tests/cdp/test_cdp_smoke.py -v
"""

import os
import signal
import socket
import subprocess
import sys
import time

import pytest

FWSC = os.path.join(os.path.dirname(__file__), "..", "..", "reference", "firmwares", "felucca.fwsc")
FWSC = os.path.normpath(FWSC)
FM1_LIVE = os.path.join(os.path.dirname(__file__), "..", "..", "target", "debug", "fm1-live")
FM1_LIVE = os.path.normpath(FM1_LIVE)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for_port(port, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.1)
    return False


@pytest.fixture(scope="module")
def cdp_server():
    if not os.path.exists(FWSC):
        pytest.skip(f"no firmware: {FWSC}")
    if not os.path.exists(FM1_LIVE):
        pytest.skip(f"fm1-live not built: {FM1_LIVE}")
    port = free_port()
    proc = subprocess.Popen(
        [FM1_LIVE, "--fwsc", FWSC, "--cdp", str(port)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if not wait_for_port(port):
        proc.kill()
        pytest.fail("cdp server did not start")
    yield f"http://127.0.0.1:{port}", port
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()


@pytest.fixture(scope="module")
def browser_page(cdp_server):
    from playwright.sync_api import sync_playwright
    url, _ = cdp_server
    pw = sync_playwright().start()
    chromium_path = os.environ.get("CHROMIUM_PATH") or None
    browser = pw.chromium.launch(headless=True, executable_path=chromium_path)
    page = browser.new_page()
    page.goto(url)
    page.wait_for_function("document.getElementById('status').textContent.includes('instructions/s')", timeout=30000)
    yield page
    browser.close()
    pw.stop()


def test_boot_and_lcd(browser_page):
    """The page boots, the canvas has non-zero pixel data."""
    page = browser_page
    status = page.text_content("#status")
    assert "instructions/s" in status

    pixel_sum = page.evaluate("""() => {
        const c = document.getElementById('panel');
        const ctx = c.getContext('2d');
        const d = ctx.getImageData(370, 42, 240, 240).data;
        let sum = 0;
        for (let i = 0; i < d.length; i++) sum += d[i];
        return sum;
    }""")
    assert pixel_sum > 0, "LCD area should have non-zero pixels"


def test_press_button(browser_page):
    """Pressing a button via pressButton() changes the LCD revision."""
    page = browser_page
    rev_before = page.evaluate("() => getLcdRevision().then(r => r.revision)")
    page.evaluate("() => pressButton(12)")
    time.sleep(0.5)
    rev_after = page.evaluate("() => getLcdRevision().then(r => r.revision)")
    assert rev_after != rev_before, f"revision should change after button press: {rev_before} -> {rev_after}"


def test_encoder_turn(browser_page):
    """Turning an encoder via turnEncoder() changes the LCD revision."""
    page = browser_page
    rev_before = page.evaluate("() => getLcdRevision().then(r => r.revision)")
    page.evaluate("() => turnEncoder(3, 5)")
    time.sleep(0.5)
    rev_after = page.evaluate("() => getLcdRevision().then(r => r.revision)")
    assert rev_after != rev_before, f"revision should change after encoder turn: {rev_before} -> {rev_after}"

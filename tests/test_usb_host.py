"""Dedicated guest acceptance, never silently skipped.

FM1_USB_GUEST_MANIFEST=.../manifest.json FM1_LIVE=target/release/fm1-live \
  python3 -m unittest discover -s tests -p test_usb_host.py -k GuestFilesystemTests

No host FAT parser is used: all returned bytes come from guest FatFS in RAM.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / "tools/usb-host/create_test_disk.sh"
HASHES = {
    16: "d6905b1fb469f20326e4f26d0cfe055da95cfc7a0142c2488e7cf8dfdca5d349",
    32: "193db175e07317ec1a578e0ed4dd9ffc1b5827752b643bfd8566a56df94f4cdb",
}


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def load_contract():
    path = Path(os.environ["FM1_USB_GUEST_MANIFEST"]).resolve()
    manifest = json.loads(path.read_text())
    assert manifest["version"] == 1
    assert manifest["modd_revision"] == "cee682b28d2f580d2954faad3eb50011d45fb347"
    for guest in manifest["guests"].values():
        assert digest(path.parent / guest["file"]) == guest["sha256"], "Guest checksum mismatch"
    live = Path(os.environ.get("FM1_LIVE", ROOT / "target/release/fm1-live")).resolve()
    if not live.is_file():
        raise FileNotFoundError(f"Build fm1-live first: {live}")
    return path.parent, manifest, live


def run_guest(contract, directory, image, mode, interpreter):
    parent, manifest, live = contract
    guest = manifest["guests"][mode]
    ram = directory / "ram.bin"
    # Do not inherit diagnostic settings that alter execution or script inputs.
    env = {k: v for k, v in os.environ.items() if not k.startswith("FM1_")}
    env.update(FM1_DUMPRAM=str(ram), FM1_BENCH_KEYS=manifest["keys"])
    if interpreter:
        env["FM1_NO_JIT"] = "1"
    result = subprocess.run([str(live), "--fwsc", str(parent / guest["file"]),
                             "--usb-mode", "host", "--disk", str(image),
                             "--bench", str(manifest["duration_ms"])],
                            env=env, capture_output=True, timeout=240)
    (directory / "stdout.log").write_bytes(result.stdout)
    (directory / "stderr.log").write_bytes(result.stderr)
    assert result.returncode == 0, result.stderr.decode(errors="replace")[-4000:]
    offset = guest["result_address"] - 0x01C00000
    data = ram.read_bytes()
    assert 0 <= offset <= len(data) - guest["result_size"], "Result outside RAM"
    data = data[offset:offset + guest["result_size"]]
    header = struct.unpack_from("<10I", data)
    assert header[:2] == (0x5544534B, 1), "Guest result not initialized"
    return header, data


class GuestFilesystemTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contract = load_contract()

    def test_fatfs_lifecycle_contents_and_persistence(self):
        # One temp root per test; retain logs on failure via the exception message.
        with tempfile.TemporaryDirectory(prefix="fm1-usb-guest-") as temporary:
            directory = Path(temporary)
            for fat in (16, 32):
                fixture = directory / f"fat{fat}.img"
                subprocess.run(["sh", str(ROOT / "tools/usb-host/create_test_disk.sh"),
                                str(fixture), str(fat)], check=True, capture_output=True)
                reference = None
                for interpreter in (False, True):
                    with self.subTest(fat=fat, interpreter=interpreter):
                        image = directory / "run.img"
                        shutil.copyfile(fixture, image)
                        header, observed = run_guest(self.contract, directory, image, "ordinary", interpreter)
                        activated, deactivated, hosts, devices = struct.unpack_from("<4I", observed, 20368)
                        self.assertGreaterEqual(activated, 2, "Script must enter Files twice")
                        self.assertGreaterEqual(deactivated, 1, "Script must leave Files")
                        self.assertGreaterEqual(hosts, 2, "Guest OTG must start two host sessions")
                        self.assertGreaterEqual(devices, 1, "Guest OTG must return to device mode")
                        self.assertGreaterEqual(header[3], 2, f"Files must mount twice; result={header}")
                        self.assertGreaterEqual(header[4], 1, "Files exit must unmount")
                        self.assertEqual(header[5], 1, "MSC enumeration must complete")
                        self.assertEqual(header[6], (32 if fat == 16 else 64) * 2048)
                        for reopened in (False, True):
                            header, data = run_guest(self.contract, directory, image, "probe", interpreter)
                            self.assertEqual(header[2], 2, f"Guest error {header[8]}")
                            self.assertEqual(header[9], int(reopened))
                            self.assertGreaterEqual(header[3], 2)
                            self.assertGreaterEqual(header[4], 1)
                            names = [data[40 + i * 20:56 + i * 20].split(b"\0")[0]
                                     for i in range(header[7])]
                            self.assertEqual(set(names), {b"README.TXT", b"PRESETS.TXT", b"SETLIST.CSV",
                                                          b"PATTERN.BIN"} | ({b"OUTPUT.BIN"} if reopened else set()))
                            self.assertEqual(struct.unpack_from("<4I", data, 200), (233, 606, 229, 17033))
                            contents = data[216:216 + 18101]
                            # Text bytes are fixture literals, not extracted by a host FAT implementation.
                            script = (ROOT / "tools/usb-host/create_test_disk.sh").read_text()
                            # Each here-document includes its final newline.
                            texts = b"".join((block.split("\nENDFILE", 1)[0] + "\n").encode() for block in
                                             script.split("<< 'ENDFILE'\n")[1:])
                            pattern = bytes((i * 17 + (i >> 9) * 31) & 255 for i in range(17033))
                            self.assertEqual(contents, texts + pattern)
                            written = data[18317:18317 + 2051]
                            self.assertEqual(written, bytes((i * 29 + (i >> 9) * 7 + 3) & 255
                                                            for i in range(2051)))
                            stable = contents + written
                            if reference is None:
                                reference = stable
                            self.assertEqual(stable, reference, "JIT/interpreter/reopen results differ")


class DiskFixtureTests(unittest.TestCase):
    def test_reproducible_images_and_contents(self):
        with tempfile.TemporaryDirectory(prefix="fm1-usb-fixture-") as tmp:
            for fat in (16, 32):
                with self.subTest(fat=fat):
                    for suffix in ("a", "b"):
                        image = Path(tmp) / f"fat{fat}-{suffix}.img"
                        subprocess.run(["sh", str(GENERATOR), str(image), str(fat)],
                                       check=True, capture_output=True)
                        with image.open("rb") as f:
                            digest = hashlib.file_digest(f, "sha256").hexdigest()
                        self.assertEqual(digest, HASHES[fat])
                    expected_sizes = {"README.TXT": 233, "PRESETS.TXT": 606,
                                      "SETLIST.CSV": 229, "PATTERN.BIN": 17033}
                    for name, size in expected_sizes.items():
                        data = subprocess.check_output(["mtype", "-i", str(image), "::" + name])
                        self.assertEqual(len(data), size)
                        if name == "PATTERN.BIN":
                            self.assertEqual(data, bytes((i * 17 + (i >> 9) * 31) & 255
                                                         for i in range(size)))
                    # Explicitly verify the generator cannot truncate an existing image.
                    result = subprocess.run(["sh", str(GENERATOR), str(image), str(fat)],
                                            capture_output=True)
                    self.assertNotEqual(result.returncode, 0)
                    with image.open("rb") as f:
                        self.assertEqual(hashlib.file_digest(f, "sha256").hexdigest(), HASHES[fat])

    def test_arguments_required(self):
        for args in ([], ["unused.img", "12"]):
            result = subprocess.run(["sh", str(GENERATOR), *args], capture_output=True)
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""The wall of firmwares for the film: every catalog firmware's screen, live, on one wall that the
camera opens on one of them and pulls back from until all of them are in view; the sound is the
firmware nearest the middle of the frame, changing as the camera moves (a radio dial).

    python3 tools/film_wall.py capture [--jobs N]    each firmware's LCD and audio (fm1-live --shot
                                                     --record --lcd-only), into work/film/wall/
    python3 tools/film_wall.py compose OUT.mp4 [--start ID] [--seconds S]

capture: reference/firmwares/<id>.fwsc for every id in web/firmwares.json (tools/fetch_firmwares.mjs),
played 6.5 s with PLAY and a chord early so the screens move; kept: 5 s from 1.5 s, the LCD at 25 fps
(tile.npy) and the audio (audio.npy, its level evened out). compose: 1280x820 at 25 fps, as
tools/film.py's scenes, for a scene's "clip".
"""
import json
import math
import os
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
WALL = os.path.join(ROOT, "work", "film", "wall")
LIVE = os.path.join(ROOT, "target", "release", "fm1-live")
W, H, FPS = 1280, 820, 25
KEYS = "1000:PLAY,1200:PLAY^,1500:C4,1500:E4,1500:G4,4500:C4^,4500:E4^,4500:G4^"
FROM_MS, LEN_MS, RUN_MS = 1500, 5000, 6500
RATE = 44118            # fm1-live --record's audio.raw
NFR = LEN_MS * FPS // 1000
FONT = "/System/Library/Fonts/Avenir Next.ttc"


def catalog():
    cat = json.load(open(os.path.join(ROOT, "web", "firmwares.json")))["firmwares"]
    return [(f["id"], f["name"]) for f in cat if os.path.exists(os.path.join(ROOT, "reference", "firmwares", f["id"] + ".fwsc"))]


def capture_one(fid):
    out = os.path.join(WALL, fid)
    if os.path.exists(os.path.join(out, "audio.npy")):
        return fid, "kept"
    rec = os.path.join(out, "rec")
    shutil.rmtree(rec, ignore_errors=True)
    os.makedirs(rec)
    env = dict(os.environ, FM1_BENCH_KEYS=KEYS)
    r = subprocess.run([LIVE, "--fwsc", os.path.join("reference", "firmwares", fid + ".fwsc"), "--shot", str(RUN_MS),
                        "--record", rec, "--lcd-only"], cwd=ROOT, env=env, capture_output=True, timeout=900)
    frames = []
    for j in range(NFR):
        k = round((FROM_MS + j * 1000 / FPS) / 16)
        p = os.path.join(rec, f"{k:06d}.ppm")
        if not os.path.exists(p):
            break
        frames.append(np.asarray(Image.open(p).convert("RGB")))
    if not frames:
        shutil.rmtree(rec, ignore_errors=True)
        return fid, f"no frames ({r.returncode})"
    while len(frames) < NFR:
        frames.append(frames[-1])
    np.save(os.path.join(out, "tile.npy"), np.stack(frames))
    raw = np.fromfile(os.path.join(rec, "audio.raw"), dtype="<i2") if os.path.exists(os.path.join(rec, "audio.raw")) else np.zeros(0, "<i2")
    a = raw.reshape(-1, 2).astype(np.float32) / 32768 if raw.size >= 2 else np.zeros((0, 2), np.float32)
    a = a[FROM_MS * RATE // 1000:(FROM_MS + LEN_MS) * RATE // 1000]
    a = np.pad(a, ((0, max(0, LEN_MS * RATE // 1000 - len(a))), (0, 0)))
    rms = float(np.sqrt(np.mean(a ** 2))) if a.size else 0.0
    if rms > 1e-4:
        a *= min(10 ** (-20 / 20) / rms, 10.0)   # to -20 dBFS, at most +20 dB
    np.save(os.path.join(out, "audio.npy"), a.astype(np.float32))
    shutil.rmtree(rec, ignore_errors=True)
    return fid, "ok"


def capture(jobs):
    os.makedirs(WALL, exist_ok=True)
    ids = [fid for fid, _ in catalog()]
    with ThreadPoolExecutor(jobs) as ex:
        for fid, status in ex.map(capture_one, ids):
            if status not in ("ok", "kept"):
                print(f"{fid}: {status}", flush=True)
    print(f"{len(ids)} firmwares captured into {WALL}")


def ease(x):
    x = min(max(x, 0.0), 1.0)
    return x * x * (3 - 2 * x)


def compose(out, start_id, seconds):
    fws = [(fid, name) for fid, name in catalog() if os.path.exists(os.path.join(WALL, fid, "tile.npy"))]
    n = len(fws)
    cols = math.ceil(math.sqrt(n * W / H * 0.95))
    rows = math.ceil(n / cols)
    P, T = 256, 240                      # world pitch, tile
    tiles = [np.load(os.path.join(WALL, fid, "tile.npy"), mmap_mode="r") for fid, _ in fws]
    audio = [np.load(os.path.join(WALL, fid, "audio.npy")) for fid, _ in fws]
    k0 = next((i for i, (fid, _) in enumerate(fws) if fid == start_id), n // 2)
    last = n - (rows - 1) * cols          # the last row's tiles, centred

    def pos(i):
        r = i // cols
        shift = (cols - last) * P / 2 if r == rows - 1 else 0
        return (i % cols) * P + P / 2 + shift, r * P + P / 2
    wall_w, wall_h = cols * P, rows * P
    fit = min(W / wall_w, (H - 90) / wall_h)   # (room for a caption under it)
    frames_n = int(seconds * FPS)
    sx, sy = pos(k0)
    font_cache = {}

    def font(size):
        if size not in font_cache:
            try:
                font_cache[size] = ImageFont.truetype(FONT, size, index=5)
            except OSError:
                font_cache[size] = ImageFont.load_default()
        return font_cache[size]

    def clamp(c, view, size):
        """A centre that keeps the view on the wall where the wall is wider than the view, else its middle."""
        return min(max(c, view / 2), size - view / 2) if size > view else size / 2

    def camera(t):
        cx, cy, s = path(t)
        return clamp(cx, W / s, wall_w), clamp(cy, (H - 90) / s, wall_h) + (35 / s if wall_h * s <= H - 90 else 0), s

    def path(t):
        """(centre x, centre y, scale) at t seconds: a pull-back from the first tile (0-25%), then
        a sweep across the wall while it widens (25-75%), then the whole wall (75-100%)."""
        u = t / seconds
        z_tile, z_mid = 2.6, 0.62
        if u < 0.25:
            s = math.exp(math.log(z_tile) + (math.log(z_mid) - math.log(z_tile)) * ease(u / 0.25))
            return sx, sy, s
        if u < 0.75:
            v = (u - 0.25) / 0.5
            s = math.exp(math.log(z_mid) + (math.log(fit) - math.log(z_mid)) * ease(v))
            # across the wall to its far side, then back to its middle
            far = (wall_w * 0.82, wall_h * 0.78) if sx < wall_w / 2 else (wall_w * 0.18, wall_h * 0.78)
            if v < 0.6:
                w = ease(v / 0.6)
                return sx + (far[0] - sx) * w, sy + (far[1] - sy) * w, s
            w = ease((v - 0.6) / 0.4)
            return far[0] + (wall_w / 2 - far[0]) * w, far[1] + (wall_h / 2 - far[1]) * w, s
        return wall_w / 2, wall_h / 2, fit

    ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}",
                           "-r", str(FPS), "-i", "-", "-c:v", "libx264", "-crf", "18", "-preset", "medium",
                           "-pix_fmt", "yuv420p", out + ".video.mp4"], stdin=subprocess.PIPE)
    centre = []
    for f in range(frames_n):
        t = f / FPS
        cx, cy, s = camera(t)
        img = Image.new("RGB", (W, H), (18, 18, 22))
        d = ImageDraw.Draw(img)
        ts = max(2, int(round(T * s)))
        label = ts >= 110
        fnt = font(max(11, min(30, int(ts * 0.085)))) if label else None
        best, bestd = 0, 1e18
        for i in range(n):
            wx, wy = pos(i)
            x, y = (wx - cx) * s + W / 2, (wy - cy) * s + H / 2
            dd = (x - W / 2) ** 2 + (y - H / 2) ** 2
            if dd < bestd:
                best, bestd = i, dd
            if x + ts < -10 or x - ts > W + 10 or y + ts < -10 or y - ts > H + 10:
                continue
            fr = tiles[i][(f + i * 7) % NFR]          # (each tile its own phase)
            im = Image.fromarray(np.asarray(fr)).resize((ts, ts), Image.NEAREST if ts >= T else Image.BILINEAR)
            img.paste(im, (int(x - ts / 2), int(y - ts / 2)))
            if label:
                name = fws[i][1]
                tw = d.textlength(name, font=fnt)
                d.text((x - tw / 2, y + ts / 2 + 2), name, font=fnt, fill=(205, 205, 210))
        centre.append(best)
        ff.stdin.write(img.tobytes())
    ff.stdin.close()
    ff.wait()

    # the sound: the firmware nearest the middle, frame by frame, 40 ms windows overlapped
    hop = RATE // FPS
    win = np.hanning(2 * hop).astype(np.float32)[:, None]
    mix = np.zeros((hop * (frames_n + 2), 2), np.float32)
    for f, i in enumerate(centre):
        a = audio[i]
        p0 = (f * hop) % max(1, len(a) - 2 * hop)
        seg = a[p0:p0 + 2 * hop]
        if len(seg) < 2 * hop:
            seg = np.pad(seg, ((0, 2 * hop - len(seg)), (0, 0)))
        mix[f * hop:f * hop + 2 * hop] += seg * win
    mix = np.clip(mix[:hop * frames_n], -1, 1)
    wav = out + ".audio.raw"
    (mix * 32767).astype("<i2").tofile(wav)
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", out + ".video.mp4", "-f", "s16le", "-ar", str(RATE),
                    "-ac", "2", "-i", wav, "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-ar", "44100", "-shortest", out],
                   check=True)
    os.remove(out + ".video.mp4")
    os.remove(wav)
    print(f"{out}: {n} firmwares on a {cols}x{rows} wall, {seconds} s, from {fws[k0][1]}")


if __name__ == "__main__":
    a = sys.argv[1:]
    if a and a[0] == "capture":
        capture(int(a[a.index("--jobs") + 1]) if "--jobs" in a else 6)
    elif len(a) >= 2 and a[0] == "compose":
        compose(os.path.abspath(a[1]), a[a.index("--start") + 1] if "--start" in a else "polly",
                float(a[a.index("--seconds") + 1]) if "--seconds" in a else 14.0)
    else:
        sys.exit(__doc__)

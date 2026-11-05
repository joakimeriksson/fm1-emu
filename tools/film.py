#!/usr/bin/env python3
"""A film of the emulator, rendered headlessly: each scene is a firmware run by fm1-live with a
key script (FM1_BENCH_FRAMES: the front panel at 25 fps and the audio from the boot), captions
drawn over it, the scenes cross-faded into one video.

    python3 tools/film.py tools/film-0.4.0.json work/film/film.mp4
    python3 tools/film.py tools/film-0.4.0.json work/film/film.mp4 --only open-melodee  (one scene)

The scene list is JSON: {"scenes": [ {...}, ... ]} with, per scene,
    name      the clip's name (its folder under work/film/)
    fw        the firmware id (reference/firmwares/<id>: tools/fetch_firmwares.mjs), or
    fwsc      a package file
    ms        emulated milliseconds to run (the clip starts at `from` seconds and lasts `dur`)
    keys      fm1-live's FM1_BENCH_KEYS script (ms:C4 presses, ms:C4^ releases, ms:PLAY, ms:e0:3)
    env       more environment for the run ({"FM1_LCD_TIMED": "1"})
    live      another fm1-live binary (an older release, for a before / after)
    from, dur the clip: seconds into the run, and its length
    captions  [{"text", "sub", "from", "to", "style"}]: seconds into the clip; style "title" is
              centred and large, "top" sits top left (over a recording with its own captions at
              the bottom), the default sits bottom left
    card      a full-frame card instead of the panel: {"lines": [[text, size], ...]} over the
              run's audio
    clip      a video file instead of a run (a screen recording: the plugin in a DAW, the page
              in a browser), scaled and letterboxed to the frame, with its own audio; `from` and
              `dur` cut it, the captions go over it
Needs ffmpeg (libx264, overlay, xfade, acrossfade, loudnorm) and Pillow for the captions.
"""
import json
import os
import shutil
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont

W, H = 1280, 820
FPS = 25
FONT = "/System/Library/Fonts/Avenir Next.ttc"  # macOS; any TTF/TTC works
FADE = 0.4  # seconds of cross-fade between scenes


def font(size, face=2):
    """face: index into the collection (Avenir Next: 0 Bold, 2 Demi Bold, 5 Medium, 7 Regular)."""
    try:
        return ImageFont.truetype(FONT, size, index=face)
    except OSError:
        return ImageFont.load_default(size)


def fit(d, text, size, face, maxw):
    """The font at `size`, or smaller until `text` fits in `maxw` pixels."""
    while True:
        f = font(size, face)
        if size <= 12 or d.textlength(text, font=f) <= maxw:
            return f
        size -= 2


def caption_png(path, text, sub=None, style=None):
    img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if style == "title":
        f, fs = fit(d, text, 92, 0, W - 200), fit(d, sub or "", 40, 5, W - 200)
        tw = d.textlength(text, font=f)
        sw = d.textlength(sub, font=fs) if sub else 0
        bw = max(tw, sw) + 120
        bh = 150 + (70 if sub else 0)
        x0, y0 = (W - bw) / 2, (H - bh) / 2 - 40
        d.rounded_rectangle((x0, y0, x0 + bw, y0 + bh), 28, fill=(0, 0, 0, 170))
        d.text(((W - tw) / 2, y0 + 28), text, font=f, fill=(255, 255, 255, 255))
        if sub:
            d.text(((W - sw) / 2, y0 + 142), sub, font=fs, fill=(220, 220, 220, 255))
    else:
        f, fs = fit(d, text, 50, 2, W - 128), fit(d, sub or "", 30, 5, W - 128)
        tw = d.textlength(text, font=f)
        sw = d.textlength(sub, font=fs) if sub else 0
        bw = max(tw, sw) + 56
        bh = 84 + (40 if sub else 0)
        x0, y0 = 36, (36 if style == "top" else H - bh - 36)
        d.rounded_rectangle((x0, y0, x0 + bw, y0 + bh), 18, fill=(0, 0, 0, 165))
        d.text((x0 + 28, y0 + 14), text, font=f, fill=(255, 255, 255, 255))
        if sub:
            d.text((x0 + 28, y0 + 78), sub, font=fs, fill=(220, 220, 220, 255))
    img.save(path)


def card_png(path, lines):
    img = Image.new("RGB", (W, H), (24, 24, 28))
    d = ImageDraw.Draw(img)
    total = sum(size + 22 for _, size in lines)
    y = (H - total) / 2
    for text, size in lines:
        f = font(size, 0 if size >= 60 else 5)
        tw = d.textlength(text, font=f)
        d.text(((W - tw) / 2, y), text, font=f, fill=(245, 245, 245) if size >= 60 else (200, 200, 205))
        y += size + 22
    img.save(path)


def run(cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def render(sc, root, out):
    """fm1-live's frames and audio for the scene into out/frames/."""
    frames = os.path.join(out, "frames")
    if os.path.isdir(frames):
        shutil.rmtree(frames)
    live = sc.get("live", os.path.join(root, "target/release/fm1-live"))
    env = dict(os.environ, FM1_BENCH_FRAMES=frames, FM1_WARMUP_MS="0")
    env["FM1_BENCH_KEYS"] = sc.get("keys", "")
    if not sc.get("keys"):
        env["FM1_BENCH_NOKEYS"] = "1"
    env.update(sc.get("env", {}))
    cmd = [live, "--bench", sc["ms"]]
    cmd += ["--fwsc", sc["fwsc"]] if "fwsc" in sc else ["--fw", sc["fw"]]
    run(cmd, env=env, cwd=root, stdout=subprocess.DEVNULL)
    return frames


def scene(sc, root, work):
    out = os.path.join(work, sc["name"])
    os.makedirs(out, exist_ok=True)
    clip = os.path.join(out, "clip.mp4")
    if "clip" in sc:
        src = sc["clip"] if os.path.isabs(sc["clip"]) else os.path.join(root, sc["clip"])
        inputs = ["-i", src]
        first = f"[0:v]scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,fps={FPS},format=rgba[v0]"
    else:
        frames = render(sc, root, out)
        inputs = ["-f", "s16le", "-ar", "44100", "-ac", "2", "-i", os.path.join(frames, "audio.raw")]
        first = "[0:v]format=rgba[v0]"
    if "clip" in sc:
        pass
    elif "card" in sc:
        card = os.path.join(out, "card.png")
        card_png(card, sc["card"]["lines"])
        inputs = ["-loop", "1", "-framerate", FPS, "-i", card] + inputs
    else:
        inputs = ["-framerate", FPS, "-i", os.path.join(frames, "%06d.ppm")] + inputs
    fc, last = [first], "v0"
    # (a clip is one input with both streams; a run is the frames [0] and the audio [1])
    nin, audio = (1, "0:a") if "clip" in sc else (2, "1:a")
    for i, c in enumerate(sc.get("captions", [])):
        png = os.path.join(out, f"cap{i}.png")
        caption_png(png, c["text"], c.get("sub"), c.get("style"))
        inputs += ["-loop", "1", "-framerate", FPS, "-i", png]
        a, b = c["from"] + sc.get("from", 0), c["to"] + sc.get("from", 0)
        fc.append(f"[{i + nin}:v]format=rgba,fade=in:st={a}:d=0.3:alpha=1,fade=out:st={b - 0.3}:d=0.3:alpha=1[c{i}]")
        fc.append(f"[{last}][c{i}]overlay=0:0:enable='between(t,{a},{b})'[v{i + 1}]")
        last = f"v{i + 1}"
    fc.append(f"[{last}]format=yuv420p[vout]")
    fc.append(f"[{audio}]loudnorm=I=-16:TP=-1.5:LRA=11[aout]")
    cmd = ["ffmpeg", "-y", "-loglevel", "error"] + inputs + ["-filter_complex", ";".join(fc),
           "-map", "[vout]", "-map", "[aout]", "-ss", sc.get("from", 0), "-t", sc["dur"],
           "-c:v", "libx264", "-crf", "18", "-preset", "medium", "-c:a", "aac", "-b:a", "192k",
           "-ar", "44100", "-shortest", clip]
    run(cmd)
    return clip


def duration(path):
    r = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of",
                        "csv=p=0", path], capture_output=True, text=True, check=True)
    return float(r.stdout.strip())


def assemble(clips, out):
    if len(clips) == 1:
        shutil.copy(clips[0], out)
        return
    inputs = []
    for c in clips:
        inputs += ["-i", c]
    durs = [duration(c) for c in clips]
    fc, vlast, alast, t = [], "0:v", "0:a", 0.0
    for i in range(1, len(clips)):
        t += durs[i - 1] - FADE
        fc.append(f"[{vlast}][{i}:v]xfade=transition=fade:duration={FADE}:offset={t:.3f}[xv{i}]")
        fc.append(f"[{alast}][{i}:a]acrossfade=d={FADE}[xa{i}]")
        vlast, alast = f"xv{i}", f"xa{i}"
    cmd = ["ffmpeg", "-y", "-loglevel", "error"] + inputs + ["-filter_complex", ";".join(fc),
           "-map", f"[{vlast}]", "-map", f"[{alast}]", "-c:v", "libx264", "-crf", "18",
           "-preset", "medium", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k",
           "-movflags", "+faststart", out]
    run(cmd)


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    spec, out = sys.argv[1], sys.argv[2]
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    work = os.path.dirname(os.path.abspath(out)) or "."
    os.makedirs(work, exist_ok=True)
    with open(spec) as f:
        scenes = json.load(f)["scenes"]
    clips = [scene(sc, root, work) for sc in scenes if only is None or sc["name"] == only]
    if not clips:
        sys.exit(f"no scene named {only}")
    assemble(clips, out)
    print(f"{out}: {duration(out):.1f} s, {len(clips)} scenes")


if __name__ == "__main__":
    main()

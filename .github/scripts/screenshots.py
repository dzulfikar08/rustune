"""Run the release binary in a pseudo-terminal, send key sequences and save
what the screen shows after each step as PNG.

The terminal is emulated with pyte; cells are painted with Pillow using
their own foreground/background colours (truecolor included) in DejaVu Sans
Mono. Default colours are painted twice, as a dark and a light terminal, to
show the footer reads on both.

Usage: screenshots.py BINARY SKIN_WSZ OUT_DIR
"""

import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import wave

import fcntl
import termios

import pyte
from PIL import Image, ImageDraw, ImageFont

BIN, SKIN, OUT = (os.path.abspath(a) for a in sys.argv[1:4])
os.makedirs(OUT, exist_ok=True)

FONT_DIR = "/usr/share/fonts/truetype/dejavu"
FONT = ImageFont.truetype(os.path.join(FONT_DIR, "DejaVuSansMono.ttf"), 14)
BOLD = ImageFont.truetype(os.path.join(FONT_DIR, "DejaVuSansMono-Bold.ttf"), 14)
CELL_W, CELL_H = 9, 18

# Terminal palettes: (default fg, default bg).
DARK = ("d0d0d0", "141414")
LIGHT = ("202020", "f8f8f8")
NAMED = {
    "black": "000000", "red": "cd0000", "green": "00cd00", "brown": "cdcd00",
    "blue": "0000ee", "magenta": "cd00cd", "cyan": "00cdcd", "white": "e5e5e5",
    "brightblack": "7f7f7f", "brightred": "ff0000", "brightgreen": "00ff00",
    "brightbrown": "ffff00", "brightblue": "5c5cff", "brightmagenta": "ff00ff",
    "brightcyan": "00ffff", "brightwhite": "ffffff",
}


def rgb(name, default):
    if name == "default":
        name = default
    name = NAMED.get(name, name)
    try:
        return tuple(int(name[i:i + 2], 16) for i in (0, 2, 4))
    except ValueError:
        return tuple(int(default[i:i + 2], 16) for i in (0, 2, 4))


def paint(screen, path, palette):
    fg0, bg0 = palette
    img = Image.new("RGB", (screen.columns * CELL_W, screen.lines * CELL_H), rgb(bg0, bg0))
    draw = ImageDraw.Draw(img)
    for y in range(screen.lines):
        line = screen.buffer[y]
        for x in range(screen.columns):
            ch = line[x]
            fg, bg = rgb(ch.fg, fg0), rgb(ch.bg, bg0)
            if ch.reverse:
                fg, bg = bg, fg
            x0, y0 = x * CELL_W, y * CELL_H
            draw.rectangle([x0, y0, x0 + CELL_W - 1, y0 + CELL_H - 1], fill=bg)
            c = ch.data
            if c in ("", " "):
                continue
            half = CELL_H // 2
            if c == "▀":
                draw.rectangle([x0, y0, x0 + CELL_W - 1, y0 + half - 1], fill=fg)
            elif c == "▄":
                draw.rectangle([x0, y0 + half, x0 + CELL_W - 1, y0 + CELL_H - 1], fill=fg)
            elif c == "█":
                draw.rectangle([x0, y0, x0 + CELL_W - 1, y0 + CELL_H - 1], fill=fg)
            else:
                draw.text((x0, y0 + 1), c, font=BOLD if ch.bold else FONT, fill=fg)
    img.save(path)


def make_wav(path, freq, secs=20, rate=22050):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(b"".join(
            struct.pack("<h", int(6000 * math.sin(2 * math.pi * freq * i / rate)))
            for i in range(rate * secs)))


def home_for(theme, music):
    home = tempfile.mkdtemp(prefix="home-")
    conf = os.path.join(home, ".config", "rustune")
    os.makedirs(os.path.join(conf, "skins"))
    shutil.copy(SKIN, os.path.join(conf, "skins", "base-2.91.wsz"))
    with open(os.path.join(conf, "config.toml"), "w") as f:
        f.write('onboarding_done = true\nmusic_dir = "%s"\ntheme = "%s"\ngraphics = "halfblock"\n'
                % (music, theme))
    with open(os.path.join(conf, "state.toml"), "w") as f:
        f.write('skin = "base-2.91.wsz"\ntheme = "%s"\n' % theme)
    return home


TAB, BACKTAB, ENTER, ESC, BACKSPACE = b"\t", b"\x1b[Z", b"\r", b"\x1b", b"\x7f"

failures = []


def session(name, theme, cols, rows, steps, tracks):
    music = os.path.dirname(tracks[0])
    home = home_for(theme, music)
    env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", HOME=home,
               XDG_CONFIG_HOME=os.path.join(home, ".config"),
               XDG_CACHE_HOME=os.path.join(home, ".cache"),
               XDG_RUNTIME_DIR=tempfile.mkdtemp(dir="/tmp"))
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    proc = subprocess.Popen([BIN, *tracks], stdin=slave, stdout=slave, stderr=slave, env=env,
                            start_new_session=True)
    os.close(slave)

    screen = pyte.Screen(cols, rows)
    # Answer terminal queries (device attributes, cursor position).
    screen.write_process_input = lambda data: os.write(master, data.encode())
    stream = pyte.ByteStream(screen)
    lock = threading.Lock()
    raw = bytearray()

    def drain():
        while True:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            raw.extend(chunk)
            with lock:
                stream.feed(chunk)

    threading.Thread(target=drain, daemon=True).start()
    for i, (label, keys, wait, check) in enumerate(steps, 1):
        for k in keys:
            os.write(master, k)
            time.sleep(0.4)
        time.sleep(wait)
        with lock:
            text = screen.display
            base = os.path.join(OUT, "%s-%02d-%s" % (name, i, label))
            paint(screen, base + ".png", DARK)
            if theme == "Winamp" and label in ("cursor", "radio-menu"):
                paint(screen, base + "-light-terminal.png", LIGHT)
        print("== %s step %d %s ==" % (name, i, label))
        print("\n".join(text[-3:]))
        if check and not check(text):
            failures.append("%s/%s: check failed:\n%s" % (name, label, "\n".join(text[-3:])))
    os.write(master, b"q")
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        failures.append("%s: did not exit on q" % name)
    if b"panicked" in raw:
        failures.append("%s: panic in output" % name)


def footer(*needles):
    return lambda text: all(any(n in line for line in text[-2:]) for n in needles)


music = tempfile.mkdtemp(prefix="music-")
tracks = []
for n, (name, freq) in enumerate([("Sine A 220 Hz", 220), ("Sine B 330 Hz", 330),
                                   ("Sine C 440 Hz", 440), ("Sine D 550 Hz", 550),
                                   ("Sine E 660 Hz", 660)]):
    path = os.path.join(music, "%02d %s.wav" % (n + 1, name))
    make_wav(path, freq)
    tracks.append(path)

WINAMP_STEPS = [
    ("start", [], 4, footer("Playlist · 5 tracks", "all keys")),
    ("cursor", [b"j", b"j"], 1, footer("move", "play")),
    ("enter", [ENTER], 2, None),
    ("tab-online", [TAB], 1, footer("source Online")),
    ("radio-menu", [TAB], 1, footer("Radio", "back")),
    ("radio-cursor", [b"j", b"j"], 1, None),
    ("radio-open", [ENTER], 6, footer("Radio › Trending")),
    ("radio-back", [BACKSPACE], 1, None),
    ("back-to-playlist", [ESC], 1, footer("Playlist")),
    ("local", [BACKTAB, BACKTAB], 1, footer("source Local")),
    ("search-results", [ENTER], 2, footer("match")),
    ("search-results", [ENTER], 2, footer("in the playlist")),
]

session("winamp-160x48", "Winamp", 160, 48, WINAMP_STEPS, tracks)
session("winamp-100x30", "Winamp", 100, 30, [
    ("start", [], 4, footer("all keys")),
    ("cursor", [b"j", b"j"], 1, None),
    ("radio-menu", [TAB, TAB], 1, footer("Radio")),
    ("back-to-playlist", [ESC], 1, None),
    ("search-typing", [BACKTAB, BACKTAB, b"/", b"s", b"i", b"n"], 1, footer("Search Local: sin")),
], tracks)
session("winamp-skin-280x120", "Winamp", 280, 120, [
    ("start", [], 5, footer("all keys")),
    ("cursor", [b"j", b"j"], 1, None),
    ("radio-menu", [TAB, TAB], 1, footer("Radio")),
], tracks)
session("dark-160x48", "Dark", 160, 48, [
    ("start", [], 4, None),
    ("cursor", [b"j", b"j"], 1, None),
], tracks)

print("\n".join(sorted(os.listdir(OUT))))
if failures:
    print("\n\n".join(failures))
    sys.exit(1)

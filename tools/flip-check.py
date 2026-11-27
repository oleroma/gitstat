#!/usr/bin/env python3
"""Checks a recorded flip or slide (Settings > Diagnostics > Record frames).

Usage: flip-check.py <recordings folder or one recording> [--last N] [--crops DIR]

For each recording it compares the frames either side of every change of
phase (the word in a frame's name: slide, fade, turn, hold, reveal, after):
those are the moments one drawing takes over from another, where nothing on
screen should change. It prints how many pixels did, and where. A swap is
clean at 0. With --crops it saves the two frames' changed areas side by
side, enlarged, to look at.

Needs Pillow and numpy.
"""
import os
import re
import sys

import numpy as np
from PIL import Image

THRESHOLD = 20  # a channel must change by more than this to count
BLOCK = 24  # px; changed pixels nearer than this are one area


def phase(name):
    m = re.match(r"\d+-\d+ms-([a-z]+)", name)
    return m.group(1) if m else name


def load(path):
    return np.asarray(Image.open(path).convert("RGB")).astype(np.int16)


def areas(mask):
    h, w = mask.shape
    grid = mask[: h // BLOCK * BLOCK, : w // BLOCK * BLOCK]
    grid = grid.reshape(h // BLOCK, BLOCK, w // BLOCK, BLOCK).any(axis=(1, 3))
    seen = np.zeros_like(grid)
    found = []
    for y, x in zip(*np.nonzero(grid)):
        if seen[y, x]:
            continue
        stack, ys, xs = [(y, x)], [], []
        seen[y, x] = True
        while stack:
            a, b = stack.pop()
            ys.append(a)
            xs.append(b)
            for c, d in ((a + 1, b), (a - 1, b), (a, b + 1), (a, b - 1)):
                if 0 <= c < grid.shape[0] and 0 <= d < grid.shape[1] and grid[c, d] and not seen[c, d]:
                    seen[c, d] = True
                    stack.append((c, d))
        found.append((min(xs) * BLOCK, min(ys) * BLOCK, (max(xs) + 1) * BLOCK, (max(ys) + 1) * BLOCK))
    return found


def check(folder, crops):
    names = sorted(n for n in os.listdir(folder) if n.endswith(".png"))
    if len(names) < 2:
        return None
    print(f"{os.path.basename(folder)}: {len(names)} frames")
    worst = 0
    for a, b in zip(names, names[1:]):
        if phase(a) == phase(b):
            continue
        fa, fb = load(os.path.join(folder, a)), load(os.path.join(folder, b))
        if fa.shape != fb.shape:
            continue
        mask = np.abs(fa - fb).max(axis=2) > THRESHOLD
        count = int(mask.sum())
        worst = max(worst, count)
        found = areas(mask) if count else []
        print(f"  {phase(a):>7} -> {phase(b):<7} {count:>7} px in {len(found)} areas  ({a[:4]} -> {b[:4]})")
        for box in found[:12]:
            print(f"      at {box}")
        if crops and count:
            os.makedirs(crops, exist_ok=True)
            for i, box in enumerate(found[:6]):
                x0, y0, x1, y1 = box
                box = (max(0, x0 - 8), max(0, y0 - 8), x1 + 8, y1 + 8)
                pa = Image.open(os.path.join(folder, a)).convert("RGB").crop(box)
                pb = Image.open(os.path.join(folder, b)).convert("RGB").crop(box)
                z = max(1, min(4, 1200 // max(1, pa.width * 2)))
                both = Image.new("RGB", (pa.width * 2 * z + 8, pa.height * z), (255, 0, 255))
                both.paste(pa.resize((pa.width * z, pa.height * z), Image.NEAREST), (0, 0))
                both.paste(pb.resize((pb.width * z, pb.height * z), Image.NEAREST), (pa.width * z + 8, 0))
                both.save(os.path.join(crops, f"{os.path.basename(folder)}-{phase(a)}-{phase(b)}-{i}.png"))
    return worst


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    last, crops = 2, None
    if "--last" in args:
        i = args.index("--last")
        last = int(args[i + 1])
        del args[i : i + 2]
    if "--crops" in args:
        i = args.index("--crops")
        crops = args[i + 1]
        del args[i : i + 2]
    root = args[0]
    if any(n.endswith(".png") for n in os.listdir(root)):
        folders = [root]
    else:
        folders = sorted(
            (os.path.join(root, d) for d in os.listdir(root) if os.path.isdir(os.path.join(root, d))),
            key=os.path.getmtime,
        )[-last:]
    worst = [w for w in (check(f, crops) for f in folders) if w is not None]
    sys.exit(1 if any(worst) else 0)


if __name__ == "__main__":
    main()

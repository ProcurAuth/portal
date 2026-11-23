#!/usr/bin/env python3
"""Make the PNG files smaller: reduce each picture to an adaptive palette and write it again.

Usage: optimise.py DIR [LIMIT_KB]
Each picture gets the largest palette (256 colours down to 32) that keeps the file under LIMIT_KB (default 240).
The script needs Python 3 and the package Pillow. The text of a screenshot stays sharp: the interface has few colours.
"""
import glob
import os
import sys

from PIL import Image

directory = sys.argv[1]
limit = int(sys.argv[2]) * 1024 if len(sys.argv) > 2 else 240 * 1024
total = 0
for path in sorted(glob.glob(os.path.join(directory, "*.png"))):
    image = Image.open(path).convert("RGB")
    best = None
    for colours in (256, 192, 128, 96, 64, 48, 32):
        quant = image.quantize(colors=colours, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
        tmp = path + ".tmp"
        quant.save(tmp, format="PNG", optimize=True)
        size = os.path.getsize(tmp)
        if best is None or size <= limit:
            if best:
                os.remove(best[0])
            best = (tmp, size, colours)
            if size <= limit:
                break
        else:
            os.remove(tmp)
    os.replace(best[0], path)
    total += best[1]
    print(f"{os.path.basename(path)}: {best[1] // 1024} KB, {best[2]} colours")
print(f"total {total // 1024} KB")

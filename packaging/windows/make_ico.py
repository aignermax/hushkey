"""Build the multi-size .ico from assets/logo.png.

Without an argument it writes the Inno Setup icon next to this script;
install.ps1 passes the path of a git checkout's Start Menu icon instead.
"""
import os
import sys

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..", "..")

target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "logo.ico")
img = Image.open(os.path.join(ROOT, "assets", "logo.png")).convert("RGBA")
# A half-written icon must never replace a good one: shortcuts keep using it.
partial = target + ".tmp"
img.save(partial, format="ICO",
         sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64),
                (128, 128), (256, 256)])
os.replace(partial, target)
print("logo.ico written")

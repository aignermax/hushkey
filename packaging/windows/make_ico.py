"""Build the multi-size .ico for the Inno Setup installer from assets/logo.png."""
import os

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..", "..")

img = Image.open(os.path.join(ROOT, "assets", "logo.png")).convert("RGBA")
img.save(os.path.join(HERE, "logo.ico"),
         sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64),
                (128, 128), (256, 256)])
print("logo.ico written")

"""Add committed branding to the upstream static build without changing its sources."""
from pathlib import Path
import shutil

root = Path(__file__).resolve().parents[1]
source = root / "assets/icons/web"
output = root / "web/dist"
page = output / "index.html"
html = page.read_text()
old = '<link rel="icon" href="data:,">'
links = """<link rel="icon" href="favicon.ico" sizes="any">
<link rel="icon" type="image/png" sizes="32x32" href="icons/favicon-32.png">
<link rel="apple-touch-icon" sizes="180x180" href="icons/apple-touch-icon-180.png">
<link rel="manifest" href="site.webmanifest">"""
if old not in html:
    raise SystemExit("Upstream favicon markup changed; review the branding adapter")
shutil.copytree(source / "icons", output / "icons", dirs_exist_ok=True)
for name in ("favicon.ico", "site.webmanifest"):
    shutil.copyfile(source / name, output / name)
page.write_text(html.replace(old, links, 1))
print("Packaged FM-1 browser and home-screen icons")

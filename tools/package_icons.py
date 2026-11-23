"""Package committed icon artifacts without regenerating or installing them."""
from pathlib import Path
import shutil
import tempfile

root = Path(__file__).resolve().parents[1]
(root / "target").mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="icon-export-", dir=root / "target") as stage:
    output = Path(stage) / "FM-1-icons"
    (output / "assets").mkdir(parents=True)
    (output / "tools").mkdir()
    shutil.copytree(root / "assets/icons", output / "assets/icons")
    for name in ("generate-icons.swift", "check_icon_artwork.py", "package_icons.py"):
        shutil.copyfile(root / "tools" / name, output / "tools" / name)
    (output / "README.md").write_text((root / "assets/icons/README.md").read_text().replace("../../context/", "context/"))
    (output / "context").mkdir()
    (output / "context/icons.md").write_text((root / "context/icons.md").read_text().replace("[status](status.md)", "the repository context/status.md").replace("[the mandatory rebuild checklist](development.md#rebuild-after-changes)", "the repository context/development.md rebuild checklist"))
    shutil.make_archive(str(root / "target/FM-1-icons"), "zip", stage, "FM-1-icons")
    shutil.copytree(output, root / "target/FM-1-icons", dirs_exist_ok=True)
print("Packaged target/FM-1-icons.zip and refreshed target/FM-1-icons/")

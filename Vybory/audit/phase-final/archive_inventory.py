import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[2]
archive = root / "audit" / "phase-final" / "archive"
inventory_path = root / "audit" / "phase-final" / "archive_checksums.json"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


files = []
for path in sorted(item for item in archive.rglob("*") if item.is_file()):
    files.append({
        "path": path.relative_to(archive).as_posix(),
        "bytes": path.stat().st_size,
        "sha256": sha256(path),
    })

inventory_path.write_text(
    json.dumps({"archive_root": str(archive), "file_count": len(files), "files": files}, ensure_ascii=False, indent=2),
    encoding="utf-8",
)
print(json.dumps({"file_count": len(files), "inventory": str(inventory_path)}, ensure_ascii=False))

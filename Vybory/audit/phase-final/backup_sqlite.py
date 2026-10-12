import sqlite3
from pathlib import Path

root = Path(__file__).resolve().parents[2]
source_path = root / "database" / "elections.sqlite"
backup_path = root / "audit" / "phase-final" / "archive" / "elections-pre-final.sqlite"
backup_path.parent.mkdir(parents=True, exist_ok=True)
if backup_path.exists():
    raise SystemExit(f"backup already exists: {backup_path}")
source = sqlite3.connect(source_path)
target = sqlite3.connect(backup_path)
with target:
    source.backup(target)
print(target.execute("PRAGMA integrity_check").fetchone()[0])
print(len(target.execute("PRAGMA foreign_key_check").fetchall()))
target.close()
source.close()

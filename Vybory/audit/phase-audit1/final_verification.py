import csv
import hashlib
import json
import sqlite3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
AUDIT = ROOT / "audit" / "phase-audit1"
OLD = ROOT / "output" / "2026"
NEW = OLD / "candidate-audit1"
DB = ROOT / "database" / "elections.sqlite"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest().upper()


def load_numeric(path):
    result = {}
    published_nonempty = 0
    protocol_date_nonempty = 0
    with path.open("r", encoding="utf-8-sig", newline="") as handle:
        reader = csv.DictReader(handle)
        for row in reader:
            key = row["uik_id"]
            result[key] = tuple(int(row[f"line_{line:02}"]) for line in range(1, 23))
            published_nonempty += bool(row.get("published_at"))
            protocol_date_nonempty += bool(row.get("protocol_date"))
    return result, published_nonempty, protocol_date_nonempty


old, _, _ = load_numeric(OLD / "all_uiks.csv")
new, published_nonempty, protocol_date_nonempty = load_numeric(NEW / "all_uiks.csv")
baseline = json.loads((AUDIT / "baseline_hashes.json").read_text(encoding="utf-8-sig"))
baseline_map = {row["Path"]: row["SHA256"] for row in baseline}

manifest = json.loads((NEW / "manifest.json").read_text(encoding="utf-8"))
manifest_mismatches = []
for item in manifest["files"]:
    path = NEW / item["path"]
    if not path.exists() or sha256(path) != item["sha256"].upper() or path.stat().st_size != item["bytes"]:
        manifest_mismatches.append(item["path"])

conn = sqlite3.connect(DB)
integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
foreign_key_violations = len(conn.execute("PRAGMA foreign_key_check").fetchall())
aggregate_sources = conn.execute("SELECT COUNT(*) FROM aggregate_sources").fetchone()[0]
source_hash_errors = 0
for body, stored in conn.execute(
    "SELECT s.body_json,s.sha256 FROM aggregate_sources a JOIN source_documents s ON s.id=a.source_document_id"
):
    if hashlib.sha256(body.encode("utf-8")).hexdigest().lower() != stored.lower():
        source_hash_errors += 1

availability = conn.execute(
    "SELECT diagnostic_status,COALESCE(http_status,-1),COUNT(*) FROM protocol_availability_checks GROUP BY diagnostic_status,COALESCE(http_status,-1) ORDER BY 1,2"
).fetchall()
categories = dict(conn.execute("SELECT category,COUNT(*) FROM precincts GROUP BY category"))
db_protocols = conn.execute("SELECT COUNT(*) FROM protocols WHERE is_strict_valid=1").fetchone()[0]
db_duplicates = conn.execute(
    "SELECT COUNT(*) FROM (SELECT election_id,uik_id,COUNT(*) c FROM protocols GROUP BY election_id,uik_id HAVING c>1)"
).fetchone()[0]
line_errors = conn.execute(
    "SELECT COUNT(*) FROM protocols p WHERE p.is_strict_valid=1 AND (SELECT COUNT(*) FROM protocol_values v WHERE v.version_id=p.current_version_id AND v.line_number BETWEEN 1 AND 22 AND v.value IS NOT NULL) != 22"
).fetchone()[0]
conn.close()

result = {
    "sqlite_integrity": integrity,
    "foreign_key_violations": foreign_key_violations,
    "db_strict_valid_protocols": db_protocols,
    "db_duplicate_uiks": db_duplicates,
    "db_protocols_without_22_numeric_lines": line_errors,
    "old_csv_rows": len(old),
    "candidate_csv_rows": len(new),
    "same_uik_set": old.keys() == new.keys(),
    "numeric_22_mismatches_old_vs_candidate": sum(old.get(key) != values for key, values in new.items()) + len(old.keys() - new.keys()),
    "candidate_published_at_nonempty": published_nonempty,
    "candidate_protocol_date_nonempty": protocol_date_nonempty,
    "aggregate_sources": aggregate_sources,
    "aggregate_source_hash_errors": source_hash_errors,
    "availability_status_counts": availability,
    "category_counts": categories,
    "candidate_manifest_files": len(manifest["files"]),
    "candidate_manifest_mismatches": manifest_mismatches,
    "original_all_uiks_unchanged": sha256(OLD / "all_uiks.csv") == baseline_map["output/2026/all_uiks.csv"],
    "original_manifest_unchanged": sha256(OLD / "manifest.json") == baseline_map["output/2026/manifest.json"],
    "original_report_unchanged": sha256(OLD / "report.json") == baseline_map["output/2026/report.json"],
}
(AUDIT / "final_verification.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps(result, ensure_ascii=False, indent=2))

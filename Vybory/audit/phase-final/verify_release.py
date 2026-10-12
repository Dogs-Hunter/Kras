import argparse
import csv
import hashlib
import json
import sqlite3
from datetime import datetime
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
DB = ROOT / "database" / "elections.sqlite"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def csv_values(path):
    values = {}
    dates = {"protocol_date_nonempty": 0, "source_created_at_nonempty": 0}
    with path.open("r", encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle)
        header = reader.fieldnames or []
        for row in reader:
            key = row["uik_id"]
            if key in values:
                raise AssertionError(f"duplicate UIK: {key}")
            numeric = tuple(int(row[f"line_{line:02}"]) for line in range(1, 23))
            if any(value < 0 for value in numeric):
                raise AssertionError(f"negative value: {key}")
            values[key] = numeric
            dates["protocol_date_nonempty"] += bool(row["protocol_date"])
            dates["source_created_at_nonempty"] += bool(row["source_created_at"])
    return header, values, dates


def row_count(path):
    with path.open("r", encoding="utf-8", newline="") as handle:
        return sum(1 for _ in csv.reader(handle)) - 1


parser = argparse.ArgumentParser()
parser.add_argument("--release", required=True, type=Path)
parser.add_argument("--reference", required=True, type=Path)
parser.add_argument("--output", required=True, type=Path)
parser.add_argument("--archive", type=Path)
args = parser.parse_args()
release = args.release.resolve()
reference = args.reference.resolve()

header, current, dates = csv_values(release / "all_uiks.csv")
with (reference / "all_uiks.csv").open("r", encoding="utf-8", newline="") as handle:
    reference_rows = csv.DictReader(handle)
    reference_values = {
        row["uik_id"]: tuple(int(row[f"line_{line:02}"]) for line in range(1, 23))
        for row in reference_rows
    }

required_columns = {
    "election_id", "election_date", "uik_id", "protocol_date",
    "source_created_at", "source_record_id", "source_url", "retrieved_at",
    *(f"line_{line:02}" for line in range(1, 23)),
}
assert required_columns <= set(header)
assert "published_at" not in header
assert len(header) == 44
assert current == reference_values
assert len(current) == 89132

report = json.loads((release / "report.json").read_text(encoding="utf-8"))
schema = json.loads((release / "protocol_schema.json").read_text(encoding="utf-8"))
parties = json.loads((release / "political_parties.json").read_text(encoding="utf-8"))
manifest = json.loads((release / "manifest.json").read_text(encoding="utf-8"))
assert report["discovered_uiks"] == 91399
assert report["strict_valid_protocols"] == 89132
assert report["unavailable_protocols"] == 2267
assert report["coverage_denominator"] == "discovered_uiks"
assert report["official_repeated_protocols"] == 176
assert report["uiks_with_multiple_saved_versions"] == 0
assert report["repeated_without_prior_saved_version"] == 176
assert report["aggregate_sources_saved"] == 2839
expected_categories = {
    "regular": (88348, 86081, 2267),
    "special": (2718, 2718, 0),
    "foreign": (326, 326, 0),
    "baikonur": (7, 7, 0),
}
actual_categories = {
    item["name"]: (item["discovered_uiks"], item["received_protocols"], item["missing_protocols"])
    for item in report["coverage_by_category"]
}
assert actual_categories == expected_categories
assert len(schema["lines"]) == 22
assert [item["protocol_line"] for item in parties] == list(range(13, 23))
assert (release / "README_DATASET.md").is_file()
assert row_count(release / "special_precincts" / "foreign" / "all.csv") == 326
assert row_count(release / "special_precincts" / "baikonur" / "all.csv") == 7

listed = set()
manifest_errors = []
for item in manifest["files"]:
    relative = PurePosixPath(item["path"])
    if relative.is_absolute() or ".." in relative.parts:
        manifest_errors.append(f"unsafe:{item['path']}")
        continue
    path = release.joinpath(*relative.parts)
    listed.add(relative.as_posix())
    if not path.is_file():
        manifest_errors.append(f"missing:{item['path']}")
    elif path.stat().st_size != item["bytes"]:
        manifest_errors.append(f"size:{item['path']}")
    elif digest(path) != item["sha256"]:
        manifest_errors.append(f"sha256:{item['path']}")
actual_files = {
    path.relative_to(release).as_posix()
    for path in release.rglob("*")
    if path.is_file() and path.name != "manifest.json"
}
assert listed == actual_files
assert not manifest_errors

conn = sqlite3.connect(DB)
integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
foreign_keys = len(conn.execute("PRAGMA foreign_key_check").fetchall())
db_counts = {
    "discovered": conn.execute("SELECT COUNT(*) FROM precincts").fetchone()[0],
    "strict_valid": conn.execute("SELECT COUNT(*) FROM protocols WHERE is_strict_valid=1").fetchone()[0],
    "duplicates": conn.execute("SELECT COUNT(*) FROM (SELECT election_id,uik_id,COUNT(*) c FROM protocols GROUP BY election_id,uik_id HAVING c>1)").fetchone()[0],
    "bad_line_sets": conn.execute("SELECT COUNT(*) FROM protocols p WHERE is_strict_valid=1 AND (SELECT COUNT(*) FROM protocol_values v WHERE v.version_id=p.current_version_id AND v.line_number BETWEEN 1 AND 22 AND v.value IS NOT NULL)!=22").fetchone()[0],
    "source_metadata": conn.execute("SELECT COUNT(*) FROM protocol_source_metadata WHERE source_created_at IS NOT NULL AND source_field_name='createdAt'").fetchone()[0],
    "aggregate_sources": conn.execute("SELECT COUNT(*) FROM aggregate_sources").fetchone()[0],
    "aggregate_source_documents": conn.execute("SELECT COUNT(*) FROM aggregate_sources a JOIN source_documents s ON s.id=a.source_document_id WHERE s.document_type='protocol_2_tik_aggregate'").fetchone()[0],
    "availability_unknown": conn.execute("SELECT COUNT(*) FROM protocol_availability_checks WHERE diagnostic_status='Unknown'").fetchone()[0],
}
assert integrity == "ok" and foreign_keys == 0
assert db_counts == {
    "discovered": 91399, "strict_valid": 89132, "duplicates": 0,
    "bad_line_sets": 0, "source_metadata": 89132,
    "aggregate_sources": 2839, "aggregate_source_documents": 2839,
    "availability_unknown": 2267,
}
timestamp_errors = 0
for value, in conn.execute("SELECT source_created_at FROM protocol_source_metadata WHERE source_created_at IS NOT NULL"):
    parsed = datetime.fromisoformat(value)
    if parsed.utcoffset() is None:
        timestamp_errors += 1
conn.close()
assert timestamp_errors == 0

archive = None
if args.archive:
    archive_root = args.archive.resolve()
    old_manifest = json.loads((archive_root / "manifest.json").read_text(encoding="utf-8"))
    old_errors = []
    for item in old_manifest["files"]:
        path = archive_root.joinpath(*PurePosixPath(item["path"]).parts)
        if not path.is_file() or path.stat().st_size != item["bytes"] or digest(path) != item["sha256"]:
            old_errors.append(item["path"])
    candidate_manifest = json.loads((archive_root / "candidate-audit1" / "manifest.json").read_text(encoding="utf-8"))
    candidate_errors = []
    for item in candidate_manifest["files"]:
        path = archive_root / "candidate-audit1" / Path(*PurePosixPath(item["path"]).parts)
        if not path.is_file() or path.stat().st_size != item["bytes"] or digest(path) != item["sha256"]:
            candidate_errors.append(item["path"])
    assert not old_errors and not candidate_errors
    archive = {
        "path": str(archive_root),
        "previous_manifest_files": len(old_manifest["files"]),
        "candidate_manifest_files": len(candidate_manifest["files"]),
        "previous_manifest_errors": old_errors,
        "candidate_manifest_errors": candidate_errors,
    }

result = {
    "release_path": str(release),
    "reference_path": str(reference),
    "csv_columns": len(header),
    "csv_rows": len(current),
    "unique_uiks": len(current),
    "numeric_22_mismatches": 0,
    **dates,
    "manifest_files": len(manifest["files"]),
    "manifest_errors": manifest_errors,
    "sqlite_integrity": integrity,
    "foreign_key_violations": foreign_keys,
    "database_counts": db_counts,
    "timestamp_parse_errors": timestamp_errors,
    "category_counts": actual_categories,
    "archive": archive,
}
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps(result, ensure_ascii=False, indent=2))

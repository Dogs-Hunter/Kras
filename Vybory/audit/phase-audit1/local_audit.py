import csv
import hashlib
import json
import sqlite3
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DB = ROOT / "database" / "elections.sqlite"
CSV = ROOT / "output" / "2026" / "all_uiks.csv"
CACHE = ROOT / "cache" / "raw" / "protocols"
AUDIT = ROOT / "audit" / "phase-audit1"


def scalar(conn, sql):
    return conn.execute(sql).fetchone()[0]


conn = sqlite3.connect(f"file:{DB.as_posix()}?mode=ro", uri=True)
conn.row_factory = sqlite3.Row

csv_rows = {}
duplicate_csv_ids = []
with CSV.open("r", encoding="utf-8", newline="") as handle:
    for row in csv.DictReader(handle):
        uik_id = row["uik_id"]
        if uik_id in csv_rows:
            duplicate_csv_ids.append(uik_id)
        csv_rows[uik_id] = row

summary = {
    "sqlite_integrity": scalar(conn, "PRAGMA integrity_check"),
    "foreign_key_violations": len(conn.execute("PRAGMA foreign_key_check").fetchall()),
    "journal_mode": scalar(conn, "PRAGMA journal_mode"),
    "csv_rows": len(csv_rows),
    "csv_duplicate_uik_ids": len(duplicate_csv_ids),
    "protocols": scalar(conn, "SELECT COUNT(*) FROM protocols"),
    "protocol_versions": scalar(conn, "SELECT COUNT(*) FROM protocol_versions"),
    "official_repeated_protocols": scalar(
        conn, "SELECT COUNT(*) FROM protocol_versions WHERE repeated=1"
    ),
    "uiks_with_multiple_saved_versions": scalar(
        conn,
        "SELECT COUNT(*) FROM (SELECT protocol_id FROM protocol_versions GROUP BY protocol_id HAVING COUNT(*)>1)",
    ),
    "repeated_without_prior_saved_version": scalar(
        conn,
        "SELECT COUNT(*) FROM protocol_versions pv WHERE pv.repeated=1 AND "
        "(SELECT COUNT(*) FROM protocol_versions p2 WHERE p2.protocol_id=pv.protocol_id)=1",
    ),
    "empty_addresses": scalar(
        conn,
        "SELECT COUNT(*) FROM precincts WHERE precinct_address IS NULL OR TRIM(precinct_address)=''",
    ),
    "empty_sign_datetime": scalar(
        conn,
        "SELECT COUNT(*) FROM protocol_versions WHERE sign_datetime IS NULL OR TRIM(sign_datetime)=''",
    ),
    "nonempty_published_at": scalar(
        conn,
        "SELECT COUNT(*) FROM protocol_versions WHERE published_at IS NOT NULL AND TRIM(published_at)<>''",
    ),
    "uncategorized_geography": scalar(
        conn, "SELECT COUNT(*) FROM precincts WHERE federal_district IS NULL"
    ),
}

checks = Counter()
conflicts = []
rows = conn.execute(
    """
    SELECT p.uik_id, pr.protocol_num, pv.id version_id, pv.official_report_id,
           pv.repeated, sd.sha256, sd.body_json
    FROM precincts p
    JOIN protocols pr ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
    JOIN protocol_versions pv ON pv.id=pr.current_version_id
    JOIN source_documents sd ON sd.id=pv.source_document_id
    ORDER BY p.uik_id
    """
)
for dbrow in rows:
    uik_id = dbrow["uik_id"]
    csv_row = csv_rows.get(uik_id)
    if csv_row is None:
        checks["missing_csv_row"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "missing_csv_row"})
        continue
    raw = dbrow["body_json"]
    digest = hashlib.sha256(raw.encode("utf-8")).hexdigest()
    if digest != dbrow["sha256"]:
        checks["source_hash_conflict"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "source_hash_conflict"})
        continue
    cache_path = CACHE / f"{uik_id}.json"
    if not cache_path.exists():
        checks["missing_cache"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "missing_cache"})
        continue
    cache_raw = cache_path.read_text(encoding="utf-8")
    if cache_raw != raw:
        checks["cache_db_bytes_conflict"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "cache_db_bytes_conflict"})
        continue
    try:
        report = json.loads(raw)
    except json.JSONDecodeError:
        checks["invalid_source_json"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "invalid_source_json"})
        continue
    body = report.get("body", {})
    if body.get("commissionClassifierId") != uik_id:
        checks["commission_mismatch"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "commission_mismatch"})
        continue
    if body.get("protocolNum") != 2 or dbrow["protocol_num"] != 2:
        checks["protocol_type_mismatch"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "protocol_type_mismatch"})
        continue
    if report.get("id") != dbrow["official_report_id"]:
        checks["report_id_mismatch"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "report_id_mismatch"})
        continue
    by_line = {}
    duplicate_lines = set()
    for record in body.get("records", []):
        raw_line = record.get("infoPrintNum")
        try:
            line = int(raw_line)
        except (TypeError, ValueError):
            continue
        if line in by_line:
            duplicate_lines.add(line)
        by_line[line] = record.get("value")
    if duplicate_lines:
        checks["duplicate_source_lines"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "duplicate_source_lines"})
        continue
    mismatch = False
    for line in range(1, 23):
        try:
            source_value = int(by_line[line])
            csv_value = int(csv_row[f"line_{line:02d}"])
        except (KeyError, TypeError, ValueError):
            mismatch = True
            break
        if source_value != csv_value:
            mismatch = True
            break
    if mismatch:
        checks["numeric_conflict"] += 1
        conflicts.append({"uik_id": uik_id, "reason": "numeric_conflict"})
    else:
        checks["exact_cache_db_csv_match"] += 1

summary.update(checks)
summary["conflicts_total"] = len(conflicts)
summary["source_rows_not_in_csv"] = scalar(
    conn,
    "SELECT COUNT(*) FROM protocols WHERE is_strict_valid=0",
)

uncategorized = conn.execute(
    """
    SELECT region_id, region_name, iksrf_id, iksrf_name, tik_id, tik_name,
           uik_id, uik_number, uik_name, source_url
    FROM precincts WHERE federal_district IS NULL
    ORDER BY tik_name, uik_number, uik_id
    """
).fetchall()
with (AUDIT / "uncategorized_precincts.csv").open("w", encoding="utf-8", newline="") as handle:
    columns = list(uncategorized[0].keys()) if uncategorized else ["uik_id"]
    writer = csv.DictWriter(handle, fieldnames=columns)
    writer.writeheader()
    for row in uncategorized:
        writer.writerow(dict(row))

incomplete_tiks = conn.execute(
    """
    SELECT p.region_id, p.region_name, p.tik_id, p.tik_name,
           COUNT(*) expected_uiks,
           SUM(CASE WHEN j.status='completed' THEN 1 ELSE 0 END) received_protocols,
           SUM(CASE WHEN j.status!='completed' THEN 1 ELSE 0 END) missing_protocols
    FROM precincts p
    JOIN download_jobs j ON j.election_id=p.election_id AND j.uik_id=p.uik_id
    GROUP BY p.region_id, p.region_name, p.tik_id, p.tik_name
    HAVING COUNT(*) != SUM(CASE WHEN j.status='completed' THEN 1 ELSE 0 END)
       OR p.tik_id IS NULL
    ORDER BY p.region_id, p.tik_id
    """
).fetchall()
with (AUDIT / "incomplete_tiks.csv").open("w", encoding="utf-8", newline="") as handle:
    columns = list(incomplete_tiks[0].keys()) if incomplete_tiks else ["tik_id"]
    columns += ["official_aggregate_url", "reason"]
    writer = csv.DictWriter(handle, fieldnames=columns)
    writer.writeheader()
    for row in incomplete_tiks:
        item = dict(row)
        tik_id = item.get("tik_id")
        item["official_aggregate_url"] = (
            "http://apps.cikrf.ru/service/ik-inp-service-pbcopy/reports/242"
            f"?commissionClassifierId={tik_id}&protocolNum=2"
            if tik_id
            else ""
        )
        item["reason"] = "missing_uik_protocols" if tik_id else "no_tik_relation"
        writer.writerow(item)

failed = conn.execute(
    """
    SELECT p.region_id, p.region_name, p.tik_id, p.tik_name, p.uik_id,
           p.uik_number, p.uik_name, j.status, j.attempts, j.last_error,
           j.updated_at
    FROM precincts p
    JOIN download_jobs j ON j.election_id=p.election_id AND j.uik_id=p.uik_id
    WHERE j.status='failed'
    ORDER BY p.region_id, p.tik_id, p.uik_number, p.uik_id
    """
).fetchall()
with (AUDIT / "unavailable_protocols.csv").open("w", encoding="utf-8", newline="") as handle:
    columns = list(failed[0].keys()) if failed else ["uik_id"]
    columns += ["source_url", "http_status", "diagnostic_status", "diagnostic_basis"]
    writer = csv.DictWriter(handle, fieldnames=columns)
    writer.writeheader()
    for row in failed:
        item = dict(row)
        item["source_url"] = (
            "http://apps.cikrf.ru/service/ik-inp-service-pbcopy/reports/242"
            f"?commissionClassifierId={item['uik_id']}&protocolNum=2"
        )
        item["http_status"] = "404" if "404 Not Found" in (item["last_error"] or "") else ""
        item["diagnostic_status"] = "Unknown"
        item["diagnostic_basis"] = "stored HTTP 404 alone is insufficient to prove non-publication"
        writer.writerow(item)

summary["incomplete_tik_rows"] = len(incomplete_tiks)
summary["unavailable_protocol_rows"] = len(failed)
summary["uncategorized_tik_names"] = dict(
    Counter((row["tik_name"] or "") for row in uncategorized)
)

(AUDIT / "local_audit_summary.json").write_text(
    json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
)
with (AUDIT / "provenance_conflicts.csv").open("w", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=["uik_id", "reason"])
    writer.writeheader()
    writer.writerows(conflicts)

print(json.dumps(summary, ensure_ascii=False, indent=2))
conn.close()

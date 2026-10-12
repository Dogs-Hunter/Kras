import csv
import json
import re
import sqlite3
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
AUDIT = ROOT / "audit" / "phase-audit1"
DB = ROOT / "database" / "elections.sqlite"
BASE = "http://apps.cikrf.ru/service/ik-inp-service-pbcopy"
USER_AGENT = "Vybory/0.1 (official election data research; reproducible client)"
CAMPAIGN_ID = 587813923


class OfficialClient:
    def __init__(self, requests_per_second=8.0):
        self.interval = 1.0 / requests_per_second
        self.last_request = 0.0
        self.api_key = None

    def wait(self):
        delay = self.interval - (time.monotonic() - self.last_request)
        if delay > 0:
            time.sleep(delay)
        self.last_request = time.monotonic()

    def raw_request(self, request):
        self.wait()
        with urllib.request.urlopen(request, timeout=45) as response:
            return response.status, response.read().decode("utf-8")

    def authenticate(self):
        status, raw = self.raw_request(
            urllib.request.Request(f"{BASE}/challenge/get", headers={"User-Agent": USER_AGENT})
        )
        challenge = json.loads(raw)
        match = re.fullmatch(r"\s*return\s+(\d+)\s*([+-])\s*(\d+)\s*;\s*", challenge["jsTask"])
        if not match:
            raise RuntimeError("unsupported official challenge")
        left, operator, right = match.groups()
        answer = int(left) + int(right) if operator == "+" else int(left) - int(right)
        payload = json.dumps(
            {
                "pubToken": challenge["pubToken"],
                "answer": str(answer),
                "fingerprint": USER_AGENT,
            }
        ).encode("utf-8")
        request = urllib.request.Request(
            f"{BASE}/challenge/solve",
            data=payload,
            method="POST",
            headers={"Content-Type": "application/json", "User-Agent": USER_AGENT},
        )
        _, raw = self.raw_request(request)
        self.api_key = json.loads(raw)["apiKey"]

    def report(self, commission_id):
        if self.api_key is None:
            self.authenticate()
        query = urllib.parse.urlencode(
            {"commissionClassifierId": commission_id, "protocolNum": "2"}
        )
        url = f"{BASE}/reports/242?{query}"
        request = urllib.request.Request(
            url,
            headers={
                "User-Agent": USER_AGENT,
                "X-Api-Key": self.api_key,
                "X-Client-Fingerprint": USER_AGENT,
            },
        )
        try:
            status, raw = self.raw_request(request)
            return url, status, raw
        except urllib.error.HTTPError as error:
            return url, error.code, error.read().decode("utf-8", errors="replace")


def values(report):
    result = {}
    for record in report.get("body", {}).get("records", []):
        try:
            line = int(record.get("infoPrintNum"))
            value = int(record.get("value"))
        except (TypeError, ValueError):
            continue
        if 1 <= line <= 22:
            result[line] = value
    return result


conn = sqlite3.connect(f"file:{DB.as_posix()}?mode=ro", uri=True)
conn.row_factory = sqlite3.Row
client = OfficialClient()

# One UIC from 50 regular regions, plus foreign territory, Baikonur, and
# globally smallest/largest electorates. This is deterministic and diverse.
regular = conn.execute(
    """
    WITH ranked AS (
      SELECT p.region_name, p.tik_name, p.uik_id, p.uik_number,
             ROW_NUMBER() OVER (PARTITION BY p.region_name ORDER BY p.tik_id,p.uik_number,p.uik_id) rn
      FROM precincts p JOIN protocols pr
        ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
      WHERE pr.is_strict_valid=1 AND p.region_name IS NOT NULL
    )
    SELECT * FROM ranked WHERE rn=1 ORDER BY region_name LIMIT 50
    """
).fetchall()
special = conn.execute(
    """
    SELECT p.region_name,p.tik_name,p.uik_id,p.uik_number,1 rn
    FROM precincts p JOIN protocols pr
      ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
    WHERE pr.is_strict_valid=1 AND p.federal_district IS NULL
    GROUP BY p.tik_name ORDER BY p.tik_name
    """
).fetchall()
extremes = conn.execute(
    """
    SELECT p.region_name,p.tik_name,p.uik_id,p.uik_number,1 rn
    FROM precincts p
    JOIN protocols pr ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
    JOIN protocol_values v ON v.version_id=pr.current_version_id AND v.line_number=1
    WHERE pr.is_strict_valid=1
    ORDER BY v.value ASC LIMIT 1
    """
).fetchall()
extremes += conn.execute(
    """
    SELECT p.region_name,p.tik_name,p.uik_id,p.uik_number,1 rn
    FROM precincts p
    JOIN protocols pr ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
    JOIN protocol_values v ON v.version_id=pr.current_version_id AND v.line_number=1
    WHERE pr.is_strict_valid=1
    ORDER BY v.value DESC LIMIT 1
    """
).fetchall()

sample = {}
for row in [*regular, *special, *extremes]:
    sample[row["uik_id"]] = dict(row)

external_rows = []
for item in sample.values():
    local_raw = conn.execute(
        """
        SELECT sd.body_json FROM protocols pr
        JOIN protocol_versions pv ON pv.id=pr.current_version_id
        JOIN source_documents sd ON sd.id=pv.source_document_id
        WHERE pr.uik_id=? AND pr.protocol_num=2
        """,
        (item["uik_id"],),
    ).fetchone()[0]
    local = json.loads(local_raw)
    url, status, raw = client.report(item["uik_id"])
    live = json.loads(raw) if status == 200 else None
    external_rows.append(
        {
            "region_name": item["region_name"] or "",
            "tik_name": item["tik_name"] or "",
            "uik_id": item["uik_id"],
            "uik_number": item["uik_number"] or "",
            "source_url": url,
            "http_status": status,
            "local_report_id": local.get("id", ""),
            "live_report_id": live.get("id", "") if live else "",
            "numeric_22_match": live is not None and values(live) == values(local) and len(values(live)) == 22,
            "same_official_report": live is not None and live.get("id") == local.get("id"),
            "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
    )

with (AUDIT / "external_uik_sample.csv").open("w", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=list(external_rows[0]))
    writer.writeheader()
    writer.writerows(external_rows)

# Four deterministic UICs from each unavailable region. A 404 remains Unknown:
# the endpoint response alone cannot prove whether publication is absent.
unavailable = conn.execute(
    """
    WITH ranked AS (
      SELECT p.region_name,p.tik_name,p.uik_id,p.uik_number,c.commission_type,
             ROW_NUMBER() OVER(PARTITION BY p.region_name ORDER BY p.tik_id,p.uik_number,p.uik_id) rn
      FROM precincts p JOIN download_jobs j
        ON j.election_id=p.election_id AND j.uik_id=p.uik_id
      JOIN commissions c ON c.external_id=p.uik_id
      WHERE j.status='failed'
    ) SELECT * FROM ranked WHERE rn<=4 ORDER BY region_name,rn
    """
).fetchall()
unavailable_rows = []
for row in unavailable:
    url, status, raw = client.report(row["uik_id"])
    unavailable_rows.append(
        {
            "region_name": row["region_name"],
            "tik_name": row["tik_name"] or "",
            "uik_id": row["uik_id"],
            "uik_number": row["uik_number"] or "",
            "commission_type": row["commission_type"],
            "campaign_id": CAMPAIGN_ID,
            "report_id": 242,
            "protocol_num": 2,
            "source_url": url,
            "http_status": status,
            "diagnostic_status": "Unknown",
            "diagnostic_basis": "correct type-5 UUID and endpoint confirmed; sampled official response remains 404",
            "response_excerpt": raw[:240].replace("\r", " ").replace("\n", " "),
            "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
    )

with (AUDIT / "unavailable_live_sample.csv").open("w", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=list(unavailable_rows[0]))
    writer.writeheader()
    writer.writerows(unavailable_rows)

# Test every incomplete TIK for the existence of a separately published
# official aggregate. The aggregate is not used to fill missing UIC data.
incomplete = list(csv.DictReader((AUDIT / "incomplete_tiks.csv").open("r", encoding="utf-8", newline="")))
aggregate_rows = []
for row in incomplete:
    tik_id = row["tik_id"]
    if not tik_id:
        aggregate_rows.append({**row, "http_status": "", "official_lines": 0, "official_report_id": "", "official_aggregate_exists": False, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())})
        continue
    url, status, raw = client.report(tik_id)
    report = json.loads(raw) if status == 200 else None
    aggregate_rows.append(
        {
            **row,
            "official_aggregate_url": url,
            "http_status": status,
            "official_lines": len(values(report)) if report else 0,
            "official_report_id": report.get("id", "") if report else "",
            "official_aggregate_exists": report is not None and len(values(report)) == 22,
            "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
    )

with (AUDIT / "incomplete_tik_aggregates.csv").open("w", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=list(aggregate_rows[0]))
    writer.writeheader()
    writer.writerows(aggregate_rows)

summary = {
    "external_uik_sample_size": len(external_rows),
    "external_uik_http_200": sum(row["http_status"] == 200 for row in external_rows),
    "external_uik_numeric_22_match": sum(row["numeric_22_match"] for row in external_rows),
    "external_uik_same_report_id": sum(row["same_official_report"] for row in external_rows),
    "unavailable_live_sample_size": len(unavailable_rows),
    "unavailable_live_404": sum(row["http_status"] == 404 for row in unavailable_rows),
    "incomplete_tiks_checked": len(aggregate_rows),
    "incomplete_tik_official_aggregates": sum(str(row["official_aggregate_exists"]).lower() == "true" for row in aggregate_rows),
}
(AUDIT / "external_audit_summary.json").write_text(
    json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
)
print(json.dumps(summary, ensure_ascii=False, indent=2))
conn.close()

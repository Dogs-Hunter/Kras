import csv
import json
import re
import sqlite3
import time
import urllib.request
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
AUDIT = ROOT / "audit" / "phase-audit1"
BASE = "http://apps.cikrf.ru/service/ik-inp-service-pbcopy"
AGENT = "Vybory/0.1 (official election data research; reproducible client)"


def request(url, key=None, data=None):
    headers = {"User-Agent": AGENT}
    if key:
        headers.update({"X-Api-Key": key, "X-Client-Fingerprint": AGENT})
    if data is not None:
        headers["Content-Type"] = "application/json"
    try:
        with urllib.request.urlopen(urllib.request.Request(url, data=data, headers=headers), timeout=45) as response:
            return response.status, response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", errors="replace")


_, raw = request(f"{BASE}/challenge/get")
challenge = json.loads(raw)
match = re.fullmatch(r"\s*return\s+(\d+)\s*([+-])\s*(\d+)\s*;\s*", challenge["jsTask"])
if not match:
    raise RuntimeError("unsupported challenge")
left, operator, right = match.groups()
answer = int(left) + int(right) if operator == "+" else int(left) - int(right)
payload = json.dumps({"pubToken": challenge["pubToken"], "answer": str(answer), "fingerprint": AGENT}).encode()
_, raw = request(f"{BASE}/challenge/solve", data=payload)
key = json.loads(raw)["apiKey"]

rows = list(csv.DictReader((AUDIT / "external_uik_sample.csv").open(encoding="utf-8")))[:3]
conn = sqlite3.connect(ROOT / "database" / "elections.sqlite")
results = []
for row in rows:
    internal_id = conn.execute("SELECT internal_id FROM commissions WHERE external_id=?", (row["uik_id"],)).fetchone()[0]
    url = f"{BASE}/commissionOrg/{internal_id}"
    status, raw = request(url, key=key)
    body = json.loads(raw)
    results.append({
        "uik_id": row["uik_id"],
        "uik_number": row["uik_number"],
        "commission_org_id": internal_id,
        "url": url,
        "http_status": status,
        "top_level_keys": sorted(body),
        "name": body.get("name"),
        "address": body.get("address"),
        "voting_room": body.get("votingRoom"),
        "raw": body,
    })
    time.sleep(0.2)
conn.close()

(AUDIT / "address_sample.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps([{k: v for k, v in item.items() if k != "raw"} for item in results], ensure_ascii=False, indent=2))

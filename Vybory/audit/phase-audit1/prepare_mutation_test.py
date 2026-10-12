import sqlite3
from pathlib import Path


root = Path(__file__).resolve().parents[2]
source = sqlite3.connect(
    f"file:{(root / 'audit/phase-audit1/elections-before-audit.sqlite').as_posix()}?mode=ro",
    uri=True,
)
destination_path = root / "audit" / "phase-audit1" / "aggregate-mutation-test.sqlite"
if destination_path.exists():
    destination_path.unlink()
destination = sqlite3.connect(destination_path)
source.backup(destination)
row = destination.execute(
    """
    SELECT p.tik_id,p.uik_id,pr.current_version_id
    FROM precincts p
    JOIN download_jobs j ON j.election_id=p.election_id AND j.uik_id=p.uik_id
    JOIN protocols pr ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
    WHERE p.tik_id IS NOT NULL
    GROUP BY p.tik_id
    HAVING COUNT(*)=SUM(CASE WHEN j.status='completed' THEN 1 ELSE 0 END)
    ORDER BY p.tik_id LIMIT 1
    """
).fetchone()
destination.execute(
    "UPDATE protocol_values SET value=value+1 WHERE version_id=? AND line_number=13",
    (row[2],),
)
destination.commit()
print("|".join(map(str, row)))
destination.close()
source.close()

CREATE TABLE IF NOT EXISTS protocol_source_metadata (
    version_id INTEGER PRIMARY KEY REFERENCES protocol_versions(id) ON DELETE CASCADE,
    source_created_at TEXT,
    source_field_name TEXT NOT NULL DEFAULT 'createdAt'
);

INSERT OR IGNORE INTO protocol_source_metadata(version_id,source_created_at,source_field_name)
SELECT id,published_at,'createdAt'
FROM protocol_versions;

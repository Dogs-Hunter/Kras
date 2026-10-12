PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS elections (
    id INTEGER PRIMARY KEY,
    external_id TEXT NOT NULL UNIQUE,
    vrn TEXT,
    name TEXT NOT NULL,
    voting_date TEXT NOT NULL,
    raw_json TEXT NOT NULL,
    retrieved_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS commissions (
    external_id TEXT PRIMARY KEY,
    internal_id INTEGER,
    election_id INTEGER NOT NULL REFERENCES elections(id),
    commission_type INTEGER NOT NULL,
    number INTEGER,
    name TEXT NOT NULL,
    parent_external_id TEXT REFERENCES commissions(external_id),
    region_id TEXT,
    region_name TEXT,
    federal_district TEXT,
    has_children INTEGER NOT NULL,
    source_url TEXT NOT NULL,
    raw_json TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS commission_relations (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    parent_external_id TEXT NOT NULL REFERENCES commissions(external_id),
    child_external_id TEXT NOT NULL REFERENCES commissions(external_id),
    source_url TEXT NOT NULL,
    PRIMARY KEY (election_id, parent_external_id, child_external_id)
);

CREATE TABLE IF NOT EXISTS precincts (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    uik_id TEXT NOT NULL REFERENCES commissions(external_id),
    uik_number INTEGER,
    uik_name TEXT NOT NULL,
    region_id TEXT,
    region_name TEXT,
    federal_district TEXT,
    iksrf_id TEXT,
    iksrf_name TEXT,
    oik_id TEXT,
    oik_name TEXT,
    tik_id TEXT,
    tik_name TEXT,
    precinct_address TEXT,
    category TEXT NOT NULL DEFAULT 'regular',
    source_url TEXT NOT NULL,
    PRIMARY KEY (election_id, uik_id)
);

CREATE TABLE IF NOT EXISTS source_documents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_url TEXT NOT NULL,
    source_record_id TEXT,
    document_type TEXT NOT NULL,
    retrieved_at TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    body_json TEXT NOT NULL,
    UNIQUE(source_url, sha256)
);

CREATE TABLE IF NOT EXISTS protocols (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    election_id INTEGER NOT NULL REFERENCES elections(id),
    uik_id TEXT NOT NULL REFERENCES commissions(external_id),
    protocol_num INTEGER NOT NULL,
    current_version_id INTEGER,
    status TEXT NOT NULL,
    is_strict_valid INTEGER NOT NULL DEFAULT 0,
    UNIQUE(election_id, uik_id, protocol_num)
);

CREATE TABLE IF NOT EXISTS protocol_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol_id INTEGER NOT NULL REFERENCES protocols(id),
    source_document_id INTEGER NOT NULL REFERENCES source_documents(id),
    official_protocol_id TEXT,
    official_report_id TEXT NOT NULL UNIQUE,
    version_label TEXT NOT NULL,
    signed INTEGER NOT NULL,
    repeated INTEGER NOT NULL,
    sign_datetime TEXT,
    published_at TEXT,
    retrieved_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS protocol_values (
    version_id INTEGER NOT NULL REFERENCES protocol_versions(id) ON DELETE CASCADE,
    line_number INTEGER NOT NULL CHECK(line_number > 0),
    value INTEGER,
    raw_value TEXT,
    official_text TEXT NOT NULL,
    parse_status TEXT NOT NULL,
    party_name TEXT,
    PRIMARY KEY(version_id, line_number)
);

CREATE TABLE IF NOT EXISTS political_parties (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    protocol_line INTEGER NOT NULL,
    ballot_position INTEGER,
    official_name TEXT NOT NULL,
    source_url TEXT NOT NULL,
    PRIMARY KEY(election_id, protocol_line)
);

CREATE TABLE IF NOT EXISTS download_jobs (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    uik_id TEXT NOT NULL REFERENCES commissions(external_id),
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(election_id, uik_id)
);

CREATE TABLE IF NOT EXISTS download_errors (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    election_id INTEGER NOT NULL,
    uik_id TEXT,
    stage TEXT NOT NULL,
    error_kind TEXT NOT NULL,
    message TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS validation_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES protocol_versions(id) ON DELETE CASCADE,
    severity TEXT NOT NULL,
    code TEXT NOT NULL,
    message TEXT NOT NULL,
    UNIQUE(version_id, code, message)
);

CREATE TABLE IF NOT EXISTS aggregate_checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    election_id INTEGER NOT NULL,
    commission_id TEXT NOT NULL,
    line_number INTEGER NOT NULL,
    precinct_count INTEGER NOT NULL,
    completed_count INTEGER NOT NULL,
    uik_sum INTEGER,
    official_value INTEGER,
    difference INTEGER,
    checked_at TEXT NOT NULL,
    UNIQUE(election_id, commission_id, line_number)
);

CREATE INDEX IF NOT EXISTS idx_commissions_parent ON commissions(parent_external_id);
CREATE INDEX IF NOT EXISTS idx_precincts_tik ON precincts(tik_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON download_jobs(status);
CREATE INDEX IF NOT EXISTS idx_protocol_values_line ON protocol_values(line_number);

CREATE TABLE IF NOT EXISTS aggregate_sources (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    commission_id TEXT NOT NULL REFERENCES commissions(external_id),
    source_document_id INTEGER NOT NULL REFERENCES source_documents(id),
    official_report_id TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    PRIMARY KEY (election_id, commission_id)
);

CREATE TABLE IF NOT EXISTS protocol_availability_checks (
    election_id INTEGER NOT NULL REFERENCES elections(id),
    uik_id TEXT NOT NULL REFERENCES commissions(external_id),
    source_url TEXT NOT NULL,
    http_status INTEGER,
    diagnostic_status TEXT NOT NULL,
    diagnostic_basis TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    PRIMARY KEY (election_id, uik_id)
);

CREATE INDEX IF NOT EXISTS idx_protocol_versions_protocol
ON protocol_versions(protocol_id);

UPDATE precincts
SET category='foreign'
WHERE tik_name='Территория за пределами РФ';

UPDATE precincts
SET category='baikonur'
WHERE tik_name='Город Байконур (Республика Казахстан)';

INSERT OR IGNORE INTO protocol_availability_checks(
    election_id,uik_id,source_url,http_status,diagnostic_status,diagnostic_basis,checked_at
)
SELECT j.election_id,j.uik_id,
       'http://apps.cikrf.ru/service/ik-inp-service-pbcopy/reports/242?commissionClassifierId=' || j.uik_id || '&protocolNum=2',
       CASE WHEN j.last_error LIKE '%404%' THEN 404 ELSE NULL END,
       'Unknown',
       CASE WHEN j.last_error LIKE '%404%'
            THEN 'Индивидуальная официальная конечная точка вернула HTTP 404; это не доказывает отсутствие публикации'
            ELSE 'Причина недоступности не установлена' END,
       j.updated_at
FROM download_jobs j
WHERE j.status='failed';

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::model::{
    CommissionNode, Election, HierarchyContext, ParsedProtocol, PrecinctJob, ValidationOutcome,
};

pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не удалось открыть SQLite {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch(include_str!("../migrations/001_initial.sql"))?;
        conn.execute_batch(include_str!("../migrations/002_phase_audit1.sql"))?;
        conn.execute_batch(include_str!("../migrations/003_phase_final.sql"))?;
        Ok(Self { conn })
    }

    pub fn save_election(&mut self, election: &Election) -> Result<()> {
        let now = now();
        let raw = serde_json::to_string(election)?;
        self.conn.execute(
            "INSERT INTO elections(id, external_id, vrn, name, voting_date, raw_json, retrieved_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET external_id=excluded.external_id, vrn=excluded.vrn,
               name=excluded.name, voting_date=excluded.voting_date, raw_json=excluded.raw_json,
               retrieved_at=excluded.retrieved_at",
            params![
                election.id,
                election.external_id,
                election.vrn,
                election.name,
                election.voting_date,
                raw,
                now
            ],
        )?;
        Ok(())
    }

    pub fn upsert_commission(
        &mut self,
        election_id: i64,
        node: &CommissionNode,
        parent_external_id: Option<&str>,
        context: &HierarchyContext,
        source_url: &str,
    ) -> Result<()> {
        let now = now();
        let raw = serde_json::to_string(node)?;
        self.conn.execute(
            "INSERT INTO commissions(external_id, internal_id, election_id, commission_type,
               number, name, parent_external_id, region_id, region_name, federal_district,
               has_children, source_url, raw_json, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(external_id) DO UPDATE SET internal_id=excluded.internal_id,
               election_id=excluded.election_id, commission_type=excluded.commission_type,
               number=excluded.number, name=excluded.name, parent_external_id=excluded.parent_external_id,
               region_id=excluded.region_id, region_name=excluded.region_name,
               federal_district=excluded.federal_district, has_children=excluded.has_children,
               source_url=excluded.source_url, raw_json=excluded.raw_json, updated_at=excluded.updated_at",
            params![
                node.external_id,
                node.id,
                election_id,
                node.commission_type,
                node.number,
                node.name,
                parent_external_id,
                context.region_id,
                context.region_name,
                context.federal_district,
                node.has_children,
                source_url,
                raw,
                now
            ],
        )?;
        if let Some(parent) = parent_external_id {
            self.conn.execute(
                "INSERT OR IGNORE INTO commission_relations(election_id, parent_external_id,
                   child_external_id, source_url) VALUES(?1, ?2, ?3, ?4)",
                params![election_id, parent, node.external_id, source_url],
            )?;
        }
        Ok(())
    }

    pub fn upsert_precinct_and_job(
        &mut self,
        election_id: i64,
        node: &CommissionNode,
        context: &HierarchyContext,
        source_url: &str,
    ) -> Result<()> {
        let category_hint = format!(
            "{} {} {}",
            node.name,
            context.tik_name.as_deref().unwrap_or_default(),
            context.region_name.as_deref().unwrap_or_default()
        )
        .to_lowercase()
        .replace('ё', "е");
        let category = if context.tik_name.as_deref() == Some("Территория за пределами РФ")
        {
            "foreign"
        } else if context.tik_name.as_deref() == Some("Город Байконур (Республика Казахстан)")
        {
            "baikonur"
        } else if category_hint.contains("дистанцион")
            || category_hint.contains("дег")
            || category_hint.contains("электронного голосования")
        {
            "deg"
        } else if context.federal_district.as_deref() == Some("Специальные и зарубежные")
        {
            "special"
        } else {
            "regular"
        };
        self.conn.execute(
            "INSERT INTO precincts(election_id, uik_id, uik_number, uik_name, region_id,
               region_name, federal_district, iksrf_id, iksrf_name, oik_id, oik_name,
               tik_id, tik_name, category, source_url)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(election_id, uik_id) DO UPDATE SET uik_number=excluded.uik_number,
               uik_name=excluded.uik_name, region_id=excluded.region_id,
               region_name=excluded.region_name, federal_district=excluded.federal_district,
               iksrf_id=excluded.iksrf_id, iksrf_name=excluded.iksrf_name,
               oik_id=excluded.oik_id, oik_name=excluded.oik_name,
               tik_id=excluded.tik_id, tik_name=excluded.tik_name,
               category=excluded.category, source_url=excluded.source_url",
            params![
                election_id,
                node.external_id,
                node.number,
                node.name,
                context.region_id,
                context.region_name,
                context.federal_district,
                context.iksrf_id,
                context.iksrf_name,
                context.oik_id,
                context.oik_name,
                context.tik_id,
                context.tik_name,
                category,
                source_url
            ],
        )?;
        self.conn.execute(
            "INSERT INTO download_jobs(election_id, uik_id, status, attempts, updated_at)
             VALUES(?1, ?2, 'pending', 0, ?3)
             ON CONFLICT(election_id, uik_id) DO NOTHING",
            params![election_id, node.external_id, now()],
        )?;
        Ok(())
    }

    pub fn reset_interrupted_jobs(&mut self, election_id: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE download_jobs SET status='pending', updated_at=?2
             WHERE election_id=?1 AND status='running'",
            params![election_id, now()],
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pending_jobs(
        &self,
        election_id: i64,
        federal_district: Option<&str>,
        region: Option<&str>,
        tik: Option<&str>,
        uik: Option<i64>,
        limit: Option<usize>,
        include_failed: bool,
    ) -> Result<Vec<PrecinctJob>> {
        let status = if include_failed {
            "('pending','failed')"
        } else {
            "('pending')"
        };
        let sql = format!(
            "SELECT j.election_id, j.uik_id, p.uik_number, p.region_name, p.tik_id
             FROM download_jobs j JOIN precincts p
               ON p.election_id=j.election_id AND p.uik_id=j.uik_id
             WHERE j.election_id=?1 AND j.status IN {status}
               AND (?2 IS NULL OR p.federal_district=?2)
               AND (?3 IS NULL OR p.region_name=?3 OR p.region_id=?3)
               AND (?4 IS NULL OR p.tik_id=?4 OR p.tik_name=?4)
               AND (?5 IS NULL OR p.uik_number=?5)
             ORDER BY p.region_id, p.tik_id, p.uik_number
             LIMIT ?6"
        );
        let max_rows = i64::try_from(limit.unwrap_or(usize::MAX)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![election_id, federal_district, region, tik, uik, max_rows],
            |row| {
                Ok(PrecinctJob {
                    election_id: row.get(0)?,
                    uik_id: row.get(1)?,
                    uik_number: row.get(2)?,
                    region_name: row.get(3)?,
                    tik_id: row.get(4)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn mark_running(&mut self, job: &PrecinctJob) -> Result<()> {
        self.conn.execute(
            "UPDATE download_jobs SET status='running', attempts=attempts+1,
             last_error=NULL, updated_at=?3 WHERE election_id=?1 AND uik_id=?2",
            params![job.election_id, job.uik_id, now()],
        )?;
        Ok(())
    }

    pub fn save_protocol(
        &mut self,
        job: &PrecinctJob,
        source_url: &str,
        raw: &str,
        parsed: &ParsedProtocol,
        validation: &ValidationOutcome,
    ) -> Result<()> {
        anyhow::ensure!(
            parsed.commission_id == job.uik_id,
            "ответ относится к УИК {}, но ожидалась УИК {}",
            parsed.commission_id,
            job.uik_id
        );
        let retrieved_at = now();
        let sha256 = hex::encode(Sha256::digest(raw.as_bytes()));
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO source_documents(source_url, source_record_id,
               document_type, retrieved_at, sha256, body_json)
             VALUES(?1, ?2, 'protocol_2_uik', ?3, ?4, ?5)",
            params![source_url, parsed.report_id, retrieved_at, sha256, raw],
        )?;
        let source_id: i64 = tx.query_row(
            "SELECT id FROM source_documents WHERE source_url=?1 AND sha256=?2",
            params![source_url, sha256],
            |row| row.get(0),
        )?;
        let status = if validation.strict_valid {
            "valid"
        } else {
            "invalid"
        };
        tx.execute(
            "INSERT INTO protocols(election_id, uik_id, protocol_num, status, is_strict_valid)
             VALUES(?1, ?2, 2, ?3, ?4)
             ON CONFLICT(election_id, uik_id, protocol_num) DO UPDATE SET
               status=excluded.status, is_strict_valid=excluded.is_strict_valid",
            params![job.election_id, job.uik_id, status, validation.strict_valid],
        )?;
        let protocol_id: i64 = tx.query_row(
            "SELECT id FROM protocols WHERE election_id=?1 AND uik_id=?2 AND protocol_num=2",
            params![job.election_id, job.uik_id],
            |row| row.get(0),
        )?;
        let version_label = if parsed.repeated {
            "repeated"
        } else {
            "current"
        };
        tx.execute(
            "INSERT INTO protocol_versions(protocol_id, source_document_id, official_protocol_id,
               official_report_id, version_label, signed, repeated, sign_datetime, published_at,
               retrieved_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(official_report_id) DO UPDATE SET source_document_id=excluded.source_document_id,
               signed=excluded.signed, repeated=excluded.repeated,
               sign_datetime=excluded.sign_datetime, published_at=excluded.published_at,
               retrieved_at=excluded.retrieved_at",
            params![
                protocol_id,
                source_id,
                parsed.official_protocol_id,
                parsed.report_id,
                version_label,
                parsed.signed,
                parsed.repeated,
                parsed.sign_datetime,
                parsed.source_created_at,
                retrieved_at
            ],
        )?;
        let version_id: i64 = tx.query_row(
            "SELECT id FROM protocol_versions WHERE official_report_id=?1",
            [&parsed.report_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO protocol_source_metadata(version_id,source_created_at,source_field_name)
             VALUES(?1,?2,'createdAt')
             ON CONFLICT(version_id) DO UPDATE SET source_created_at=excluded.source_created_at,
               source_field_name=excluded.source_field_name",
            params![version_id, parsed.source_created_at],
        )?;
        for value in parsed.lines.values() {
            tx.execute(
                "INSERT INTO protocol_values(version_id, line_number, value, raw_value,
                   official_text, parse_status, party_name)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(version_id, line_number) DO UPDATE SET value=excluded.value,
                   raw_value=excluded.raw_value, official_text=excluded.official_text,
                   parse_status=excluded.parse_status, party_name=excluded.party_name",
                params![
                    version_id,
                    value.line_number,
                    value.value,
                    value.raw_value,
                    value.official_text,
                    match &value.state {
                        crate::model::ValueState::Present => "present",
                        crate::model::ValueState::Missing => "missing",
                        crate::model::ValueState::ParseError => "parse_error",
                        crate::model::ValueState::Unpublished => "unpublished",
                        crate::model::ValueState::Untrusted => "untrusted",
                    },
                    value.party_name
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM validation_results WHERE version_id=?1",
            [version_id],
        )?;
        for issue in &validation.issues {
            tx.execute(
                "INSERT OR IGNORE INTO validation_results(version_id, severity, code, message)
                 VALUES(?1, ?2, ?3, ?4)",
                params![version_id, issue.severity, issue.code, issue.message],
            )?;
        }
        tx.execute(
            "UPDATE protocols SET current_version_id=?2, status=?3, is_strict_valid=?4 WHERE id=?1",
            params![protocol_id, version_id, status, validation.strict_valid],
        )?;
        tx.execute(
            "UPDATE download_jobs SET status='completed', last_error=NULL, updated_at=?3
             WHERE election_id=?1 AND uik_id=?2",
            params![job.election_id, job.uik_id, retrieved_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_aggregate_source(
        &mut self,
        election_id: i64,
        commission_id: &str,
        source_url: &str,
        official_report_id: &str,
        raw: &str,
    ) -> Result<i64> {
        let checked_at = now();
        let sha256 = hex::encode(Sha256::digest(raw.as_bytes()));
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO source_documents(source_url,source_record_id,document_type,retrieved_at,sha256,body_json)
             VALUES(?1,?2,'protocol_2_tik_aggregate',?3,?4,?5)",
            params![source_url, official_report_id, checked_at, sha256, raw],
        )?;
        let source_id = tx.query_row(
            "SELECT id FROM source_documents WHERE source_url=?1 AND sha256=?2",
            params![source_url, sha256],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO aggregate_sources(election_id,commission_id,source_document_id,official_report_id,checked_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(election_id,commission_id) DO UPDATE SET
               source_document_id=excluded.source_document_id,
               official_report_id=excluded.official_report_id,
               checked_at=excluded.checked_at",
            params![election_id, commission_id, source_id, official_report_id, checked_at],
        )?;
        tx.commit()?;
        Ok(source_id)
    }

    pub fn record_job_error(
        &mut self,
        job: &PrecinctJob,
        stage: &str,
        message: &str,
    ) -> Result<()> {
        let now = now();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE download_jobs SET status='failed', last_error=?3, updated_at=?4
             WHERE election_id=?1 AND uik_id=?2",
            params![job.election_id, job.uik_id, message, now],
        )?;
        tx.execute(
            "INSERT INTO download_errors(election_id, uik_id, stage, error_kind, message, occurred_at)
             VALUES(?1, ?2, ?3, 'runtime', ?4, ?5)",
            params![job.election_id, job.uik_id, stage, message, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self, table: &str) -> Result<i64> {
        let allowed = [
            "commissions",
            "precincts",
            "protocols",
            "protocol_versions",
            "download_errors",
        ];
        anyhow::ensure!(allowed.contains(&table), "недопустимая таблица");
        Ok(self
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?)
    }

    pub fn precinct_category(&self, election_id: i64, uik_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT category FROM precincts WHERE election_id=?1 AND uik_id=?2",
                params![election_id, uik_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    pub fn scalar_i64(&self, sql: &str) -> Result<i64> {
        Ok(self.conn.query_row(sql, [], |row| row.get(0))?)
    }

    pub fn election_external_id(&self, election_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT external_id FROM elections WHERE id=?1",
                [election_id],
                |row| row.get(0),
            )
            .optional()?)
    }
}

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

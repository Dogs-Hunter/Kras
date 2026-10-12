use std::{collections::BTreeMap, fs::File, path::Path};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{CAMPAIGN_ID, db::Database};

#[derive(Debug, Clone)]
struct ExportRow {
    election_id: i64,
    election_name: String,
    election_date: String,
    region_id: Option<String>,
    region_name: Option<String>,
    federal_district: Option<String>,
    iksrf_id: Option<String>,
    iksrf_name: Option<String>,
    oik_id: Option<String>,
    oik_name: Option<String>,
    tik_id: Option<String>,
    tik_name: Option<String>,
    uik_id: String,
    uik_number: Option<i64>,
    precinct_address: Option<String>,
    protocol_status: String,
    protocol_version: String,
    protocol_date: Option<String>,
    source_created_at: Option<String>,
    source_record_id: String,
    source_url: String,
    retrieved_at: String,
    category: String,
    lines: [i64; 22],
}

#[derive(Debug, Serialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    bytes: u64,
}

pub fn export_all(db: &Database, output_root: &Path) -> Result<serde_json::Value> {
    std::fs::create_dir_all(output_root)?;
    std::fs::create_dir_all(output_root.join("federal_districts"))?;
    std::fs::create_dir_all(output_root.join("special_precincts"))?;
    std::fs::create_dir_all(output_root.join("special_precincts/foreign"))?;
    std::fs::create_dir_all(output_root.join("special_precincts/baikonur"))?;
    std::fs::create_dir_all(output_root.join("deg"))?;

    let rows = load_rows(db)?;
    let all_path = output_root.join("all_uiks.csv");
    write_csv(&all_path, &rows)?;

    let mut groups: BTreeMap<(String, String, String, String), Vec<&ExportRow>> = BTreeMap::new();
    let mut special = Vec::new();
    let mut foreign = Vec::new();
    let mut baikonur = Vec::new();
    let mut deg = Vec::new();
    for row in &rows {
        let fd = row
            .federal_district
            .as_deref()
            .unwrap_or("Специальные и зарубежные");
        if row.category == "deg" {
            deg.push(row);
            continue;
        }
        if row.category == "foreign" {
            foreign.push(row);
        } else if row.category == "baikonur" {
            baikonur.push(row);
        }
        if row.category != "regular" || fd == "Специальные и зарубежные" {
            special.push(row);
            continue;
        }
        groups
            .entry((
                fd.to_owned(),
                row.region_name
                    .clone()
                    .unwrap_or_else(|| "Без субъекта".to_owned()),
                row.tik_name.clone().unwrap_or_else(|| "Без ТИК".to_owned()),
                row.tik_id.clone().unwrap_or_else(|| "unknown".to_owned()),
            ))
            .or_default()
            .push(row);
    }
    for ((district, region, tik_name, tik_id), grouped_rows) in groups {
        let directory = output_root
            .join("federal_districts")
            .join(sanitize_component(&district))
            .join(sanitize_component(&region));
        std::fs::create_dir_all(&directory)?;
        let filename = format!(
            "{}_{}.csv",
            sanitize_component(&tik_name),
            sanitize_component(&tik_id)
        );
        write_csv_refs(&directory.join(filename), &grouped_rows)?;
    }
    write_csv_refs(
        &output_root.join("special_precincts").join("all.csv"),
        &special,
    )?;
    write_csv_refs(
        &output_root.join("special_precincts/foreign/all.csv"),
        &foreign,
    )?;
    write_csv_refs(
        &output_root.join("special_precincts/baikonur/all.csv"),
        &baikonur,
    )?;
    write_csv_refs(&output_root.join("deg").join("all.csv"), &deg)?;
    export_commissions(db, &output_root.join("commissions.json"))?;
    export_missing(db, &output_root.join("missing_protocols.csv"))?;
    export_invalid(db, &output_root.join("invalid_protocols.csv"))?;
    export_discrepancies(db, &output_root.join("discrepancies.csv"))?;
    export_protocol_schema(db, &output_root.join("protocol_schema.json"))?;
    export_political_parties(db, &output_root.join("political_parties.json"))?;
    std::fs::write(
        output_root.join("README_DATASET.md"),
        include_str!("../docs/dataset_readme_template.md"),
    )?;

    let report = statistics(db)?;
    std::fs::write(
        output_root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let mut files = Vec::new();
    for path in collect_export_files(output_root)? {
        let relative = path
            .strip_prefix(output_root)?
            .to_string_lossy()
            .replace('\\', "/");
        files.push(ManifestFile {
            path: relative,
            sha256: file_sha256(&path)?,
            bytes: path.metadata()?.len(),
        });
    }
    let manifest = json!({
        "generated_at": Utc::now().to_rfc3339(),
        "program_version": env!("CARGO_PKG_VERSION"),
        "election_id": CAMPAIGN_ID,
        "record_count": rows.len(),
        "sources": [
            "http://www.izbirkom.ru/",
            "http://apps.cikrf.ru/service/ik-inp-service-pbcopy"
        ],
        "files": files,
    });
    std::fs::write(
        output_root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(report)
}

fn collect_export_files(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) != Some("manifest.json") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn load_rows(db: &Database) -> Result<Vec<ExportRow>> {
    let mut stmt = db.conn.prepare(
        "SELECT e.id, e.name, e.voting_date, p.region_id, p.region_name,
           p.federal_district, p.iksrf_id, p.iksrf_name, p.oik_id, p.oik_name,
           p.tik_id, p.tik_name, p.uik_id, p.uik_number, p.precinct_address,
           pr.status, pv.version_label, pv.sign_datetime,
           COALESCE(psm.source_created_at,pv.published_at),
           pv.official_report_id, sd.source_url, sd.retrieved_at, p.category, pv.id
         FROM protocols pr
         JOIN precincts p ON p.election_id=pr.election_id AND p.uik_id=pr.uik_id
         JOIN elections e ON e.id=pr.election_id
         JOIN protocol_versions pv ON pv.id=pr.current_version_id
         LEFT JOIN protocol_source_metadata psm ON psm.version_id=pv.id
         JOIN source_documents sd ON sd.id=pv.source_document_id
         WHERE pr.election_id=?1 AND pr.protocol_num=2 AND pr.is_strict_valid=1
         ORDER BY p.region_id, p.tik_id, p.uik_number, p.uik_id",
    )?;
    let base_rows = stmt.query_map([CAMPAIGN_ID], |row| {
        Ok((
            ExportRow {
                election_id: row.get(0)?,
                election_name: row.get(1)?,
                election_date: row.get(2)?,
                region_id: row.get(3)?,
                region_name: row.get(4)?,
                federal_district: row.get(5)?,
                iksrf_id: row.get(6)?,
                iksrf_name: row.get(7)?,
                oik_id: row.get(8)?,
                oik_name: row.get(9)?,
                tik_id: row.get(10)?,
                tik_name: row.get(11)?,
                uik_id: row.get(12)?,
                uik_number: row.get(13)?,
                precinct_address: row.get(14)?,
                protocol_status: row.get(15)?,
                protocol_version: row.get(16)?,
                protocol_date: row.get(17)?,
                source_created_at: row.get(18)?,
                source_record_id: row.get(19)?,
                source_url: row.get(20)?,
                retrieved_at: row.get(21)?,
                category: row.get(22)?,
                lines: [0; 22],
            },
            row.get::<_, i64>(23)?,
        ))
    })?;
    let mut value_stmt = db.conn.prepare(
        "SELECT line_number, value FROM protocol_values
         WHERE version_id=?1 AND line_number BETWEEN 1 AND 22 ORDER BY line_number",
    )?;
    let mut result = Vec::new();
    for base in base_rows {
        let (mut row, version_id) = base?;
        let values = value_stmt.query_map([version_id], |record| {
            Ok((record.get::<_, i64>(0)?, record.get::<_, Option<i64>>(1)?))
        })?;
        let mut found = 0;
        for value in values {
            let (line, value) = value?;
            if (1..=22).contains(&line)
                && let Some(value) = value
            {
                row.lines[usize::try_from(line - 1)?] = value;
                found += 1;
            }
        }
        anyhow::ensure!(
            found == 22,
            "строгий протокол {} потерял строки",
            row.uik_id
        );
        result.push(row);
    }
    Ok(result)
}

fn csv_header() -> Vec<String> {
    let mut header = vec![
        "election_id",
        "election_name",
        "election_date",
        "region_id",
        "region_name",
        "federal_district",
        "iksrf_id",
        "iksrf_name",
        "oik_id",
        "oik_name",
        "tik_id",
        "tik_name",
        "uik_id",
        "uik_number",
        "precinct_address",
        "protocol_status",
        "protocol_version",
        "protocol_date",
        "source_created_at",
        "source_record_id",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    header.extend((1..=22).map(|line| format!("line_{line:02}")));
    header.extend(
        ["source_url", "retrieved_at"]
            .into_iter()
            .map(str::to_owned),
    );
    header
}

fn csv_record(row: &ExportRow) -> Vec<String> {
    let mut record = vec![
        row.election_id.to_string(),
        row.election_name.clone(),
        row.election_date.clone(),
        row.region_id.clone().unwrap_or_default(),
        row.region_name.clone().unwrap_or_default(),
        row.federal_district.clone().unwrap_or_default(),
        row.iksrf_id.clone().unwrap_or_default(),
        row.iksrf_name.clone().unwrap_or_default(),
        row.oik_id.clone().unwrap_or_default(),
        row.oik_name.clone().unwrap_or_default(),
        row.tik_id.clone().unwrap_or_default(),
        row.tik_name.clone().unwrap_or_default(),
        row.uik_id.clone(),
        row.uik_number.map(|n| n.to_string()).unwrap_or_default(),
        row.precinct_address.clone().unwrap_or_default(),
        row.protocol_status.clone(),
        row.protocol_version.clone(),
        row.protocol_date.clone().unwrap_or_default(),
        row.source_created_at.clone().unwrap_or_default(),
        row.source_record_id.clone(),
    ];
    record.extend(row.lines.iter().map(ToString::to_string));
    record.push(row.source_url.clone());
    record.push(row.retrieved_at.clone());
    record
}

fn write_csv(path: &Path, rows: &[ExportRow]) -> Result<()> {
    let references = rows.iter().collect::<Vec<_>>();
    write_csv_refs(path, &references)
}

fn write_csv_refs(path: &Path, rows: &[&ExportRow]) -> Result<()> {
    let mut writer = csv::WriterBuilder::new().from_path(path)?;
    writer.write_record(csv_header())?;
    for row in rows {
        writer.write_record(csv_record(row))?;
    }
    writer.flush()?;
    Ok(())
}

fn export_commissions(db: &Database, path: &Path) -> Result<()> {
    let mut stmt = db.conn.prepare(
        "SELECT external_id, internal_id, commission_type, number, name,
           parent_external_id, region_id, region_name, federal_district, source_url
         FROM commissions WHERE election_id=?1 ORDER BY commission_type, region_id, number, name",
    )?;
    let rows = stmt.query_map([CAMPAIGN_ID], |row| {
        Ok(json!({
            "external_id": row.get::<_, String>(0)?,
            "internal_id": row.get::<_, Option<i64>>(1)?,
            "type": row.get::<_, i64>(2)?,
            "number": row.get::<_, Option<i64>>(3)?,
            "name": row.get::<_, String>(4)?,
            "parent_external_id": row.get::<_, Option<String>>(5)?,
            "region_id": row.get::<_, Option<String>>(6)?,
            "region_name": row.get::<_, Option<String>>(7)?,
            "federal_district": row.get::<_, Option<String>>(8)?,
            "source_url": row.get::<_, String>(9)?,
        }))
    })?;
    let values = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    std::fs::write(path, serde_json::to_vec_pretty(&values)?)?;
    Ok(())
}

fn export_missing(db: &Database, path: &Path) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "region_name",
        "tik_name",
        "uik_id",
        "uik_number",
        "status",
        "attempts",
        "last_error",
        "source_url",
        "http_status",
        "diagnostic_status",
        "diagnostic_basis",
        "checked_at",
    ])?;
    let mut stmt = db.conn.prepare(
        "SELECT p.region_name, p.tik_name, p.uik_id, p.uik_number, j.status, j.attempts, j.last_error,
                a.source_url,a.http_status,a.diagnostic_status,a.diagnostic_basis,a.checked_at
         FROM download_jobs j JOIN precincts p ON p.election_id=j.election_id AND p.uik_id=j.uik_id
         LEFT JOIN protocol_availability_checks a
           ON a.election_id=j.election_id AND a.uik_id=j.uik_id
         WHERE j.election_id=?1 AND j.status!='completed'
         ORDER BY p.region_id, p.tik_id, p.uik_number",
    )?;
    let rows = stmt.query_map([CAMPAIGN_ID], |row| {
        Ok([
            row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?.to_string(),
            row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(8)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            row.get::<_, Option<String>>(10)?.unwrap_or_default(),
            row.get::<_, Option<String>>(11)?.unwrap_or_default(),
        ])
    })?;
    for row in rows {
        writer.write_record(row?)?;
    }
    writer.flush()?;
    Ok(())
}

fn export_protocol_schema(db: &Database, path: &Path) -> Result<()> {
    let mut stmt = db.conn.prepare(
        "SELECT v.line_number, MIN(v.official_text), MIN(v.party_name)
         FROM protocol_values v JOIN protocol_versions pv ON pv.id=v.version_id
         JOIN protocols p ON p.id=pv.protocol_id
         WHERE p.election_id=?1 AND v.line_number BETWEEN 1 AND 22
         GROUP BY v.line_number ORDER BY v.line_number",
    )?;
    let lines = stmt
        .query_map([CAMPAIGN_ID], |row| {
            let line: i64 = row.get(0)?;
            Ok(json!({
                "line_number": line,
                "official_text": row.get::<_, String>(1)?,
                "kind": if line >= 13 { "party" } else { "control" },
                "party_name": row.get::<_, Option<String>>(2)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let value = json!({
        "election_id": CAMPAIGN_ID,
        "protocol_number": 2,
        "report_number": 242,
        "source_url": "http://apps.cikrf.ru/service/ik-inp-service-pbcopy/reports/242",
        "lines": lines,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&value)?)?;
    Ok(())
}

fn export_political_parties(db: &Database, path: &Path) -> Result<()> {
    let mut stmt = db.conn.prepare(
        "SELECT protocol_line,ballot_position,official_name,source_url
         FROM political_parties WHERE election_id=?1 ORDER BY protocol_line",
    )?;
    let rows = stmt
        .query_map([CAMPAIGN_ID], |row| {
            Ok(json!({
                "protocol_line": row.get::<_, i64>(0)?,
                "ballot_position": row.get::<_, Option<i64>>(1)?,
                "official_name": row.get::<_, String>(2)?,
                "source_url": row.get::<_, String>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    std::fs::write(path, serde_json::to_vec_pretty(&rows)?)?;
    Ok(())
}

fn export_invalid(db: &Database, path: &Path) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record(["uik_id", "uik_number", "region_name", "code", "message"])?;
    let mut stmt = db.conn.prepare(
        "SELECT p.uik_id, p.uik_number, p.region_name, vr.code, vr.message
         FROM protocols pr JOIN precincts p ON p.election_id=pr.election_id AND p.uik_id=pr.uik_id
         JOIN validation_results vr ON vr.version_id=pr.current_version_id
         WHERE pr.election_id=?1 AND pr.is_strict_valid=0 AND vr.severity='error'
         ORDER BY p.region_id, p.tik_id, p.uik_number, vr.code",
    )?;
    let rows = stmt.query_map([CAMPAIGN_ID], |row| {
        Ok([
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ])
    })?;
    for row in rows {
        writer.write_record(row?)?;
    }
    writer.flush()?;
    Ok(())
}

fn export_discrepancies(db: &Database, path: &Path) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "commission_id",
        "line_number",
        "precinct_count",
        "completed_count",
        "uik_sum",
        "official_value",
        "difference",
        "checked_at",
    ])?;
    let mut stmt = db.conn.prepare(
        "SELECT commission_id, line_number, precinct_count, completed_count,
           uik_sum, official_value, difference, checked_at
         FROM aggregate_checks WHERE election_id=?1 AND COALESCE(difference, 0) != 0
         ORDER BY commission_id, line_number",
    )?;
    let rows = stmt.query_map([CAMPAIGN_ID], |row| {
        Ok([
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.to_string(),
            row.get::<_, i64>(2)?.to_string(),
            row.get::<_, i64>(3)?.to_string(),
            row.get::<_, Option<i64>>(4)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, Option<i64>>(5)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, Option<i64>>(6)?
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.get::<_, String>(7)?,
        ])
    })?;
    for row in rows {
        writer.write_record(row?)?;
    }
    writer.flush()?;
    Ok(())
}

pub fn statistics(db: &Database) -> Result<serde_json::Value> {
    let value = |sql: &str| -> Result<i64> { Ok(db.conn.query_row(sql, [], |row| row.get(0))?) };
    let discovered_uiks = value("SELECT COUNT(*) FROM precincts")?;
    let valid = value("SELECT COUNT(*) FROM protocols WHERE is_strict_valid=1")?;
    let coverage = if discovered_uiks == 0 {
        0.0
    } else {
        valid as f64 * 100.0 / discovered_uiks as f64
    };
    let coverage_by_region = coverage_groups(db, "p.region_name", "p.region_id, p.region_name")?;
    let coverage_by_federal_district = coverage_groups(
        db,
        "CASE p.category WHEN 'foreign' THEN 'Зарубежные участки' WHEN 'baikonur' THEN 'Байконур' ELSE p.federal_district END",
        "CASE p.category WHEN 'foreign' THEN 'Зарубежные участки' WHEN 'baikonur' THEN 'Байконур' ELSE p.federal_district END",
    )?;
    let coverage_by_category = coverage_groups(db, "p.category", "p.category")?;
    Ok(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "election_id": CAMPAIGN_ID,
        "sqlite_integrity": db.integrity_check()?,
        "regions": value("SELECT COUNT(*) FROM commissions WHERE commission_type=2")?,
        "iksrf": value("SELECT COUNT(*) FROM commissions WHERE commission_type=2")?,
        "oik": value("SELECT COUNT(*) FROM commissions WHERE commission_type=3")?,
        "tik": value("SELECT COUNT(*) FROM commissions WHERE commission_type=4")?,
        "discovered_uiks": discovered_uiks,
        "received_protocols": value("SELECT COUNT(*) FROM protocols")?,
        "strict_valid_protocols": valid,
        "coverage_denominator": "discovered_uiks",
        "timestamp_semantics": {
            "election_date": "дата выборов",
            "protocol_date": "signDateTime: дата и время подписания протокола, если опубликованы",
            "source_created_at": "createdAt: дата и время генерации записи в официальной информационной системе, исходный ISO-8601 со смещением",
            "retrieved_at": "UTC-время получения ответа программой"
        },
        "missing_protocols": value("SELECT COUNT(*) FROM download_jobs WHERE status='pending'")?,
        "unavailable_protocols": value("SELECT COUNT(*) FROM download_jobs WHERE status='failed'")?,
        "invalid_protocols": value("SELECT COUNT(*) FROM protocols WHERE is_strict_valid=0")?,
        "incomplete_protocols": value("SELECT COUNT(DISTINCT version_id) FROM validation_results WHERE code IN ('missing_line','missing_value','parse_error','unpublished_value','untrusted_value')")?,
        "duplicate_current_protocols": value("SELECT COUNT(*) FROM (SELECT election_id,uik_id,COUNT(*) c FROM protocols GROUP BY election_id,uik_id HAVING c>1)")?,
        "multiple_versions": value("SELECT COUNT(*) FROM (SELECT protocol_id,COUNT(*) c FROM protocol_versions GROUP BY protocol_id HAVING c>1)")?,
        "official_repeated_protocols": value("SELECT COUNT(*) FROM protocol_versions pv JOIN protocols p ON p.current_version_id=pv.id WHERE pv.repeated=1")?,
        "uiks_with_multiple_saved_versions": value("SELECT COUNT(*) FROM (SELECT protocol_id,COUNT(*) c FROM protocol_versions GROUP BY protocol_id HAVING c>1)")?,
        "repeated_without_prior_saved_version": value("SELECT COUNT(*) FROM protocol_versions pv JOIN protocols p ON p.current_version_id=pv.id JOIN (SELECT protocol_id,COUNT(*) c FROM protocol_versions GROUP BY protocol_id) versions ON versions.protocol_id=p.id WHERE pv.repeated=1 AND versions.c=1")?,
        "foreign_precincts": value("SELECT COUNT(*) FROM precincts WHERE category='foreign'")?,
        "baikonur_precincts": value("SELECT COUNT(*) FROM precincts WHERE category='baikonur'")?,
        "unknown_geography_precincts": value("SELECT COUNT(*) FROM precincts WHERE federal_district IS NULL AND category NOT IN ('foreign','baikonur')")?,
        "aggregate_sources_saved": value("SELECT COUNT(*) FROM aggregate_sources")?,
        "arithmetic_errors": value("SELECT COUNT(DISTINCT version_id) FROM validation_results WHERE code LIKE 'control_relation_%'")?,
        "aggregate_discrepancies": value("SELECT COUNT(*) FROM aggregate_checks WHERE COALESCE(difference,0)!=0")?,
        "coverage_percent": coverage,
        "coverage_by_region": coverage_by_region,
        "coverage_by_federal_district": coverage_by_federal_district,
        "coverage_by_category": coverage_by_category,
        "download_errors": value("SELECT COUNT(*) FROM download_errors")?,
    }))
}

fn coverage_groups(
    db: &Database,
    label_expression: &str,
    group_expression: &str,
) -> Result<Vec<serde_json::Value>> {
    let sql = format!(
        "SELECT COALESCE({label_expression}, 'Без категории'), COUNT(*),
           SUM(CASE WHEN pr.is_strict_valid=1 THEN 1 ELSE 0 END)
         FROM precincts p
         LEFT JOIN protocols pr
           ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id AND pr.protocol_num=2
         GROUP BY {group_expression} ORDER BY {group_expression}"
    );
    let mut stmt = db.conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let discovered: i64 = row.get(1)?;
        let received: i64 = row.get(2)?;
        let percent = if discovered == 0 {
            0.0
        } else {
            received as f64 * 100.0 / discovered as f64
        };
        Ok(json!({
            "name": name,
            "discovered_uiks": discovered,
            "received_protocols": received,
            "missing_protocols": discovered - received,
            "coverage_percent": percent,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn sanitize_component(value: &str) -> String {
    let value = value.trim();
    let mut result = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || ch.is_control()
            || ch.is_whitespace()
        {
            result.push('_');
        } else {
            result.push(ch);
        }
    }
    let result = result.trim_matches(['.', '_']).to_owned();
    if result.is_empty() {
        "unnamed".to_owned()
    } else {
        result.chars().take(120).collect()
    }
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::sanitize_component;

    #[test]
    fn creates_safe_stable_filename() {
        assert_eq!(sanitize_component("ТИК: 1 / тест"), "ТИК__1___тест");
        assert_eq!(sanitize_component("..."), "unnamed");
    }
}

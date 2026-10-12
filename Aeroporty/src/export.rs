use std::{collections::BTreeMap, path::Path};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result, io_error},
    models::{
        Airport, AirportStatus, Dataset, GeographicIssue, QuarantineRecord, RejectedAirport,
        Report, Runway, RunwayKind,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Csv,
    Json,
    Toml,
    Yaml,
}

impl ExportFormat {
    pub const ALL: [Self; 4] = [Self::Csv, Self::Json, Self::Toml, Self::Yaml];
}

#[derive(Debug, Serialize, Deserialize)]
struct CsvRow {
    airport_id: u64,
    icao: String,
    latitude: f64,
    longitude: f64,
    status: AirportStatus,
    airport_type: String,
    country: String,
    elevation_ft: i32,
    timezone: String,
    runways_count: usize,
    airport_name: String,
    airport_provenance_json: String,
    airport_evidence_json: String,
    runway_id: String,
    runway_length_ft: f64,
    runway_surface: String,
    runway_le_ident: Option<String>,
    runway_he_ident: Option<String>,
    runway_closed: bool,
    runway_kind: RunwayKind,
    runway_provenance_json: String,
    runway_evidence_json: String,
}

pub fn export_dataset(dataset: &Dataset, output: &Path, formats: &[ExportFormat]) -> Result<()> {
    std::fs::create_dir_all(output).map_err(|e| io_error(output, e))?;
    for format in formats {
        match format {
            ExportFormat::Csv => write_csv(dataset, &output.join("airports.csv"))?,
            ExportFormat::Json => write_and_check_json(dataset, &output.join("airports.json"))?,
            ExportFormat::Toml => write_and_check_toml(dataset, &output.join("airports.toml"))?,
            ExportFormat::Yaml => write_and_check_yaml(dataset, &output.join("airports.yaml"))?,
        }
    }
    Ok(())
}

pub fn write_reports(
    report: &Report,
    rejected: &[RejectedAirport],
    quarantine: &[QuarantineRecord],
    geographic_issues: &[GeographicIssue],
    dataset: &Dataset,
    output: &Path,
) -> Result<()> {
    std::fs::create_dir_all(output).map_err(|e| io_error(output, e))?;
    write_atomic(
        &output.join("report.json"),
        &serde_json::to_vec_pretty(report)?,
    )?;
    let path = output.join("rejected.csv");
    let part = path.with_extension("csv.part");
    let mut writer = csv::Writer::from_path(&part)?;
    writer.write_record(["id", "icao", "name", "reasons"])?;
    for row in rejected {
        writer.write_record([
            row.id.map(|v| v.to_string()).unwrap_or_default(),
            row.icao.clone().unwrap_or_default(),
            row.name.clone(),
            row.reasons.join(" | "),
        ])?;
    }
    writer.flush().map_err(|e| io_error(&part, e))?;
    replace_file(&part, &path)?;
    write_csv_records(&output.join("quarantine.csv"), quarantine)?;
    write_csv_records(&output.join("geographic_issues.csv"), geographic_issues)?;
    let evidence = dataset.airports.iter().map(|airport| serde_json::json!({
        "icao": airport.icao,
        "airport": airport.evidence,
        "runways": airport.runways.iter().map(|runway| serde_json::json!({"id": runway.id, "evidence": runway.evidence})).collect::<Vec<_>>()
    })).collect::<Vec<_>>();
    write_atomic(
        &output.join("evidence.json"),
        &serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(())
}

fn write_csv_records<T: Serialize>(path: &Path, records: &[T]) -> Result<()> {
    let part = path.with_extension("csv.part");
    let mut writer = csv::Writer::from_path(&part)?;
    for record in records {
        writer.serialize(record)?;
    }
    writer.flush().map_err(|e| io_error(&part, e))?;
    replace_file(&part, path)
}

pub fn read_dataset_json(path: &Path) -> Result<Dataset> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn verify_exports(output: &Path) -> Result<Dataset> {
    let json = read_dataset_json(&output.join("airports.json"))?;
    let toml_text = read_text(&output.join("airports.toml"))?;
    let toml_dataset: Dataset = toml::from_str(&toml_text)?;
    let yaml_text = read_text(&output.join("airports.yaml"))?;
    let yaml_dataset: Dataset = serde_norway::from_str(&yaml_text)?;
    let csv_dataset = read_csv(&output.join("airports.csv"))?;
    if json != toml_dataset || json != yaml_dataset || json != csv_dataset {
        return Err(Error::Data(
            "экспорты CSV/JSON/TOML/YAML не эквивалентны".into(),
        ));
    }
    Ok(json)
}

fn write_and_check_json(dataset: &Dataset, path: &Path) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(dataset)?;
    let decoded: Dataset = serde_json::from_slice(&bytes)?;
    ensure_equal(dataset, &decoded, "JSON")?;
    write_atomic(path, &bytes)
}

fn write_and_check_toml(dataset: &Dataset, path: &Path) -> Result<()> {
    let text = toml::to_string_pretty(dataset)?;
    let decoded: Dataset = toml::from_str(&text)?;
    ensure_equal(dataset, &decoded, "TOML")?;
    write_atomic(path, text.as_bytes())
}

fn write_and_check_yaml(dataset: &Dataset, path: &Path) -> Result<()> {
    let text = serde_norway::to_string(dataset)?;
    let decoded: Dataset = serde_norway::from_str(&text)?;
    ensure_equal(dataset, &decoded, "YAML")?;
    write_atomic(path, text.as_bytes())
}

fn write_csv(dataset: &Dataset, path: &Path) -> Result<()> {
    let part = path.with_extension("csv.part");
    let mut writer = csv::Writer::from_path(&part)?;
    for airport in &dataset.airports {
        for runway in &airport.runways {
            writer.serialize(CsvRow {
                airport_id: airport.id,
                icao: airport.icao.clone(),
                latitude: airport.latitude,
                longitude: airport.longitude,
                status: airport.status,
                airport_type: airport.airport_type.clone(),
                country: airport.country.clone(),
                elevation_ft: airport.elevation_ft,
                timezone: airport.timezone.clone(),
                runways_count: airport.runways_count,
                airport_name: airport.name.clone(),
                airport_provenance_json: serde_json::to_string(&airport.provenance)?,
                airport_evidence_json: serde_json::to_string(&airport.evidence)?,
                runway_id: runway.id.clone(),
                runway_length_ft: runway.length_ft,
                runway_surface: runway.surface.clone(),
                runway_le_ident: runway.le_ident.clone(),
                runway_he_ident: runway.he_ident.clone(),
                runway_closed: runway.closed,
                runway_kind: runway.kind,
                runway_provenance_json: serde_json::to_string(&runway.provenance)?,
                runway_evidence_json: serde_json::to_string(&runway.evidence)?,
            })?;
        }
    }
    writer.flush().map_err(|e| io_error(&part, e))?;
    replace_file(&part, path)?;
    let decoded = read_csv(path)?;
    ensure_equal(dataset, &decoded, "CSV")
}

fn read_csv(path: &Path) -> Result<Dataset> {
    let mut reader = csv::Reader::from_path(path)?;
    let mut airports: BTreeMap<(String, u64), Airport> = BTreeMap::new();
    for row in reader.deserialize::<CsvRow>() {
        let row = row?;
        let airport_provenance = serde_json::from_str(&row.airport_provenance_json)?;
        let airport_evidence = serde_json::from_str(&row.airport_evidence_json)?;
        let runway_provenance = serde_json::from_str(&row.runway_provenance_json)?;
        let runway_evidence = serde_json::from_str(&row.runway_evidence_json)?;
        let key = (row.icao.clone(), row.airport_id);
        let runway = Runway {
            id: row.runway_id,
            length_ft: row.runway_length_ft,
            surface: row.runway_surface,
            le_ident: row.runway_le_ident,
            he_ident: row.runway_he_ident,
            closed: row.runway_closed,
            kind: row.runway_kind,
            provenance: runway_provenance,
            evidence: runway_evidence,
        };
        let airport = airports.entry(key).or_insert_with(|| Airport {
            icao: row.icao,
            latitude: row.latitude,
            longitude: row.longitude,
            status: row.status,
            airport_type: row.airport_type,
            country: row.country,
            elevation_ft: row.elevation_ft,
            timezone: row.timezone,
            runways_count: row.runways_count,
            id: row.airport_id,
            name: row.airport_name,
            runways: Vec::new(),
            provenance: airport_provenance,
            evidence: airport_evidence,
        });
        airport.runways.push(runway);
    }
    Ok(Dataset {
        airports: airports.into_values().collect(),
    })
}

fn ensure_equal(expected: &Dataset, actual: &Dataset, format: &str) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        let detail = expected
            .airports
            .iter()
            .zip(&actual.airports)
            .find(|(left, right)| left != right)
            .map(|(left, right)| {
                format!(
                    "; первое расхождение ICAO {}: до={left:?}, после={right:?}",
                    left.icao
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "; разное число записей: до={}, после={}",
                    expected.airports.len(),
                    actual.airports.len()
                )
            });
        Err(Error::Data(format!(
            "обратное чтение {format} изменило данные{detail}"
        )))
    }
}

fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| io_error(path, e))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let part = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|v| v.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&part, bytes).map_err(|e| io_error(&part, e))?;
    replace_file(&part, path)
}

fn replace_file(part: &Path, path: &Path) -> Result<()> {
    match std::fs::rename(part, path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) && path.exists() =>
        {
            std::fs::remove_file(path).map_err(|e| io_error(path, e))?;
            std::fs::rename(part, path).map_err(|e| io_error(path, e))
        }
        Err(error) => Err(io_error(path, error)),
    }
}

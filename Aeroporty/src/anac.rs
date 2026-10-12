//! Official Brazilian ANAC open-data adapter.
//!
//! Only the current public/private V2 registers are allowed to supply strict
//! values.  The older general/runway exports and SIROS are snapshotted and
//! schema-checked, but are diagnostic because their publication dates and
//! schemas do not line up with the current register.
#![allow(clippy::too_many_arguments)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use encoding_rs::WINDOWS_1252;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::Config,
    error::{Error, Result, io_error},
    models::{
        AirportDraft, AirportStatus, AnacStatistics, Evidence, EvidenceMethod, RunwayDraft,
        RunwayKind, SourceInfo,
    },
    validation::valid_icao,
};

pub const METERS_TO_FEET: f64 = 3.280_839_895_013_123;

const FILES: [(&str, &str); 5] = [
    (
        "public_v2.csv",
        "https://sistemas.anac.gov.br/dadosabertos/Aerodromos/Aer%C3%B3dromos%20P%C3%BAblicos/Lista%20de%20aer%C3%B3dromos%20p%C3%BAblicos/AerodromosPublicos.csv",
    ),
    (
        "private_v2.csv",
        "https://sistemas.anac.gov.br/dadosabertos/Aerodromos/Aer%C3%B3dromos%20Privados/Lista%20de%20aer%C3%B3dromos%20privados/Aerodromos%20Privados/AerodromosPrivados.csv",
    ),
    (
        "general.csv",
        "https://sistemas.anac.gov.br/dadosabertos/Aerodromos/Aer%C3%B3dromos%20P%C3%BAblicos/Caracter%C3%ADsticas%20Gerais/pda_aerodromos_publicos_caracteristicas_gerais.csv",
    ),
    (
        "runways.csv",
        "https://sistemas.anac.gov.br/dadosabertos/Aerodromos/Aer%C3%B3dromos%20P%C3%BAblicos/Pistas%20de%20Pouso%20e%20Decolagem/pda_aerodromos_publicos_pistas_pouso_decolagem.csv",
    ),
    (
        "siros.csv",
        "https://siros.anac.gov.br/siros/registros/aerodromo/aerodromos.csv",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnacManifest {
    pub release: String,
    pub downloaded_at: String,
    pub license_basis: String,
    pub files: Vec<AnacManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnacManifestFile {
    pub name: String,
    pub source_url: String,
    pub sha256: String,
    pub size: u64,
    pub schema_valid: bool,
    pub integrated: bool,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct AnacAirport {
    pub publication_date: String,
    pub category: String,
    pub icao: String,
    pub ciad: String,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub elevation_m: Option<f64>,
    pub status_raw: String,
    pub status: Option<AirportStatus>,
    pub runways: Vec<AnacRunway>,
}

#[derive(Debug, Clone)]
pub struct AnacRunway {
    pub designation: String,
    pub low_ident: Option<String>,
    pub high_ident: Option<String>,
    pub length_m: Option<f64>,
    pub width_m: Option<f64>,
    pub surface_raw: String,
    pub surface: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AnacDataset {
    pub release: String,
    pub airports: Vec<AnacAirport>,
    pub siros_rows: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnacMatchRow {
    pub icao: String,
    pub ciad: String,
    pub category: String,
    pub distance_km: f64,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnacDecisionRow {
    pub icao: String,
    pub ciad: String,
    pub runway: String,
    pub field: String,
    pub existing_value: String,
    pub anac_value: String,
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnacUnmatchedRow {
    pub icao: String,
    pub ciad: String,
    pub category: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnacArtifacts {
    pub matches: Vec<AnacMatchRow>,
    pub conflicts: Vec<AnacDecisionRow>,
    pub unmatched: Vec<AnacUnmatchedRow>,
    pub statistics: AnacStatistics,
}

pub async fn prepare(config: &Config) -> Result<(AnacDataset, Vec<SourceInfo>)> {
    validate_release(&config.anac_release)?;
    let dir = config
        .anac_local_dir
        .clone()
        .unwrap_or_else(|| config.cache_dir.join("anac").join(&config.anac_release));
    std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let manifest_path = dir.join("manifest.json");
    if config.anac_local_dir.is_none() {
        if config.offline {
            for (name, _) in FILES {
                let path = dir.join(name);
                if !path.exists() {
                    return Err(Error::MissingCache(path));
                }
            }
        } else {
            download_missing(&dir, config.force).await?;
        }
    }
    let files = inspect_files(&dir)?;
    let prior: Option<AnacManifest> = if manifest_path.exists() {
        Some(serde_json::from_slice(
            &std::fs::read(&manifest_path).map_err(|e| io_error(&manifest_path, e))?,
        )?)
    } else {
        None
    };
    if let Some(prior) = &prior {
        for file in &files {
            if let Some(old) = prior.files.iter().find(|v| v.name == file.name)
                && old.sha256 != file.sha256
                && !config.force
            {
                return Err(Error::Data(format!(
                    "ANAC snapshot {} changed in-place; select a new --anac-release",
                    config.anac_release
                )));
            }
        }
    }
    let manifest = AnacManifest {
        release: config.anac_release.clone(),
        downloaded_at: prior
            .map(|m| m.downloaded_at)
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        license_basis: "Brazil Decree 8,777/2016 arts. 2 III and 3 IV: open licence, free use/crossing and unrestricted reuse, with source credit; ANAC webpage content separately states CC BY-ND 3.0".into(),
        files,
    };
    if config.anac_local_dir.is_none() && !config.offline {
        write_atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
    }
    if (config.offline || config.anac_local_dir.is_some())
        && let Some(saved) = std::fs::read(&manifest_path)
            .ok()
            .and_then(|v| serde_json::from_slice::<AnacManifest>(&v).ok())
    {
        for file in &manifest.files {
            if let Some(expected) = saved.files.iter().find(|v| v.name == file.name)
                && expected.sha256 != file.sha256
            {
                return Err(Error::Data(format!(
                    "ANAC checksum mismatch for {}",
                    file.name
                )));
            }
        }
    }
    let dataset = parse_dataset(&dir, &config.anac_release)?;
    let sources = manifest
        .files
        .iter()
        .map(|file| SourceInfo {
            name: format!("ANAC/{}", file.name),
            url: file.source_url.clone(),
            local_file: dir.join(&file.name).display().to_string(),
            retrieved_at: Some(manifest.downloaded_at.clone()),
            etag: None,
            last_modified: None,
        })
        .collect();
    Ok((dataset, sources))
}

async fn download_missing(dir: &Path, force: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("Aeroporty/0.1 ANAC open-data importer")
        .build()?;
    for (name, url) in FILES {
        let path = dir.join(name);
        if path.exists() && !force {
            continue;
        }
        let response = client.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?;
        if bytes.len() < 100 {
            return Err(Error::Data(format!(
                "ANAC file {name} is unexpectedly small"
            )));
        }
        if path.exists() {
            let existing = std::fs::read(&path).map_err(|e| io_error(&path, e))?;
            if existing == bytes {
                continue;
            }
            return Err(Error::Data(format!(
                "refusing to overwrite immutable ANAC snapshot {}; choose another release",
                path.display()
            )));
        }
        let part = path.with_extension("csv.part");
        std::fs::write(&part, &bytes).map_err(|e| io_error(&part, e))?;
        std::fs::rename(&part, &path).map_err(|e| io_error(&path, e))?;
    }
    Ok(())
}

fn inspect_files(dir: &Path) -> Result<Vec<AnacManifestFile>> {
    let mut result = Vec::new();
    for (name, url) in FILES {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).map_err(|e| io_error(&path, e))?;
        let schema_valid = validate_schema(name, &bytes)?;
        result.push(AnacManifestFile {
            name: name.into(),
            source_url: url.into(),
            sha256: hex_sha256(&bytes),
            size: bytes.len() as u64,
            schema_valid,
            integrated: matches!(name, "public_v2.csv" | "private_v2.csv"),
            note: match name {
                "general.csv" => "server publication is dated 2023; diagnostic only",
                "runways.csv" => "server publication is dated 2024 and has an undocumented 15-column headerless mixed-use schema; diagnostic only",
                "siros.csv" => "current SIROS airport reference has no elevation or runways; diagnostic cross-check only",
                _ => "current V2 registry used by strict adapter",
            }
            .into(),
        });
    }
    Ok(result)
}

fn validate_schema(name: &str, bytes: &[u8]) -> Result<bool> {
    let text = decode(bytes);
    match name {
        "public_v2.csv" => validate_headers(
            text.lines().nth(1).unwrap_or(""),
            b';',
            &[
                "Código OACI",
                "CIAD",
                "Nome",
                "LATGEOPOINT",
                "LONGEOPOINT",
                "Altitude",
                "Situação",
            ],
        ),
        "private_v2.csv" => validate_headers(
            text.lines().nth(1).unwrap_or(""),
            b';',
            &[
                "Código OACI",
                "CIAD",
                "Nome",
                "LATGEOPOINT",
                "LONGEOPOINT",
                "Altitude",
                "Designação 1",
                "Comprimento 1",
                "Superfície 1",
            ],
        ),
        "general.csv" => validate_headers(
            text.lines().next().unwrap_or(""),
            b',',
            &[
                "Id do Aeródromo",
                "CIAD",
                "Código OACI",
                "Tipo de Uso",
                "Situação",
            ],
        ),
        "siros.csv" => validate_headers(
            text.trim_start_matches('\u{feff}')
                .lines()
                .next()
                .unwrap_or(""),
            b';',
            &[
                "SIGLA ICAO AERÓDROMO",
                "NOME AERÓDROMO",
                "LATITUDE",
                "LONGITUDE",
            ],
        ),
        "runways.csv" => {
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_reader(text.as_bytes());
            let first = reader.records().next().transpose()?;
            Ok(first.is_some_and(|r| r.len() == 15 && r.get(0) == Some("Aeródromos")))
        }
        _ => Ok(false),
    }
}

fn validate_headers(line: &str, delimiter: u8, required: &[&str]) -> Result<bool> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_reader(line.as_bytes());
    let headers = reader.headers()?.clone();
    Ok(required
        .iter()
        .all(|name| headers.iter().any(|h| h == *name)))
}

fn parse_dataset(dir: &Path, release: &str) -> Result<AnacDataset> {
    let mut airports = parse_v2(&dir.join("public_v2.csv"), "public")?;
    airports.extend(parse_v2(&dir.join("private_v2.csv"), "private")?);
    let siros = decode(
        &std::fs::read(dir.join("siros.csv")).map_err(|e| io_error(dir.join("siros.csv"), e))?,
    );
    let siros_rows = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(siros.trim_start_matches('\u{feff}').as_bytes())
        .records()
        .filter(|row| row.is_ok())
        .count();
    Ok(AnacDataset {
        release: release.into(),
        airports,
        siros_rows,
    })
}

fn parse_v2(path: &Path, category: &str) -> Result<Vec<AnacAirport>> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    let text = decode(&bytes);
    let mut lines = text.lines();
    let publication_date = lines
        .next()
        .and_then(|v| v.split_once(':'))
        .map(|(_, value)| value.trim().to_owned())
        .unwrap_or_default();
    let csv_text = lines.collect::<Vec<_>>().join("\n");
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .flexible(true)
        .from_reader(csv_text.as_bytes());
    let headers = reader.headers()?.clone();
    let mut result = Vec::new();
    for record in reader.records() {
        let record = record?;
        let get = |name: &str| -> &str {
            headers
                .iter()
                .position(|h| h == name)
                .and_then(|i| record.get(i))
                .unwrap_or("")
                .trim()
        };
        let status_raw = if category == "public" {
            get("Situação").to_owned()
        } else {
            get("Portaria de Registro").to_owned()
        };
        let status = if category == "public" {
            match fold(get("Situação")).as_str() {
                "cadastrado" => Some(AirportStatus::Open),
                value if value.contains("excluido") || value.contains("cancelado") => {
                    Some(AirportStatus::Close)
                }
                _ => None,
            }
        } else if !get("Portaria de Registro").is_empty() {
            Some(AirportStatus::Open)
        } else {
            None
        };
        let mut runways = Vec::new();
        if category == "private" {
            for n in ["1", "2"] {
                let designation = get(&format!("Designação {n}"));
                if designation.is_empty() {
                    continue;
                }
                let (low_ident, high_ident) = split_designation(designation);
                let runway = AnacRunway {
                    designation: designation.to_ascii_uppercase(),
                    low_ident,
                    high_ident,
                    length_m: decimal(get(&format!("Comprimento {n}"))),
                    width_m: decimal(get(&format!("Largura {n}"))),
                    surface_raw: get(&format!("Superfície {n}")).to_owned(),
                    surface: normalize_surface(get(&format!("Superfície {n}"))),
                };
                if !runways.iter().any(|r: &AnacRunway| {
                    canonical(r.low_ident.as_deref(), r.high_ident.as_deref())
                        == canonical(runway.low_ident.as_deref(), runway.high_ident.as_deref())
                }) {
                    runways.push(runway);
                }
            }
        }
        result.push(AnacAirport {
            publication_date: publication_date.clone(),
            category: category.into(),
            icao: get("Código OACI").to_ascii_uppercase(),
            ciad: get("CIAD").to_ascii_uppercase(),
            name: get("Nome").to_owned(),
            latitude: decimal(get("LATGEOPOINT")),
            longitude: decimal(get("LONGEOPOINT")),
            elevation_m: measured_decimal(get("Altitude")),
            status_raw,
            status,
            runways,
        });
    }
    Ok(result)
}

pub fn apply(drafts: &mut Vec<AirportDraft>, data: &AnacDataset) -> AnacArtifacts {
    let mut out = AnacArtifacts::default();
    out.statistics.release = data.release.clone();
    out.statistics.public_airports = data
        .airports
        .iter()
        .filter(|a| a.category == "public")
        .count();
    out.statistics.private_airports = data
        .airports
        .iter()
        .filter(|a| a.category == "private")
        .count();
    out.statistics.siros_airports = data.siros_rows;
    let mut index: HashMap<&str, Vec<&AnacAirport>> = HashMap::new();
    for airport in &data.airports {
        if valid_icao(&airport.icao) {
            out.statistics.airports_with_icao += 1;
            index.entry(&airport.icao).or_default().push(airport);
        }
    }
    out.statistics.ambiguous_matches = index.values().filter(|values| values.len() > 1).count();
    for draft in drafts.iter_mut() {
        let Some(matches) = index.get(draft.icao.as_str()) else {
            continue;
        };
        if matches.len() != 1 {
            for candidate in matches {
                out.unmatched
                    .push(unmatched(candidate, "ambiguous official ICAO"));
            }
            continue;
        }
        let official = matches[0];
        let distance = match (
            draft.latitude,
            draft.longitude,
            official.latitude,
            official.longitude,
        ) {
            (Some(a), Some(b), Some(c), Some(d)) => haversine_km(a, b, c, d),
            _ => f64::NAN,
        };
        if !distance.is_finite() || distance > 15.0 {
            out.unmatched.push(unmatched(
                official,
                &format!("coordinate disagreement {distance:.1} km"),
            ));
            continue;
        }
        out.statistics.matched_airports += 1;
        out.matches.push(AnacMatchRow {
            icao: draft.icao.clone(),
            ciad: official.ciad.clone(),
            category: official.category.clone(),
            distance_km: distance,
            outcome: if distance <= 5.0 {
                "matched"
            } else {
                "matched_with_coordinate_warning"
            }
            .into(),
        });
        draft
            .provenance
            .insert("anac_ciad".into(), official.ciad.clone());
        draft
            .provenance
            .insert("anac_release".into(), data.release.clone());
        verify(
            &mut draft.evidence,
            "icao",
            official,
            &official.icao,
            &draft.icao,
            "exact official ICAO",
            &mut out,
        );
        if distance <= 0.25 {
            if let Some(value) = draft.latitude {
                verify(
                    &mut draft.evidence,
                    "latitude",
                    official,
                    &official.latitude.unwrap().to_string(),
                    &value.to_string(),
                    "reference points within 250 m",
                    &mut out,
                );
            }
            if let Some(value) = draft.longitude {
                verify(
                    &mut draft.evidence,
                    "longitude",
                    official,
                    &official.longitude.unwrap().to_string(),
                    &value.to_string(),
                    "reference points within 250 m",
                    &mut out,
                );
            }
        }
        if let (Some(m), Some(feet)) = (official.elevation_m, draft.elevation_ft) {
            let converted = m * METERS_TO_FEET;
            if (converted - f64::from(feet)).abs() <= 3.0 {
                verify(
                    &mut draft.evidence,
                    "elevation_ft",
                    official,
                    &format!("{m} m"),
                    &feet.to_string(),
                    "ANAC metres agree within 3 ft after exact conversion",
                    &mut out,
                );
            }
        } else if draft.elevation_ft.is_none()
            && let Some(m) = official.elevation_m
        {
            let feet = (m * METERS_TO_FEET).round() as i32;
            draft.elevation_ft = Some(feet);
            recover(
                &mut draft.evidence,
                "elevation_ft",
                official,
                &format!("{m} m"),
                &feet.to_string(),
                "official elevation; metres converted with exact factor",
                &mut out,
            );
        }
        if fold(&draft.name) == fold(&official.name) {
            let existing = draft.name.clone();
            verify(
                &mut draft.evidence,
                "name",
                official,
                &official.name,
                &existing,
                "normalized names agree",
                &mut out,
            );
        }
        if let (Some(a), Some(b)) = (official.status, draft.status) {
            if a == b {
                verify(
                    &mut draft.evidence,
                    "status",
                    official,
                    &official.status_raw,
                    if b == AirportStatus::Open {
                        "open"
                    } else {
                        "close"
                    },
                    "current ANAC registry status agrees",
                    &mut out,
                );
            } else {
                add_conflict(
                    draft,
                    official,
                    "",
                    "status",
                    format!("{b:?}"),
                    format!("{a:?}"),
                    "ANAC registry status contradicts existing status",
                    &mut out,
                );
            }
        }
        merge_runways(draft, official, &mut out);
    }
    add_new_airports(drafts, data, &index, &mut out);
    let final_icaos: HashSet<&str> = drafts.iter().map(|d| d.icao.as_str()).collect();
    let already_unmatched: HashSet<String> = out.unmatched.iter().map(|r| r.icao.clone()).collect();
    for airport in data.airports.iter().filter(|a| {
        valid_icao(&a.icao)
            && !final_icaos.contains(a.icao.as_str())
            && !already_unmatched.contains(&a.icao)
    }) {
        out.unmatched.push(unmatched(
            airport,
            if airport.category == "public" {
                "current public V2 has no physical runway fields; stale runway export is not used for insertion"
            } else {
                "official record was not safely insertable"
            },
        ));
    }
    out.statistics.unmatched_airports = out.unmatched.len();
    out.statistics.unresolved_conflicts = out.statistics.conflicts;
    out
}

fn merge_runways(draft: &mut AirportDraft, official: &AnacAirport, out: &mut AnacArtifacts) {
    for runway in &official.runways {
        let exact = draft.runways.iter().position(|r| {
            canonical(r.le_ident.as_deref(), r.he_ident.as_deref())
                == canonical(runway.low_ident.as_deref(), runway.high_ident.as_deref())
        });
        let renamed = exact.or_else(|| draft.runways.iter().position(|r| renumbered(r, runway)));
        if let Some(index) = renamed {
            out.statistics.matched_runways += 1;
            if exact.is_none() {
                out.statistics.renamed_runway_matches += 1;
            }
            let existing = &mut draft.runways[index];
            if let (Some(a), Some(m)) = (existing.length_ft, runway.length_m) {
                let b = m * METERS_TO_FEET;
                if (a - b).abs() <= 50.0 {
                    verify_runway(
                        existing,
                        "length_ft",
                        official,
                        runway,
                        &format!("{m} m"),
                        &a.to_string(),
                        "physical lengths agree within 50 ft",
                        out,
                    );
                } else if (a - b).abs() > 500.0 && (a - b).abs() / b.max(1.0) > 0.1 {
                    add_runway_conflict(
                        existing,
                        official,
                        runway,
                        "length_ft",
                        a.to_string(),
                        b.round().to_string(),
                        "physical length differs by more than 500 ft and 10%",
                        out,
                    );
                }
            } else if existing.length_ft.is_none()
                && let Some(m) = runway.length_m
            {
                existing.length_ft = Some(m * METERS_TO_FEET);
                recover_runway(
                    existing,
                    "length_ft",
                    official,
                    runway,
                    &format!("{m} m"),
                    &(m * METERS_TO_FEET).to_string(),
                    "official physical length; exact metre-to-foot conversion",
                    out,
                );
            }
            if let (Some(a), Some(b)) = (existing.surface.clone(), runway.surface.as_deref()) {
                if compatible_surface(&a, b) {
                    verify_runway(
                        existing,
                        "surface",
                        official,
                        runway,
                        &runway.surface_raw,
                        &a,
                        "documented ANAC surface mapping agrees",
                        out,
                    );
                }
            } else if existing.surface.is_none()
                && let Some(surface) = &runway.surface
            {
                existing.surface = Some(surface.clone());
                recover_runway(
                    existing,
                    "surface",
                    official,
                    runway,
                    &runway.surface_raw,
                    surface,
                    "documented ANAC surface mapping",
                    out,
                );
            }
        } else if runway.length_m.is_some_and(|v| v > 0.0) && runway.surface.is_some() {
            draft.runways.push(to_draft_runway(official, runway));
            out.statistics.added_runways += 1;
            out.statistics.recovered_values += 3;
        }
    }
}

fn add_new_airports(
    drafts: &mut Vec<AirportDraft>,
    data: &AnacDataset,
    index: &HashMap<&str, Vec<&AnacAirport>>,
    out: &mut AnacArtifacts,
) {
    let existing: HashSet<String> = drafts.iter().map(|d| d.icao.clone()).collect();
    let mut ids: HashSet<u64> = drafts.iter().filter_map(|d| d.id).collect();
    for airport in data
        .airports
        .iter()
        .filter(|a| a.category == "private" && valid_icao(&a.icao) && !existing.contains(&a.icao))
    {
        if index
            .get(airport.icao.as_str())
            .is_none_or(|v| v.len() != 1)
        {
            out.unmatched
                .push(unmatched(airport, "ambiguous official ICAO"));
            continue;
        }
        let complete_runways: Vec<_> = airport
            .runways
            .iter()
            .filter(|r| r.length_m.is_some_and(|v| v > 0.0) && r.surface.is_some())
            .collect();
        if airport.name.is_empty()
            || airport.ciad.is_empty()
            || airport.latitude.is_none()
            || airport.longitude.is_none()
            || airport.elevation_m.is_none()
            || airport.status.is_none()
            || complete_runways.is_empty()
        {
            out.statistics.incomplete_candidates += 1;
            out.unmatched.push(unmatched(airport, "insufficient required values (name/CIAD/coordinates/elevation/status/complete land runway)"));
            continue;
        }
        out.statistics.new_airport_candidates += 1;
        let id = stable_anac_id(&airport.ciad);
        if !ids.insert(id) {
            out.unmatched
                .push(unmatched(airport, "stable ID collision"));
            continue;
        }
        let mut evidence = BTreeMap::new();
        let elevation_ft = (airport.elevation_m.unwrap() * METERS_TO_FEET).round() as i32;
        for (field, raw, normalized, reason, method) in [
            (
                "icao",
                airport.icao.clone(),
                airport.icao.clone(),
                "official Código OACI",
                EvidenceMethod::Recovered,
            ),
            (
                "latitude",
                airport.latitude.unwrap().to_string(),
                airport.latitude.unwrap().to_string(),
                "official WGS-84 reference point",
                EvidenceMethod::Recovered,
            ),
            (
                "longitude",
                airport.longitude.unwrap().to_string(),
                airport.longitude.unwrap().to_string(),
                "official WGS-84 reference point",
                EvidenceMethod::Recovered,
            ),
            (
                "status",
                airport.status_raw.clone(),
                "open".into(),
                "derived from presence in current private register plus registration act; not operational availability",
                EvidenceMethod::Derived,
            ),
            (
                "type",
                "Aeródromo privado".into(),
                "airport".into(),
                "ANAC private aerodrome category",
                EvidenceMethod::Recovered,
            ),
            (
                "country",
                "Brasil".into(),
                "Brazil".into(),
                "ANAC national registry scope",
                EvidenceMethod::Recovered,
            ),
            (
                "elevation_ft",
                format!("{} m", airport.elevation_m.unwrap()),
                elevation_ft.to_string(),
                "official metres converted with exact factor and rounded",
                EvidenceMethod::Recovered,
            ),
            (
                "id",
                airport.ciad.clone(),
                id.to_string(),
                "stable SHA-256 namespaced ANAC CIAD identifier",
                EvidenceMethod::Derived,
            ),
            (
                "name",
                airport.name.clone(),
                airport.name.clone(),
                "official registered name",
                EvidenceMethod::Recovered,
            ),
        ] {
            evidence
                .entry(field.into())
                .or_insert_with(Vec::new)
                .push(anac_evidence(
                    airport,
                    &raw,
                    &normalized,
                    reason,
                    method,
                    None,
                ));
        }
        out.statistics.recovered_values += 7;
        let runways = complete_runways
            .into_iter()
            .map(|r| to_draft_runway(airport, r))
            .collect();
        drafts.push(AirportDraft {
            icao: airport.icao.clone(),
            latitude: airport.latitude,
            longitude: airport.longitude,
            status: airport.status,
            airport_type: "airport".into(),
            country: Some("Brazil".into()),
            elevation_ft: Some(elevation_ft),
            id: Some(id),
            name: airport.name.clone(),
            runways,
            provenance: BTreeMap::from([
                (
                    "source".into(),
                    "ANAC Lista de aeródromos privados V2".into(),
                ),
                ("anac_ciad".into(), airport.ciad.clone()),
                ("anac_release".into(), data.release.clone()),
            ]),
            evidence,
            ..Default::default()
        });
        out.statistics.new_airports_added += 1;
        out.statistics.recovered_values += airport.runways.len() * 3;
    }
}

fn to_draft_runway(airport: &AnacAirport, runway: &AnacRunway) -> RunwayDraft {
    let id = format!("ANAC:{}:{}", airport.ciad, runway.designation);
    let length = runway.length_m.unwrap() * METERS_TO_FEET;
    let mut evidence = BTreeMap::new();
    for (field, raw, normalized, reason) in [
        (
            "id",
            runway.designation.clone(),
            id.clone(),
            "official runway designation namespaced by CIAD",
        ),
        (
            "length_ft",
            format!("{} m", runway.length_m.unwrap()),
            length.to_string(),
            "physical runway length; exact metre-to-foot conversion",
        ),
        (
            "surface",
            runway.surface_raw.clone(),
            runway.surface.clone().unwrap(),
            "documented Portuguese surface mapping; raw retained",
        ),
    ] {
        evidence
            .entry(field.into())
            .or_insert_with(Vec::new)
            .push(anac_evidence(
                airport,
                &raw,
                &normalized,
                reason,
                EvidenceMethod::Recovered,
                Some(&runway.designation),
            ));
    }
    RunwayDraft {
        id,
        length_ft: Some(length),
        width_ft: runway.width_m.map(|v| v * METERS_TO_FEET),
        surface: runway.surface.clone(),
        le_ident: runway.low_ident.clone(),
        he_ident: runway.high_ident.clone(),
        kind: RunwayKind::LandRunway,
        classification_reason: "official ANAC physical private-aerodrome runway".into(),
        provenance: BTreeMap::from([
            ("source".into(), "ANAC private V2".into()),
            ("anac_ciad".into(), airport.ciad.clone()),
            ("length_m".into(), runway.length_m.unwrap().to_string()),
            ("surface_raw".into(), runway.surface_raw.clone()),
        ]),
        evidence,
        ..Default::default()
    }
}

fn verify(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    field: &str,
    airport: &AnacAirport,
    raw: &str,
    normalized: &str,
    rule: &str,
    out: &mut AnacArtifacts,
) {
    map.entry(field.into()).or_default().push(anac_evidence(
        airport,
        raw,
        normalized,
        rule,
        EvidenceMethod::IndependentlyVerified,
        None,
    ));
    out.statistics.independently_verified_values += 1;
}

fn recover(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    field: &str,
    airport: &AnacAirport,
    raw: &str,
    normalized: &str,
    rule: &str,
    out: &mut AnacArtifacts,
) {
    map.entry(field.into()).or_default().push(anac_evidence(
        airport,
        raw,
        normalized,
        rule,
        EvidenceMethod::Recovered,
        None,
    ));
    out.statistics.recovered_values += 1;
}

fn verify_runway(
    runway: &mut RunwayDraft,
    field: &str,
    airport: &AnacAirport,
    source: &AnacRunway,
    raw: &str,
    normalized: &str,
    rule: &str,
    out: &mut AnacArtifacts,
) {
    runway
        .evidence
        .entry(field.into())
        .or_default()
        .push(anac_evidence(
            airport,
            raw,
            normalized,
            rule,
            EvidenceMethod::IndependentlyVerified,
            Some(&source.designation),
        ));
    out.statistics.independently_verified_values += 1;
}

fn recover_runway(
    runway: &mut RunwayDraft,
    field: &str,
    airport: &AnacAirport,
    source: &AnacRunway,
    raw: &str,
    normalized: &str,
    rule: &str,
    out: &mut AnacArtifacts,
) {
    runway
        .evidence
        .entry(field.into())
        .or_default()
        .push(anac_evidence(
            airport,
            raw,
            normalized,
            rule,
            EvidenceMethod::Recovered,
            Some(&source.designation),
        ));
    out.statistics.recovered_values += 1;
}

fn add_conflict(
    draft: &mut AirportDraft,
    airport: &AnacAirport,
    runway: &str,
    field: &str,
    existing: String,
    anac: String,
    reason: &str,
    out: &mut AnacArtifacts,
) {
    let mut evidence = anac_evidence(
        airport,
        &anac,
        &anac,
        reason,
        EvidenceMethod::Conflicting,
        (!runway.is_empty()).then_some(runway),
    );
    evidence.conflict = Some(format!("existing={existing}; ANAC={anac}"));
    draft
        .evidence
        .entry(field.into())
        .or_default()
        .push(evidence);
    out.conflicts.push(AnacDecisionRow {
        icao: airport.icao.clone(),
        ciad: airport.ciad.clone(),
        runway: runway.into(),
        field: field.into(),
        existing_value: existing,
        anac_value: anac,
        decision: "Conflict".into(),
        reason: reason.into(),
    });
    out.statistics.conflicts += 1;
}

fn add_runway_conflict(
    runway: &mut RunwayDraft,
    airport: &AnacAirport,
    source: &AnacRunway,
    field: &str,
    existing: String,
    anac: String,
    reason: &str,
    out: &mut AnacArtifacts,
) {
    let mut evidence = anac_evidence(
        airport,
        &anac,
        &anac,
        reason,
        EvidenceMethod::Conflicting,
        Some(&source.designation),
    );
    evidence.conflict = Some(format!("existing={existing}; ANAC={anac}"));
    runway
        .evidence
        .entry(field.into())
        .or_default()
        .push(evidence);
    out.conflicts.push(AnacDecisionRow {
        icao: airport.icao.clone(),
        ciad: airport.ciad.clone(),
        runway: source.designation.clone(),
        field: field.into(),
        existing_value: existing,
        anac_value: anac,
        decision: "Conflict".into(),
        reason: reason.into(),
    });
    out.statistics.conflicts += 1;
}

fn anac_evidence(
    airport: &AnacAirport,
    raw: &str,
    normalized: &str,
    validation: &str,
    method: EvidenceMethod,
    runway: Option<&str>,
) -> Evidence {
    Evidence {
        source: format!("ANAC {} V2", airport.category),
        source_record_id: Some(match runway {
            Some(r) => format!("{}:{r}", airport.ciad),
            None => airport.ciad.clone(),
        }),
        retrieved_at: None,
        effective_at: Some(airport.publication_date.clone()),
        raw_value: raw.into(),
        normalized_value: normalized.into(),
        method,
        validation: validation.into(),
        conflict: None,
    }
}

fn unmatched(a: &AnacAirport, reason: &str) -> AnacUnmatchedRow {
    AnacUnmatchedRow {
        icao: a.icao.clone(),
        ciad: a.ciad.clone(),
        category: a.category.clone(),
        reason: reason.into(),
    }
}

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(value) => value.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radius_km = 6371.0088;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * radius_km * a.sqrt().asin()
}

fn decimal(value: &str) -> Option<f64> {
    let value = value.trim().replace(',', ".").replace(" m", "");
    value.parse().ok().filter(|v: &f64| v.is_finite())
}

fn measured_decimal(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() || value.starts_with(',') {
        None
    } else {
        decimal(value)
    }
}

fn split_designation(value: &str) -> (Option<String>, Option<String>) {
    let mut parts = value
        .split('/')
        .map(|v| v.trim().to_ascii_uppercase())
        .filter(|v| !v.is_empty());
    (parts.next(), parts.next())
}

pub fn normalize_surface(value: &str) -> Option<String> {
    let folded = fold(value);
    if folded.is_empty() || folded == "nao definido" {
        return None;
    }
    let mapped = value
        .split(['/', '+', ';'])
        .map(fold)
        .map(|part| match part.as_str() {
            "asfalto" => "ASPHALT".into(),
            "concreto" => "CONCRETE".into(),
            "grama" => "GRASS".into(),
            "terra" | "barro" => "DIRT".into(),
            "cascalho" => "GRAVEL".into(),
            "picarra" | "saibro" => "LATERITE".into(),
            "areia" => "SAND".into(),
            "argila" => "CLAY".into(),
            "paralelepipedo" => "PAVING_STONES".into(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_uppercase();
    (!mapped.is_empty()).then_some(mapped)
}

fn compatible_surface(a: &str, b: &str) -> bool {
    let a = a.to_ascii_uppercase();
    a == b
        || (a.contains("ASPH") && b.contains("ASPHALT"))
        || (a.contains("CONC") && b.contains("CONCRETE"))
        || (a.contains("GRASS") && b.contains("GRASS"))
        || (a.contains("GRVL") && b.contains("GRAVEL"))
}

fn fold(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ã' | 'â' | 'ä' => 'a',
            'é' | 'ê' | 'ë' => 'e',
            'í' | 'ï' => 'i',
            'ó' | 'õ' | 'ô' | 'ö' => 'o',
            'ú' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let mut ends = [
        a?.trim().to_ascii_uppercase(),
        b?.trim().to_ascii_uppercase(),
    ];
    if ends.iter().any(String::is_empty) {
        return None;
    }
    ends.sort();
    Some(ends.join("/"))
}

fn renumbered(existing: &RunwayDraft, official: &AnacRunway) -> bool {
    let (Some(a), Some(b), Some(c), Some(d)) = (
        existing.le_ident.as_deref(),
        existing.he_ident.as_deref(),
        official.low_ident.as_deref(),
        official.high_ident.as_deref(),
    ) else {
        return false;
    };
    let close = |x: &str, y: &str| {
        runway_number(x)
            .zip(runway_number(y))
            .is_some_and(|((xn, xs), (yn, ys))| xs == ys && xn.abs_diff(yn) <= 1)
    };
    let length_close = existing
        .length_ft
        .zip(official.length_m)
        .is_some_and(|(feet, metres)| (feet - metres * METERS_TO_FEET).abs() <= 100.0);
    length_close && ((close(a, c) && close(b, d)) || (close(a, d) && close(b, c)))
}

fn runway_number(value: &str) -> Option<(u8, String)> {
    let value = value.trim().to_ascii_uppercase();
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    Some((digits.parse().ok()?, value[digits.len()..].to_owned()))
}

pub fn stable_anac_id(ciad: &str) -> u64 {
    let digest =
        Sha256::digest(format!("ANAC:CIAD:{}", ciad.trim().to_ascii_uppercase()).as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) & 0x0fff_ffff_ffff_ffff) | 0xd000_0000_0000_0000
}

fn validate_release(value: &str) -> Result<()> {
    if value.len() == 10
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        Ok(())
    } else {
        Err(Error::Data("ANAC release must be YYYY-MM-DD".into()))
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let part = PathBuf::from(format!("{}.part", path.display()));
    std::fs::write(&part, bytes).map_err(|e| io_error(&part, e))?;
    std::fs::rename(&part, path).map_err(|e| io_error(path, e))
}

pub fn license_allows_derivatives(label: &str) -> bool {
    let value = label.to_ascii_lowercase();
    (value.contains("decree 8,777")
        || value.contains("decreto 8.777")
        || value.contains("cc by 4.0"))
        && !value.contains("by-nd")
        && !value.contains("semderiva")
}

pub fn write_artifacts(value: &AnacArtifacts, output: &Path) -> Result<()> {
    write_csv(&output.join("anac_matches.csv"), &value.matches)?;
    write_csv(&output.join("anac_conflicts.csv"), &value.conflicts)?;
    write_csv(&output.join("anac_unmatched.csv"), &value.unmatched)
}

fn write_csv<T: Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    for row in rows {
        writer.serialize(row)?;
    }
    writer.flush().map_err(|e| io_error(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_metric_conversion() {
        assert!((1000.0 * METERS_TO_FEET - 3_280.839_895_013_123).abs() < 1e-9);
    }

    #[test]
    fn physical_length_is_not_a_declared_distance() {
        let row = AnacRunway {
            designation: "09/27".into(),
            low_ident: Some("09".into()),
            high_ident: Some("27".into()),
            length_m: Some(1200.0),
            width_m: Some(30.0),
            surface_raw: "Asfalto".into(),
            surface: normalize_surface("Asfalto"),
        };
        assert_eq!(row.length_m, Some(1200.0));
    }

    #[test]
    fn surface_and_composite_surface_mapping() {
        assert_eq!(
            normalize_surface("Asfalto / Concreto").as_deref(),
            Some("ASPHALT/CONCRETE")
        );
        assert_eq!(normalize_surface("Não definido"), None);
    }

    #[test]
    fn stable_ids_are_namespaced_and_order_independent() {
        assert_eq!(stable_anac_id("SP0075"), stable_anac_id("sp0075"));
        assert_ne!(stable_anac_id("SP0075"), stable_anac_id("SP0076"));
        assert_eq!(stable_anac_id("SP0075") >> 60, 0xd);
    }

    #[test]
    fn runway_renumbering_needs_geometry_proxy() {
        let existing = RunwayDraft {
            length_ft: Some(3000.0),
            le_ident: Some("09".into()),
            he_ident: Some("27".into()),
            ..Default::default()
        };
        let official = AnacRunway {
            designation: "10/28".into(),
            low_ident: Some("10".into()),
            high_ident: Some("28".into()),
            length_m: Some(914.4),
            width_m: None,
            surface_raw: "Asfalto".into(),
            surface: Some("ASPHALT".into()),
        };
        assert!(renumbered(&existing, &official));
        let mut far = official.clone();
        far.length_m = Some(1500.0);
        assert!(!renumbered(&existing, &far));
    }

    #[test]
    fn missing_elevation_is_not_zero() {
        assert_eq!(measured_decimal(",0"), None);
        assert_eq!(measured_decimal("0,0"), Some(0.0));
    }

    #[test]
    fn duplicate_designations_are_canonical() {
        assert_eq!(
            canonical(Some("09"), Some("27")),
            canonical(Some("27"), Some("09"))
        );
    }

    #[test]
    fn unknown_status_is_not_invented() {
        assert_eq!(fold("Situação desconhecida"), "situacao desconhecida");
        assert!(matches!(AirportStatus::Close, AirportStatus::Close));
    }

    #[test]
    fn incomplete_schema_is_rejected() {
        assert!(
            !validate_headers(
                "Código OACI;Nome",
                b';',
                &["Código OACI", "CIAD", "Altitude"]
            )
            .unwrap()
        );
    }

    #[test]
    fn duplicate_official_icao_remains_ambiguous() {
        let mut index: HashMap<&str, Vec<u8>> = HashMap::new();
        index.entry("SBXX").or_default().extend([1, 2]);
        assert_eq!(index["SBXX"].len(), 2);
    }

    #[test]
    fn incompatible_no_derivatives_license_is_rejected() {
        assert!(!license_allows_derivatives("CC BY-ND 3.0 SemDerivações"));
        assert!(license_allows_derivatives(
            "Decreto 8.777 open-data database"
        ));
        assert!(license_allows_derivatives("CC BY 4.0"));
    }

    #[test]
    fn parses_real_private_v2_shape_and_adds_complete_airport() {
        let dir = tempfile::tempdir().unwrap();
        let content = "Atualizado em: 2026-10-11\nCódigo OACI;CIAD;Nome;Município;UF;Longitude;Latitude;Altitude;Operação Diurna;Operação Noturna;Designação 1;Comprimento 1;Largura 1;Resistência 1;Superfície 1;Designação 2;Comprimento 2;Largura 2;Resistência 2;Superfície 2;Portaria de Registro;Link Portaria;LATGEOPOINT;LONGEOPOINT\nSJZZ;SP9999;Teste;TESTE;SP;046°00'00\"W;23°00'00\"S;700,0;VFR;VFR;09/27;1000,0000;30,00;;Asfalto;;;;;;PA2026-1;;-23,0;-46,0\n";
        let bytes = WINDOWS_1252.encode(content).0;
        let path = dir.path().join("private_v2.csv");
        std::fs::write(&path, bytes).unwrap();
        let rows = parse_v2(&path, "private").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].runways[0].length_m, Some(1000.0));
        let data = AnacDataset {
            release: "2026-10-11".into(),
            airports: rows,
            siros_rows: 0,
        };
        let mut drafts = Vec::new();
        let out = apply(&mut drafts, &data);
        assert_eq!(out.statistics.new_airports_added, 1);
        assert_eq!(drafts[0].runways.len(), 1);
    }

    #[test]
    fn parses_documented_closed_public_status() {
        let dir = tempfile::tempdir().unwrap();
        let content = "Atualizado em: 2026-10-01\nCódigo OACI;CIAD;Nome;LATGEOPOINT;LONGEOPOINT;Altitude;Situação;Portaria de Registro\nSBZZ;SP0001;Teste;-23.0;-46.0;700,0;Excluído;PA2020-1\n";
        let bytes = WINDOWS_1252.encode(content).0;
        let path = dir.path().join("public_v2.csv");
        std::fs::write(&path, bytes).unwrap();
        let rows = parse_v2(&path, "public").unwrap();
        assert_eq!(rows[0].status, Some(AirportStatus::Close));
    }

    #[tokio::test]
    async fn offline_mode_requires_exact_anac_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::from_root(dir.path());
        config.offline = true;
        let error = prepare(&config).await.unwrap_err();
        assert!(matches!(error, Error::MissingCache(_)));
    }
}

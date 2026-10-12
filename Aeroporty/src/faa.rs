//! Official FAA 28-day NASR APT adapter.
#![allow(clippy::too_many_arguments)]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::{Component, Path},
    time::Duration,
};

use chrono::Utc;
use reqwest::header;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    config::Config,
    error::{Error, Result, io_error},
    models::{
        AirportDraft, AirportStatus, CandidateAssessment, Evidence, EvidenceMethod, FaaArtifacts,
        FaaDecisionRow, FaaMatchRow, FaaUnmatchedRow, QuarantineCategory, RunwayDraft, RunwayKind,
        SourceInfo,
    },
    runway_classification::classify_runway,
    validation::valid_icao,
};

const REQUIRED: [(&str, &[&str]); 3] = [
    (
        "APT_BASE.csv",
        &[
            "EFF_DATE",
            "SITE_NO",
            "SITE_TYPE_CODE",
            "ARPT_ID",
            "ICAO_ID",
            "ARPT_NAME",
            "LAT_DECIMAL",
            "LONG_DECIMAL",
            "ELEV",
            "ARPT_STATUS",
        ],
    ),
    (
        "APT_RWY.csv",
        &[
            "EFF_DATE",
            "SITE_NO",
            "SITE_TYPE_CODE",
            "RWY_ID",
            "RWY_LEN",
            "SURFACE_TYPE_CODE",
        ],
    ),
    (
        "APT_RWY_END.csv",
        &[
            "EFF_DATE",
            "SITE_NO",
            "SITE_TYPE_CODE",
            "RWY_ID",
            "RWY_END_ID",
            "LAT_DECIMAL",
            "LONG_DECIMAL",
        ],
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaaManifest {
    pub source_url: String,
    pub downloaded_at: String,
    pub effective_date: String,
    pub version: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub archive_sha256: String,
    pub archive_size: u64,
    pub files: Vec<FaaManifestFile>,
    pub schema_valid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaaManifestFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub required_headers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FaaAirport {
    pub effective_date: String,
    pub site_no: String,
    pub site_type: String,
    pub arpt_id: String,
    pub icao: String,
    pub name: String,
    pub country_code: String,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_ft: Option<f64>,
    pub status: Option<AirportStatus>,
    pub runways: Vec<FaaRunway>,
}

#[derive(Debug, Clone)]
pub struct FaaRunway {
    pub id: String,
    pub length_ft: Option<f64>,
    pub width_ft: Option<f64>,
    pub surface_raw: String,
    pub surface: Option<String>,
    pub low_ident: Option<String>,
    pub high_ident: Option<String>,
    pub low_latitude: Option<f64>,
    pub low_longitude: Option<f64>,
    pub high_latitude: Option<f64>,
    pub high_longitude: Option<f64>,
    pub kind: RunwayKind,
}

#[derive(Debug, Clone)]
pub struct FaaDataset {
    pub release: String,
    pub airports: Vec<FaaAirport>,
}

pub async fn prepare(config: &Config) -> Result<(FaaDataset, SourceInfo)> {
    validate_release(&config.faa_release)?;
    let dir = config.cache_dir.join("faa").join(&config.faa_release);
    std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let filename = release_filename(&config.faa_release)?;
    let archive = dir.join(&filename);
    let extracted = dir.join("extracted");
    let manifest_path = dir.join("manifest.json");
    let url = format!("https://nfdc.faa.gov/webContent/28DaySub/extra/{filename}?download=1");
    let prior: Option<FaaManifest> = if manifest_path.exists() {
        Some(serde_json::from_slice(
            &std::fs::read(&manifest_path).map_err(|e| io_error(&manifest_path, e))?,
        )?)
    } else {
        None
    };

    let mut etag = prior.as_ref().and_then(|m| m.etag.clone());
    let mut last_modified = prior.as_ref().and_then(|m| m.last_modified.clone());
    let mut downloaded_at = prior
        .as_ref()
        .map(|m| m.downloaded_at.clone())
        .unwrap_or_default();
    if config.offline {
        if !archive.exists() {
            return Err(Error::MissingCache(archive));
        }
        if let Some(manifest) = &prior {
            let actual = sha256_file(&archive)?;
            if actual != manifest.archive_sha256 {
                return Err(Error::Data(format!(
                    "FAA archive checksum mismatch for release {}",
                    config.faa_release
                )));
            }
        }
    } else {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .user_agent("Aeroporty/0.1 FAA NASR importer")
            .build()?;
        let mut succeeded = false;
        let mut last_error = String::new();
        for attempt in 0..3 {
            let mut request = client.get(&url).header(header::REFERER, format!("https://www.faa.gov/air_traffic/flight_info/aeronav/Aero_Data/NASR_Subscription/{}/", config.faa_release));
            if !config.force {
                if let Some(value) = &etag {
                    request = request.header(header::IF_NONE_MATCH, value);
                }
                if let Some(value) = &last_modified {
                    request = request.header(header::IF_MODIFIED_SINCE, value);
                }
            }
            match request.send().await {
                Ok(response)
                    if response.status() == reqwest::StatusCode::NOT_MODIFIED
                        && archive.exists() =>
                {
                    succeeded = true;
                    break;
                }
                Ok(response) if response.status().is_success() => {
                    etag = response
                        .headers()
                        .get(header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    last_modified = response
                        .headers()
                        .get(header::LAST_MODIFIED)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let bytes = response.bytes().await?;
                    if bytes.len() < 1024 {
                        return Err(Error::Data("FAA archive is unexpectedly small".into()));
                    }
                    let part = archive.with_extension("zip.part");
                    std::fs::write(&part, &bytes).map_err(|e| io_error(&part, e))?;
                    verify_zip(&part)?;
                    replace_file(&part, &archive)?;
                    downloaded_at = Utc::now().to_rfc3339();
                    succeeded = true;
                    break;
                }
                Ok(response) => last_error = format!("HTTP {}", response.status()),
                Err(error) => last_error = error.to_string(),
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_secs(2_u64.pow(attempt))).await;
            }
        }
        if !succeeded {
            if archive.exists() && !config.force {
                warn!(%last_error, release=%config.faa_release, "FAA недоступна; явно используется кэш этого же выпуска");
            } else {
                return Err(Error::Data(format!(
                    "не удалось получить FAA {}: {last_error}",
                    config.faa_release
                )));
            }
        }
    }

    extract_faa_archive(&archive, &extracted)?;
    let mut files = Vec::new();
    for (name, headers) in REQUIRED {
        let path = extracted.join(name);
        validate_schema(&path, headers)?;
        files.push(FaaManifestFile {
            name: name.into(),
            size: std::fs::metadata(&path)
                .map_err(|e| io_error(&path, e))?
                .len(),
            sha256: sha256_file(&path)?,
            required_headers: headers.iter().map(|s| (*s).into()).collect(),
        });
    }
    if downloaded_at.is_empty() {
        downloaded_at = Utc::now().to_rfc3339();
    }
    let manifest = FaaManifest {
        source_url: url.clone(),
        downloaded_at: downloaded_at.clone(),
        effective_date: config.faa_release.clone(),
        version: filename.clone(),
        etag: etag.clone(),
        last_modified: last_modified.clone(),
        archive_sha256: sha256_file(&archive)?,
        archive_size: std::fs::metadata(&archive)
            .map_err(|e| io_error(&archive, e))?
            .len(),
        files,
        schema_valid: true,
    };
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
        .map_err(|e| io_error(&manifest_path, e))?;
    let dataset = parse_dataset(&extracted, &config.faa_release)?;
    Ok((
        dataset,
        SourceInfo {
            name: "FAA/NASR APT".into(),
            url,
            local_file: archive.display().to_string(),
            retrieved_at: Some(downloaded_at),
            etag,
            last_modified,
        },
    ))
}

fn validate_release(value: &str) -> Result<()> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| Error::Data(format!("некорректная дата выпуска FAA: {value}")))?;
    Ok(())
}

fn release_filename(release: &str) -> Result<String> {
    let date = chrono::NaiveDate::parse_from_str(release, "%Y-%m-%d")
        .map_err(|_| Error::Data("invalid FAA release".into()))?;
    Ok(format!(
        "{}_{}_{}_APT_CSV.zip",
        date.format("%d"),
        date.format("%b"),
        date.format("%Y")
    ))
}

fn verify_zip(path: &Path) -> Result<()> {
    let file = File::open(path).map_err(|e| io_error(path, e))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| Error::Data(format!("повреждённый FAA ZIP: {e}")))?;
    if zip.is_empty() {
        return Err(Error::Data("пустой FAA ZIP".into()));
    }
    for i in 0..zip.len() {
        let _ = zip
            .by_index(i)
            .map_err(|e| Error::Data(format!("повреждённая запись ZIP: {e}")))?;
    }
    Ok(())
}

pub fn extract_faa_archive(archive: &Path, output: &Path) -> Result<()> {
    verify_zip(archive)?;
    std::fs::create_dir_all(output).map_err(|e| io_error(output, e))?;
    let file = File::open(archive).map_err(|e| io_error(archive, e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::Data(e.to_string()))?;
    let wanted: HashSet<_> = REQUIRED
        .iter()
        .map(|(n, _)| n.to_ascii_uppercase())
        .collect();
    let mut found = HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| Error::Data(e.to_string()))?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| Error::Data(format!("небезопасный путь FAA ZIP: {}", entry.name())))?;
        if enclosed.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(Error::Data("FAA ZIP path traversal".into()));
        }
        let Some(base) = enclosed.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if !wanted.contains(&base.to_ascii_uppercase()) {
            continue;
        }
        if entry.size() > 200_000_000 {
            return Err(Error::Data(format!("FAA ZIP entry too large: {base}")));
        }
        let target = output.join(base);
        let part = target.with_extension("csv.part");
        let mut writer = File::create(&part).map_err(|e| io_error(&part, e))?;
        std::io::copy(&mut entry, &mut writer).map_err(|e| io_error(&part, e))?;
        writer.flush().map_err(|e| io_error(&part, e))?;
        replace_file(&part, &target)?;
        found.insert(base.to_ascii_uppercase());
    }
    for (name, _) in REQUIRED {
        if !found.contains(&name.to_ascii_uppercase()) && !output.join(name).exists() {
            return Err(Error::Data(format!("FAA ZIP не содержит {name}")));
        }
    }
    Ok(())
}

fn validate_schema(path: &Path, required: &[&str]) -> Result<()> {
    let mut reader = csv::Reader::from_path(path)?;
    let headers = reader.headers()?.clone();
    let missing: Vec<_> = required
        .iter()
        .filter(|x| !headers.iter().any(|h| h == **x))
        .collect();
    if !missing.is_empty() {
        return Err(Error::Data(format!(
            "несовместимая FAA schema {}: отсутствуют {:?}",
            path.display(),
            missing
        )));
    }
    if reader.records().next().transpose()?.is_none() {
        return Err(Error::Data(format!("FAA file empty: {}", path.display())));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| io_error(path, e))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| io_error(path, e))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:X}", hash.finalize()))
}

fn replace_file(part: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        std::fs::remove_file(target).map_err(|e| io_error(target, e))?;
    }
    std::fs::rename(part, target).map_err(|e| io_error(target, e))
}

fn parse_dataset(dir: &Path, release: &str) -> Result<FaaDataset> {
    let mut airports = Vec::new();
    let mut by_key = HashMap::new();
    let mut base = csv::Reader::from_path(dir.join("APT_BASE.csv"))?;
    let headers = base.headers()?.clone();
    for row in base.records() {
        let row = row?;
        let get = |name: &str| field(&headers, &row, name);
        let site_no = get("SITE_NO").to_owned();
        let site_type = get("SITE_TYPE_CODE").to_owned();
        let latitude = parse_f64(get("LAT_DECIMAL"));
        let longitude = parse_f64(get("LONG_DECIMAL"));
        let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
            continue;
        };
        let status = match get("ARPT_STATUS") {
            "O" => Some(AirportStatus::Open),
            "CI" | "CP" => Some(AirportStatus::Close),
            _ => None,
        };
        let index = airports.len();
        by_key.insert((site_no.clone(), site_type.clone()), index);
        airports.push(FaaAirport {
            effective_date: get("EFF_DATE").replace('/', "-"),
            site_no,
            site_type,
            arpt_id: get("ARPT_ID").into(),
            icao: get("ICAO_ID").trim().to_ascii_uppercase(),
            name: get("ARPT_NAME").into(),
            country_code: get("COUNTRY_CODE").into(),
            latitude,
            longitude,
            elevation_ft: parse_f64(get("ELEV")),
            status,
            runways: Vec::new(),
        });
    }
    let ends = parse_ends(&dir.join("APT_RWY_END.csv"))?;
    let mut reader = csv::Reader::from_path(dir.join("APT_RWY.csv"))?;
    let headers = reader.headers()?.clone();
    for row in reader.records() {
        let row = row?;
        let get = |name: &str| field(&headers, &row, name);
        let key = (get("SITE_NO").to_owned(), get("SITE_TYPE_CODE").to_owned());
        let Some(&index) = by_key.get(&key) else {
            continue;
        };
        let id = get("RWY_ID").trim().to_ascii_uppercase();
        let (low_ident, high_ident) = split_designation(&id);
        let low = low_ident
            .as_ref()
            .and_then(|e| ends.get(&(key.0.clone(), key.1.clone(), id.clone(), e.clone())));
        let high = high_ident
            .as_ref()
            .and_then(|e| ends.get(&(key.0.clone(), key.1.clone(), id.clone(), e.clone())));
        let raw = get("SURFACE_TYPE_CODE").trim().to_ascii_uppercase();
        let surface = normalize_faa_surface(&raw);
        let mut runway = RunwayDraft {
            id: format!("FAA:{}:{id}", key.0),
            length_ft: parse_f64(get("RWY_LEN")),
            width_ft: parse_f64(get("RWY_WIDTH")),
            surface: surface.clone(),
            le_ident: low_ident.clone(),
            he_ident: high_ident.clone(),
            ..Default::default()
        };
        let airport_type = match airports[index].site_type.as_str() {
            "C" => "seaplane_base",
            "H" => "heliport",
            _ => "small_airport",
        };
        let (kind, _) = classify_runway(airport_type, &runway);
        runway.kind = kind;
        airports[index].runways.push(FaaRunway {
            id,
            length_ft: runway.length_ft,
            width_ft: runway.width_ft,
            surface_raw: raw,
            surface,
            low_ident,
            high_ident,
            low_latitude: low.and_then(|v| v.0),
            low_longitude: low.and_then(|v| v.1),
            high_latitude: high.and_then(|v| v.0),
            high_longitude: high.and_then(|v| v.1),
            kind,
        });
    }
    Ok(FaaDataset {
        release: release.into(),
        airports,
    })
}

type EndKey = (String, String, String, String);
type EndValue = (Option<f64>, Option<f64>);
fn parse_ends(path: &Path) -> Result<HashMap<EndKey, EndValue>> {
    let mut result = HashMap::new();
    let mut reader = csv::Reader::from_path(path)?;
    let headers = reader.headers()?.clone();
    for row in reader.records() {
        let row = row?;
        let get = |name: &str| field(&headers, &row, name);
        result.insert(
            (
                get("SITE_NO").into(),
                get("SITE_TYPE_CODE").into(),
                get("RWY_ID").trim().to_ascii_uppercase(),
                get("RWY_END_ID").trim().to_ascii_uppercase(),
            ),
            (
                parse_f64(get("LAT_DECIMAL")),
                parse_f64(get("LONG_DECIMAL")),
            ),
        );
    }
    Ok(result)
}

fn field<'a>(headers: &csv::StringRecord, row: &'a csv::StringRecord, name: &str) -> &'a str {
    headers
        .iter()
        .position(|h| h == name)
        .and_then(|i| row.get(i))
        .unwrap_or("")
}
fn parse_f64(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}
fn split_designation(value: &str) -> (Option<String>, Option<String>) {
    let mut p = value
        .split('/')
        .map(|v| v.trim().to_ascii_uppercase())
        .filter(|v| !v.is_empty());
    (p.next(), p.next())
}

pub fn normalize_faa_surface(raw: &str) -> Option<String> {
    let raw = raw.trim().to_ascii_uppercase();
    if raw.is_empty() || matches!(raw.as_str(), "NSTD" | "UNKNOWN") {
        return None;
    }
    let value = raw
        .split('/')
        .map(|p| match p.trim() {
            "TURF" | "GRASS" | "SOD" => "GRASS",
            "TRTD" => "TREATED",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("/");
    Some(value)
}

pub fn apply(drafts: &mut [AirportDraft], faa: &FaaDataset) -> FaaArtifacts {
    let mut artifacts = FaaArtifacts::default();
    artifacts.statistics.release = faa.release.clone();
    artifacts.statistics.airports = faa.airports.len();
    let mut index: HashMap<&str, Vec<&FaaAirport>> = HashMap::new();
    for airport in &faa.airports {
        if valid_icao(&airport.icao) {
            artifacts.statistics.airports_with_icao += 1;
            if in_faa_scope(&airport.country_code) {
                index.entry(&airport.icao).or_default().push(airport);
            }
        }
    }
    let oa_icaos: HashSet<_> = drafts.iter().map(|d| d.icao.as_str()).collect();
    for airport in &faa.airports {
        if valid_icao(&airport.icao)
            && in_faa_scope(&airport.country_code)
            && !oa_icaos.contains(airport.icao.as_str())
        {
            artifacts.unmatched.push(FaaUnmatchedRow {
                icao: airport.icao.clone(),
                site_no: airport.site_no.clone(),
                reason: "ICAO отсутствует в OurAirports scope".into(),
            });
        }
    }
    for draft in drafts {
        let Some(matches) = index.get(draft.icao.as_str()) else {
            continue;
        };
        if matches.len() != 1 {
            artifacts.statistics.ambiguous_matches += 1;
            for candidate in matches {
                artifacts.unmatched.push(FaaUnmatchedRow {
                    icao: draft.icao.clone(),
                    site_no: candidate.site_no.clone(),
                    reason: "неоднозначный официальный ICAO".into(),
                });
            }
            continue;
        }
        let official = matches[0];
        let distance = haversine_km(
            draft.latitude.unwrap_or(f64::NAN),
            draft.longitude.unwrap_or(f64::NAN),
            official.latitude,
            official.longitude,
        );
        if !distance.is_finite() || distance > 15.0 {
            artifacts.unmatched.push(FaaUnmatchedRow {
                icao: draft.icao.clone(),
                site_no: official.site_no.clone(),
                reason: format!("координаты расходятся на {distance:.1} km"),
            });
            continue;
        }
        artifacts.statistics.matched_airports += 1;
        artifacts.matches.push(FaaMatchRow {
            icao: draft.icao.clone(),
            site_no: official.site_no.clone(),
            arpt_id: official.arpt_id.clone(),
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
            .insert("faa_site_no".into(), official.site_no.clone());
        draft
            .provenance
            .insert("faa_arpt_id".into(), official.arpt_id.clone());
        draft
            .provenance
            .insert("faa_release".into(), faa.release.clone());
        confirm(
            &mut draft.evidence,
            "icao",
            official,
            &official.icao,
            &draft.icao,
            "exact official ICAO",
            &mut artifacts,
        );
        if distance <= 0.25 {
            if let Some(v) = draft.latitude {
                confirm(
                    &mut draft.evidence,
                    "latitude",
                    official,
                    &official.latitude.to_string(),
                    &v.to_string(),
                    "airport reference points within 250 m",
                    &mut artifacts,
                );
            }
            if let Some(v) = draft.longitude {
                confirm(
                    &mut draft.evidence,
                    "longitude",
                    official,
                    &official.longitude.to_string(),
                    &v.to_string(),
                    "airport reference points within 250 m",
                    &mut artifacts,
                );
            }
        }
        if let (Some(f), Some(o)) = (official.elevation_ft, draft.elevation_ft) {
            if (f - o as f64).abs() <= 1.0 {
                confirm(
                    &mut draft.evidence,
                    "elevation_ft",
                    official,
                    &f.to_string(),
                    &o.to_string(),
                    "FAA nearest-tenth-foot elevation agrees within 1 ft",
                    &mut artifacts,
                );
            }
        } else if draft.elevation_ft.is_none()
            && let Some(f) = official.elevation_ft
        {
            draft.elevation_ft = Some(f.round() as i32);
            recover(
                &mut draft.evidence,
                "elevation_ft",
                official,
                &f.to_string(),
                &(f.round() as i32).to_string(),
                "official FAA airport elevation",
                &mut artifacts,
                None,
            );
        }
        if normalize_name(&draft.name) == normalize_name(&official.name) {
            let existing = draft.name.clone();
            confirm(
                &mut draft.evidence,
                "name",
                official,
                &official.name,
                &existing,
                "normalized official name agrees",
                &mut artifacts,
            );
        }
        if let (Some(f), Some(o)) = (official.status, draft.status) {
            if f == o {
                confirm(
                    &mut draft.evidence,
                    "status",
                    official,
                    match f {
                        AirportStatus::Open => "O",
                        AirportStatus::Close => "CP/CI",
                    },
                    match o {
                        AirportStatus::Open => "open",
                        AirportStatus::Close => "close",
                    },
                    "FAA ARPT_STATUS agrees with Phase 1 status semantics",
                    &mut artifacts,
                );
            } else {
                conflict(
                    &mut draft.evidence,
                    "status",
                    official,
                    format!("{o:?}"),
                    format!("{f:?}"),
                    "FAA ARPT_STATUS contradicts source classification",
                    &mut artifacts,
                    None,
                );
            }
        }
        merge_runways(draft, official, &mut artifacts);
    }
    artifacts.statistics.unmatched_faa_airports = artifacts.unmatched.len();
    artifacts.statistics.unresolved_conflicts = artifacts.statistics.conflicts;
    artifacts
}

/// Add complete FAA-only airports as first-class candidates. Every value remains
/// single-source `Recovered`; validation and timezone resolution are still shared.
pub fn add_new_airports(
    drafts: &mut Vec<AirportDraft>,
    faa: &FaaDataset,
    artifacts: &mut FaaArtifacts,
) {
    let existing_icaos: HashSet<String> = drafts.iter().map(|d| d.icao.clone()).collect();
    let mut used_ids: HashSet<u64> = drafts.iter().filter_map(|d| d.id).collect();
    let matched: HashSet<&str> = artifacts.matches.iter().map(|m| m.icao.as_str()).collect();
    let mut icao_counts: HashMap<&str, usize> = HashMap::new();
    for airport in &faa.airports {
        if valid_icao(&airport.icao) {
            *icao_counts.entry(&airport.icao).or_default() += 1;
        }
    }

    for airport in faa
        .airports
        .iter()
        .filter(|a| valid_icao(&a.icao) && !matched.contains(a.icao.as_str()))
    {
        artifacts.statistics.unmatched_total += 1;
        let source_id = format!("{}:{}", airport.site_no, airport.site_type);
        let mut assessment = CandidateAssessment {
            icao: airport.icao.clone(),
            source_id: source_id.clone(),
            country_code: airport.country_code.clone(),
            facility_type: airport.site_type.clone(),
            category: String::new(),
            priority: 9,
            reason: String::new(),
            added: false,
        };
        if !in_faa_scope(&airport.country_code) {
            artifacts.statistics.unmatched_out_of_scope += 1;
            if existing_icaos.contains(&airport.icao) {
                artifacts.statistics.unmatched_existing_out_of_scope += 1;
                assessment.category = "already_known_outside_faa_scope".into();
                assessment.reason =
                    "ICAO уже есть в OurAirports, но страна не входит в область FAA adapter".into();
            } else {
                assessment.category = "outside_supported_region".into();
                assessment.reason =
                    "страна не входит в документированную область FAA adapter".into();
            }
            artifacts.candidates.push(assessment);
            continue;
        }
        if existing_icaos.contains(&airport.icao) {
            assessment.category = "already_known_not_matched".into();
            assessment.reason = "ICAO уже существует, но не прошёл Phase 2 identity checks".into();
            artifacts.candidates.push(assessment);
            continue;
        }
        if icao_counts.get(airport.icao.as_str()).copied().unwrap_or(0) != 1 {
            assessment.category = "ambiguous_icao".into();
            assessment.reason = "несколько FAA records используют один ICAO".into();
            artifacts.candidates.push(assessment);
            continue;
        }
        if airport.site_type != "A" {
            artifacts.statistics.unmatched_excluded_type += 1;
            assessment.category = match airport.site_type.as_str() {
                "H" => "heliport",
                "C" => "seaplane_base",
                _ => "unsupported_facility_type",
            }
            .into();
            assessment.reason = "объект не является обычным наземным аэропортом FAA type A".into();
            artifacts.candidates.push(assessment);
            continue;
        }
        let land: Vec<_> = airport
            .runways
            .iter()
            .filter(|r| r.kind == RunwayKind::LandRunway)
            .collect();
        let incomplete = land.iter().any(|r| {
            r.length_ft.is_none_or(|v| v <= 0.0)
                || r.surface
                    .as_deref()
                    .is_none_or(crate::runway_classification::is_unknown_surface)
        });
        let country = country_name(&airport.country_code);
        if airport.name.trim().is_empty()
            || airport.elevation_ft.is_none()
            || airport.status.is_none()
            || country.is_none()
            || land.is_empty()
            || incomplete
        {
            artifacts.statistics.unmatched_insufficient += 1;
            assessment.category = "insufficient_required_data".into();
            assessment.priority = 5;
            assessment.reason = format!(
                "name={}, elevation={}, status={}, country={}, land_runways={}, incomplete_runway={}",
                !airport.name.trim().is_empty(),
                airport.elevation_ft.is_some(),
                airport.status.is_some(),
                country.is_some(),
                land.len(),
                incomplete
            );
            artifacts.candidates.push(assessment);
            continue;
        }
        artifacts.statistics.new_airport_candidates += 1;
        let id = stable_faa_id(&source_id);
        if !used_ids.insert(id) {
            assessment.category = "stable_id_collision".into();
            assessment.reason = format!("детерминированный ID {id} уже используется");
            artifacts.candidates.push(assessment);
            continue;
        }
        let mut airport_evidence = BTreeMap::new();
        for (field, raw, normalized, rule) in [
            (
                "icao",
                airport.icao.as_str(),
                airport.icao.as_str(),
                "official ICAO_ID",
            ),
            (
                "latitude",
                &airport.latitude.to_string(),
                &airport.latitude.to_string(),
                "official airport reference point",
            ),
            (
                "longitude",
                &airport.longitude.to_string(),
                &airport.longitude.to_string(),
                "official airport reference point",
            ),
            (
                "type",
                airport.site_type.as_str(),
                if airport.status == Some(AirportStatus::Close) {
                    "closed"
                } else {
                    "airport"
                },
                "FAA site type A",
            ),
            (
                "country",
                airport.country_code.as_str(),
                country.unwrap(),
                "FAA country code mapping",
            ),
            (
                "elevation_ft",
                &airport.elevation_ft.unwrap().to_string(),
                &airport.elevation_ft.unwrap().round().to_string(),
                "official FAA elevation rounded to whole foot",
            ),
            (
                "status",
                match airport.status.unwrap() {
                    AirportStatus::Open => "O",
                    AirportStatus::Close => "CI/CP",
                },
                match airport.status.unwrap() {
                    AirportStatus::Open => "open",
                    AirportStatus::Close => "close",
                },
                "documented FAA ARPT_STATUS",
            ),
            (
                "id",
                source_id.as_str(),
                &id.to_string(),
                "stable SHA-256 namespaced FAA identifier",
            ),
            (
                "name",
                airport.name.as_str(),
                airport.name.as_str(),
                "official facility name",
            ),
        ] {
            airport_evidence
                .entry(field.into())
                .or_insert_with(Vec::new)
                .push(recovered_evidence(airport, raw, normalized, rule, None));
        }
        let runways =
            land.into_iter()
                .map(|r| {
                    let runway_id = format!("FAA:{}:{}", airport.site_no, r.id);
                    let mut evidence = BTreeMap::new();
                    for (field, raw, normalized, rule) in [
                        (
                            "id",
                            r.id.as_str(),
                            runway_id.as_str(),
                            "official FAA runway identifier",
                        ),
                        (
                            "length_ft",
                            &r.length_ft.unwrap().to_string(),
                            &r.length_ft.unwrap().to_string(),
                            "FAA physical runway length in feet",
                        ),
                        (
                            "surface",
                            r.surface_raw.as_str(),
                            r.surface.as_deref().unwrap(),
                            "FAA surface mapping with raw value retained",
                        ),
                    ] {
                        evidence.entry(field.into()).or_insert_with(Vec::new).push(
                            recovered_evidence(airport, raw, normalized, rule, Some(&r.id)),
                        );
                    }
                    RunwayDraft {
                        id: runway_id,
                        length_ft: r.length_ft,
                        width_ft: r.width_ft,
                        surface: r.surface.clone(),
                        le_ident: r.low_ident.clone(),
                        he_ident: r.high_ident.clone(),
                        le_latitude: r.low_latitude,
                        le_longitude: r.low_longitude,
                        he_latitude: r.high_latitude,
                        he_longitude: r.high_longitude,
                        kind: RunwayKind::LandRunway,
                        classification_reason: "official FAA type-A land runway".into(),
                        provenance: BTreeMap::from([
                            ("source".into(), "FAA/NASR APT_RWY".into()),
                            ("faa_runway_id".into(), r.id.clone()),
                        ]),
                        evidence,
                        ..Default::default()
                    }
                })
                .collect();
        drafts.push(AirportDraft {
            icao: airport.icao.clone(),
            latitude: Some(airport.latitude),
            longitude: Some(airport.longitude),
            status: airport.status,
            airport_type: if airport.status == Some(AirportStatus::Close) {
                "closed"
            } else {
                "airport"
            }
            .into(),
            country: country.map(str::to_owned),
            elevation_ft: airport.elevation_ft.map(|v| v.round() as i32),
            id: Some(id),
            name: airport.name.clone(),
            runways,
            provenance: BTreeMap::from([
                ("source".into(), "FAA/NASR APT_BASE".into()),
                ("faa_site_no".into(), airport.site_no.clone()),
                ("faa_arpt_id".into(), airport.arpt_id.clone()),
                ("faa_release".into(), faa.release.clone()),
            ]),
            evidence: airport_evidence,
            ..Default::default()
        });
        assessment.category = "new_complete_airport".into();
        assessment.priority = 1;
        assessment.reason =
            "полный официальный FAA record; передан общей timezone и strict validation".into();
        assessment.added = true;
        artifacts.statistics.new_airports_added += 1;
        artifacts.candidates.push(assessment);
    }
}

fn recovered_evidence(
    airport: &FaaAirport,
    raw: &str,
    normalized: &str,
    rule: &str,
    runway: Option<&str>,
) -> Evidence {
    evidence(
        airport,
        raw,
        normalized,
        EvidenceMethod::Recovered,
        rule,
        None,
        runway,
    )
}

pub fn stable_faa_id(source_id: &str) -> u64 {
    let digest = Sha256::digest(format!("aeroporty:FAA/NASR:{source_id}").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) & 0x0fff_ffff_ffff_ffff) | 0xe000_0000_0000_0000
}

fn country_name(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_uppercase().as_str() {
        "US" => Some("United States"),
        "AS" => Some("American Samoa"),
        "GU" => Some("Guam"),
        "MP" => Some("Northern Mariana Islands"),
        "PR" => Some("Puerto Rico"),
        "VI" => Some("U.S. Virgin Islands"),
        "FM" => Some("Micronesia"),
        "MH" => Some("Marshall Islands"),
        "PW" => Some("Palau"),
        "UM" => Some("U.S. Minor Outlying Islands"),
        _ => None,
    }
}

fn merge_runways(draft: &mut AirportDraft, official: &FaaAirport, out: &mut FaaArtifacts) {
    for fr in official
        .runways
        .iter()
        .filter(|r| r.kind == RunwayKind::LandRunway)
    {
        let key = canonical(fr.low_ident.as_deref(), fr.high_ident.as_deref());
        let matched = draft.runways.iter().position(|r| {
            canonical(r.le_ident.as_deref(), r.he_ident.as_deref()) == key && key.is_some()
        });
        if let Some(pos) = matched {
            out.statistics.matched_runways += 1;
            let runway = &mut draft.runways[pos];
            let existing_id = runway.id.clone();
            confirm_runway(
                runway,
                "id",
                official,
                fr,
                &fr.id,
                &existing_id,
                "canonical runway ends agree",
                out,
            );
            match (runway.length_ft, fr.length_ft) {
                (Some(a), Some(b)) if (a - b).abs() <= 50.0 => confirm_runway(
                    runway,
                    "length_ft",
                    official,
                    fr,
                    &b.to_string(),
                    &a.to_string(),
                    "lengths agree within 50 ft",
                    out,
                ),
                (Some(a), Some(b)) if (a - b).abs() > 500.0 && (a - b).abs() / b.max(1.0) > 0.1 => {
                    conflict_runway(
                        runway,
                        "length_ft",
                        official,
                        fr,
                        a.to_string(),
                        b.to_string(),
                        "length differs by more than 500 ft and 10%",
                        out,
                    )
                }
                (None, Some(b)) => {
                    runway.length_ft = Some(b);
                    recover_runway(
                        runway,
                        "length_ft",
                        official,
                        fr,
                        &b.to_string(),
                        &b.to_string(),
                        "official FAA physical runway length",
                        out,
                    );
                }
                _ => {}
            }
            match (&runway.surface, &fr.surface) {
                (Some(a), Some(b)) if surfaces_compatible(a, b) => {
                    let av = a.clone();
                    confirm_runway(
                        runway,
                        "surface",
                        official,
                        fr,
                        &fr.surface_raw,
                        &av,
                        "canonical surface families agree",
                        out,
                    );
                }
                (Some(a), Some(b)) => {
                    let av = a.clone();
                    not_comparable_runway(
                        runway,
                        "surface",
                        official,
                        fr,
                        av,
                        b.clone(),
                        "different land-surface classifications or publication dates; retained without blocking",
                        out,
                    );
                }
                (None, Some(b)) => {
                    runway.surface = Some(b.clone());
                    recover_runway(
                        runway,
                        "surface",
                        official,
                        fr,
                        &fr.surface_raw,
                        b,
                        "official FAA surface type",
                        out,
                    );
                }
                _ => {}
            }
            if runway.width_ft.is_none() {
                runway.width_ft = fr.width_ft;
            }
            if runway.le_latitude.is_none() {
                runway.le_latitude = fr.low_latitude;
                runway.le_longitude = fr.low_longitude;
                runway.he_latitude = fr.high_latitude;
                runway.he_longitude = fr.high_longitude;
            }
            draft.quarantine.retain(|q| {
                !(q.affects_airport
                    && q.record_id.as_deref() == Some(runway.id.as_str())
                    && matches!(
                        q.category,
                        QuarantineCategory::IncompleteLandRunway | QuarantineCategory::UnknownType
                    ))
            });
        } else if let (Some(length), Some(surface)) = (fr.length_ft, fr.surface.clone()) {
            let id = format!("FAA:{}:{}", official.site_no, fr.id);
            let mut evidence = BTreeMap::new();
            recover(
                &mut evidence,
                "id",
                official,
                &fr.id,
                &id,
                "official FAA runway recovered",
                out,
                Some(&fr.id),
            );
            recover(
                &mut evidence,
                "length_ft",
                official,
                &length.to_string(),
                &length.to_string(),
                "official FAA physical runway length",
                out,
                Some(&fr.id),
            );
            recover(
                &mut evidence,
                "surface",
                official,
                &fr.surface_raw,
                &surface,
                "official FAA surface type",
                out,
                Some(&fr.id),
            );
            draft.runways.push(RunwayDraft {
                id,
                length_ft: Some(length),
                width_ft: fr.width_ft,
                surface: Some(surface),
                le_ident: fr.low_ident.clone(),
                he_ident: fr.high_ident.clone(),
                le_latitude: fr.low_latitude,
                le_longitude: fr.low_longitude,
                he_latitude: fr.high_latitude,
                he_longitude: fr.high_longitude,
                kind: RunwayKind::LandRunway,
                classification_reason: "официальная FAA APT_RWY land runway".into(),
                provenance: BTreeMap::from([
                    ("source".into(), "FAA/NASR APT_RWY".into()),
                    ("faa_runway_id".into(), fr.id.clone()),
                ]),
                evidence,
                ..Default::default()
            });
            out.statistics.recovered_airports += usize::from(draft.runways.len() == 1);
        }
    }
}

fn evidence(
    official: &FaaAirport,
    raw: &str,
    normalized: &str,
    method: EvidenceMethod,
    rule: &str,
    conflict: Option<String>,
    runway: Option<&str>,
) -> Evidence {
    Evidence {
        source: "FAA/NASR APT".into(),
        source_record_id: Some(match runway {
            Some(r) => format!("{}:{}:{r}", official.site_no, official.site_type),
            None => format!("{}:{}", official.site_no, official.site_type),
        }),
        retrieved_at: None,
        effective_at: Some(official.effective_date.clone()),
        raw_value: raw.into(),
        normalized_value: normalized.into(),
        method,
        validation: rule.into(),
        conflict,
    }
}
fn confirm(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    field: &str,
    o: &FaaAirport,
    raw: &str,
    norm: &str,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    map.entry(field.into()).or_default().push(evidence(
        o,
        raw,
        norm,
        EvidenceMethod::IndependentlyVerified,
        rule,
        None,
        None,
    ));
    out.statistics.independently_verified_values += 1;
}
fn recover(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    field: &str,
    o: &FaaAirport,
    raw: &str,
    norm: &str,
    rule: &str,
    out: &mut FaaArtifacts,
    r: Option<&str>,
) {
    map.entry(field.into()).or_default().push(evidence(
        o,
        raw,
        norm,
        EvidenceMethod::Recovered,
        rule,
        None,
        r,
    ));
    out.statistics.recovered_values += 1;
    out.recovered.push(FaaDecisionRow {
        icao: o.icao.clone(),
        runway_id: r.map(str::to_owned),
        field: field.into(),
        ourairports_value: String::new(),
        faa_value: norm.into(),
        decision: "Recovered".into(),
        reason: rule.into(),
    });
}
fn conflict(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    field: &str,
    o: &FaaAirport,
    oa: String,
    faa: String,
    rule: &str,
    out: &mut FaaArtifacts,
    r: Option<&str>,
) {
    map.entry(field.into()).or_default().push(evidence(
        o,
        &faa,
        &faa,
        EvidenceMethod::Conflicting,
        rule,
        Some(format!("OurAirports={oa}")),
        r,
    ));
    out.statistics.conflicts += 1;
    out.conflicts.push(FaaDecisionRow {
        icao: o.icao.clone(),
        runway_id: r.map(str::to_owned),
        field: field.into(),
        ourairports_value: oa,
        faa_value: faa,
        decision: "Conflict".into(),
        reason: rule.into(),
    });
}
fn confirm_runway(
    r: &mut RunwayDraft,
    f: &str,
    o: &FaaAirport,
    fr: &FaaRunway,
    raw: &str,
    n: &str,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    confirm_with_runway(&mut r.evidence, f, o, fr, raw, n, rule, out)
}
fn confirm_with_runway(
    map: &mut BTreeMap<String, Vec<Evidence>>,
    f: &str,
    o: &FaaAirport,
    fr: &FaaRunway,
    raw: &str,
    n: &str,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    map.entry(f.into()).or_default().push(evidence(
        o,
        raw,
        n,
        EvidenceMethod::IndependentlyVerified,
        rule,
        None,
        Some(&fr.id),
    ));
    out.statistics.independently_verified_values += 1;
}
fn recover_runway(
    r: &mut RunwayDraft,
    f: &str,
    o: &FaaAirport,
    fr: &FaaRunway,
    raw: &str,
    n: &str,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    recover(&mut r.evidence, f, o, raw, n, rule, out, Some(&fr.id));
}
fn conflict_runway(
    r: &mut RunwayDraft,
    f: &str,
    o: &FaaAirport,
    fr: &FaaRunway,
    oa: String,
    faa: String,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    conflict(&mut r.evidence, f, o, oa, faa, rule, out, Some(&fr.id));
}
fn not_comparable_runway(
    r: &mut RunwayDraft,
    f: &str,
    o: &FaaAirport,
    fr: &FaaRunway,
    oa: String,
    faa: String,
    rule: &str,
    out: &mut FaaArtifacts,
) {
    r.evidence.entry(f.into()).or_default().push(evidence(
        o,
        &faa,
        &faa,
        EvidenceMethod::NotComparable,
        rule,
        Some(format!("OurAirports={oa}")),
        Some(&fr.id),
    ));
    out.conflicts.push(FaaDecisionRow {
        icao: o.icao.clone(),
        runway_id: Some(fr.id.clone()),
        field: f.into(),
        ourairports_value: oa,
        faa_value: faa,
        decision: "NotComparable".into(),
        reason: rule.into(),
    });
}
fn canonical(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let (Some(a), Some(b)) = (a, b) else {
        return None;
    };
    let mut v = [a.trim().to_ascii_uppercase(), b.trim().to_ascii_uppercase()];
    if v[0].is_empty() || v[1].is_empty() {
        return None;
    }
    v.sort();
    Some(v.join("/"))
}
fn surface_family(value: &str) -> String {
    value
        .to_ascii_uppercase()
        .split(['-', '/'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && !matches!(*s, "G" | "F" | "P" | "E"))
        .map(|s| match s {
            "ASP" | "ASPHALT" | "BIT" => "ASPH",
            "CON" | "CONCRETE" => "CONC",
            "TURF" | "SOD" | "GRS" | "GRE" => "GRASS",
            "GVL" | "GRVL" => "GRAVEL",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("/")
}
fn surfaces_compatible(a: &str, b: &str) -> bool {
    let a = surface_family(a);
    let b = surface_family(b);
    a == b
        || a == "PEM" && matches!(b.as_str(), "ASPH" | "CONC" | "ASPH/CONC")
        || b == "PEM" && matches!(a.as_str(), "ASPH" | "CONC" | "ASPH/CONC")
}
fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}
fn in_faa_scope(country: &str) -> bool {
    matches!(
        country.trim().to_ascii_uppercase().as_str(),
        "US" | "AS" | "GU" | "MP" | "PR" | "VI" | "FM" | "MH" | "PW" | "UM"
    )
}
fn haversine_km(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let p1 = a.to_radians();
    let p2 = c.to_radians();
    let dp = (c - a).to_radians();
    let dl = (d - b).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0088 * 2.0 * h.sqrt().asin()
}

pub fn write_artifacts(out: &FaaArtifacts, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
    write_csv(&dir.join("faa_matches.csv"), &out.matches)?;
    write_csv(&dir.join("faa_recovered.csv"), &out.recovered)?;
    write_csv(&dir.join("faa_conflicts.csv"), &out.conflicts)?;
    write_csv(&dir.join("faa_unmatched.csv"), &out.unmatched)?;
    write_csv(&dir.join("faa_candidate_assessment.csv"), &out.candidates)?;
    Ok(())
}
fn write_csv<T: Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    let mut w = csv::Writer::from_path(path)?;
    for r in rows {
        w.serialize(r)?
    }
    w.flush().map_err(|e| io_error(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EvidenceMap, QuarantineRecord};

    fn official(icao: &str, runway: FaaRunway) -> FaaDataset {
        FaaDataset {
            release: "2026-10-01".into(),
            airports: vec![FaaAirport {
                effective_date: "2026-10-01".into(),
                site_no: "1.".into(),
                site_type: "A".into(),
                arpt_id: icao.trim_start_matches('K').into(),
                icao: icao.into(),
                name: "TEST AIRPORT".into(),
                country_code: "US".into(),
                latitude: 30.0,
                longitude: -104.0,
                elevation_ft: Some(1000.4),
                status: Some(AirportStatus::Open),
                runways: vec![runway],
            }],
        }
    }
    fn runway(id: &str, length: Option<f64>, surface: Option<&str>) -> FaaRunway {
        let (a, b) = split_designation(id);
        FaaRunway {
            id: id.into(),
            length_ft: length,
            width_ft: Some(75.0),
            surface_raw: surface.unwrap_or("").into(),
            surface: surface.map(str::to_owned),
            low_ident: a,
            high_ident: b,
            low_latitude: None,
            low_longitude: None,
            high_latitude: None,
            high_longitude: None,
            kind: RunwayKind::LandRunway,
        }
    }
    fn draft(icao: &str) -> AirportDraft {
        AirportDraft {
            icao: icao.into(),
            latitude: Some(30.0),
            longitude: Some(-104.0),
            status: Some(AirportStatus::Open),
            airport_type: "small_airport".into(),
            country: Some("United States".into()),
            elevation_ft: None,
            id: Some(1),
            name: "Test Airport".into(),
            evidence: EvidenceMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_units_identifiers_and_site_join_from_official_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("APT_BASE.csv"),"EFF_DATE,SITE_NO,SITE_TYPE_CODE,ARPT_ID,ICAO_ID,ARPT_NAME,COUNTRY_CODE,LAT_DECIMAL,LONG_DECIMAL,ELEV,ARPT_STATUS\n2026/10/01,1.,A,MRF,KMRF,MARFA MUNI,US,30,-104,4848.8,O\n").unwrap();
        std::fs::write(dir.path().join("APT_RWY.csv"),"EFF_DATE,SITE_NO,SITE_TYPE_CODE,RWY_ID,RWY_LEN,RWY_WIDTH,SURFACE_TYPE_CODE\n2026/10/01,1.,A,09/27,2825,60,DIRT\n").unwrap();
        std::fs::write(dir.path().join("APT_RWY_END.csv"),"EFF_DATE,SITE_NO,SITE_TYPE_CODE,RWY_ID,RWY_END_ID,LAT_DECIMAL,LONG_DECIMAL\n2026/10/01,1.,A,09/27,09,30.1,-104.1\n2026/10/01,1.,A,09/27,27,30.2,-104.2\n").unwrap();
        let data = parse_dataset(dir.path(), "2026-10-01").unwrap();
        assert_eq!(data.airports[0].icao, "KMRF");
        assert_eq!(data.airports[0].arpt_id, "MRF");
        assert_eq!(data.airports[0].runways[0].length_ft, Some(2825.0));
        assert_eq!(data.airports[0].runways[0].low_latitude, Some(30.1));
    }

    #[test]
    fn rejects_incompatible_schema_and_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("APT_BASE.csv");
        std::fs::write(&p, "SITE_NO,BAD\n1,x\n").unwrap();
        assert!(validate_schema(&p, REQUIRED[0].1).is_err());
    }
    #[test]
    fn canonical_runway_matching_accepts_reversed_ends() {
        assert_eq!(
            canonical(Some("09"), Some("27")),
            canonical(Some("27"), Some("09"))
        );
    }
    #[test]
    fn distinguishes_water_and_composite_land_surfaces() {
        assert_eq!(normalize_faa_surface("WATER"), Some("WATER".into()));
        assert_eq!(normalize_faa_surface("ASPH/CONC"), Some("ASPH/CONC".into()));
        let mut d = RunwayDraft {
            id: "x".into(),
            length_ft: Some(1000.0),
            surface: Some("WATER".into()),
            ..Default::default()
        };
        assert_eq!(classify_runway("small_airport", &d).0, RunwayKind::Waterway);
        d.surface = Some("ASPH/CONC".into());
        assert_eq!(
            classify_runway("small_airport", &d).0,
            RunwayKind::LandRunway
        );
    }

    #[test]
    fn kmrf_recovers_elevation_length_surface_and_missing_runway() {
        let mut d = draft("KMRF");
        d.runways.push(RunwayDraft {
            id: "oa-1".into(),
            le_ident: Some("27".into()),
            he_ident: Some("09".into()),
            kind: RunwayKind::LandRunway,
            ..Default::default()
        });
        d.quarantine.push(QuarantineRecord {
            source: "fixture".into(),
            row_number: None,
            airport_id: Some(1),
            icao: Some("KMRF".into()),
            record_id: Some("oa-1".into()),
            category: QuarantineCategory::IncompleteLandRunway,
            reason: "missing".into(),
            affects_airport: true,
            raw: None,
        });
        let f = official("KMRF", runway("09/27", Some(2825.0), Some("DIRT")));
        let out = apply(std::slice::from_mut(&mut d), &f);
        assert_eq!(d.elevation_ft, Some(1000));
        assert_eq!(d.runways[0].length_ft, Some(2825.0));
        assert_eq!(d.runways[0].surface.as_deref(), Some("DIRT"));
        assert!(d.quarantine.is_empty());
        assert!(out.statistics.recovered_values >= 3);
        let f2 = official("KMRF", runway("13/31", Some(6203.0), Some("ASPH")));
        apply(std::slice::from_mut(&mut d), &f2);
        assert_eq!(d.runways.len(), 2);
    }

    #[test]
    fn ksea_matching_does_not_duplicate_runways() {
        let mut d = draft("KSEA");
        d.elevation_ft = Some(1000);
        d.runways.push(RunwayDraft {
            id: "oa".into(),
            length_ft: Some(11901.0),
            surface: Some("PEM".into()),
            le_ident: Some("16L".into()),
            he_ident: Some("34R".into()),
            kind: RunwayKind::LandRunway,
            ..Default::default()
        });
        let f = official("KSEA", runway("16L/34R", Some(11901.0), Some("CONC")));
        let out = apply(std::slice::from_mut(&mut d), &f);
        assert_eq!(d.runways.len(), 1);
        assert_eq!(out.statistics.matched_runways, 1);
        assert!(
            d.runways[0].evidence["surface"]
                .iter()
                .any(|e| e.method == EvidenceMethod::IndependentlyVerified)
        );
    }

    #[test]
    fn confirms_only_values_with_field_specific_agreement() {
        let mut d = draft("KAAA");
        d.elevation_ft = Some(1000);
        d.runways.push(RunwayDraft {
            id: "oa".into(),
            length_ft: Some(1000.0),
            surface: Some("ASP".into()),
            le_ident: Some("09".into()),
            he_ident: Some("27".into()),
            kind: RunwayKind::LandRunway,
            ..Default::default()
        });
        let out = apply(
            std::slice::from_mut(&mut d),
            &official("KAAA", runway("09/27", Some(1025.0), Some("ASPH"))),
        );
        assert!(out.statistics.independently_verified_values >= 4);
        assert!(
            d.runways[0].evidence["length_ft"]
                .iter()
                .any(|e| e.method == EvidenceMethod::IndependentlyVerified)
        );
    }

    #[test]
    fn detects_serious_length_conflict_and_date_is_preserved() {
        let mut d = draft("KAAA");
        d.runways.push(RunwayDraft {
            id: "oa".into(),
            length_ft: Some(2000.0),
            surface: Some("ASPH".into()),
            le_ident: Some("09".into()),
            he_ident: Some("27".into()),
            kind: RunwayKind::LandRunway,
            ..Default::default()
        });
        let out = apply(
            std::slice::from_mut(&mut d),
            &official("KAAA", runway("09/27", Some(1000.0), Some("ASPH"))),
        );
        assert_eq!(out.statistics.conflicts, 1);
        let e = d.runways[0].evidence["length_ft"].last().unwrap();
        assert_eq!(e.method, EvidenceMethod::Conflicting);
        assert_eq!(e.effective_at.as_deref(), Some("2026-10-01"));
    }

    #[test]
    fn ambiguous_duplicate_official_icao_is_not_selected() {
        let mut data = official("KAAA", runway("09/27", Some(1000.0), Some("ASPH")));
        let mut copy = data.airports[0].clone();
        copy.site_no = "2.".into();
        data.airports.push(copy);
        let mut d = draft("KAAA");
        let out = apply(std::slice::from_mut(&mut d), &data);
        assert_eq!(out.statistics.ambiguous_matches, 1);
        assert!(!d.provenance.contains_key("faa_site_no"));
    }

    #[test]
    fn corrupted_archive_and_path_traversal_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.zip");
        std::fs::write(&bad, b"not a zip").unwrap();
        assert!(extract_faa_archive(&bad, &dir.path().join("out")).is_err());
        let zip_path = dir.path().join("evil.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../APT_BASE.csv", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        assert!(extract_faa_archive(&zip_path, &dir.path().join("out2")).is_err());
    }

    #[tokio::test]
    async fn offline_mode_requires_exact_release_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::from_root(dir.path());
        c.offline = true;
        c.faa_release = "2026-10-01".into();
        assert!(matches!(prepare(&c).await, Err(Error::MissingCache(_))));
    }

    #[test]
    fn adds_complete_airport_absent_from_ourairports_with_stable_id() {
        let data = official("KNEW", runway("09/27", Some(3000.0), Some("ASPH")));
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, Some(stable_faa_id("1.:A")));
        assert_eq!(out.statistics.new_airports_added, 1);
        assert!(
            drafts[0].evidence["icao"]
                .iter()
                .any(|e| e.method == EvidenceMethod::Recovered)
        );
    }

    #[test]
    fn new_airport_keeps_multiple_physical_runways() {
        let mut data = official("KNEW", runway("09/27", Some(3000.0), Some("ASPH")));
        data.airports[0]
            .runways
            .push(runway("18/36", Some(4200.0), Some("CONC")));
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert_eq!(drafts[0].runways.len(), 2);
    }

    #[test]
    fn incomplete_or_unknown_surface_new_airport_is_not_added() {
        let data = official("KNEW", runway("09/27", Some(3000.0), None));
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert!(drafts.is_empty());
        assert_eq!(out.statistics.unmatched_insufficient, 1);
    }

    #[test]
    fn duplicate_new_icao_is_ambiguous_not_last_wins() {
        let mut data = official("KNEW", runway("09/27", Some(3000.0), Some("ASPH")));
        let mut second = data.airports[0].clone();
        second.site_no = "2.".into();
        data.airports.push(second);
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert!(drafts.is_empty());
        assert!(
            out.candidates
                .iter()
                .all(|c| c.category == "ambiguous_icao")
        );
    }

    #[test]
    fn closed_official_airport_never_becomes_open() {
        let mut data = official("KNEW", runway("09/27", Some(3000.0), Some("ASPH")));
        data.airports[0].status = Some(AirportStatus::Close);
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert_eq!(drafts[0].status, Some(AirportStatus::Close));
        assert_eq!(drafts[0].airport_type, "closed");
    }

    #[test]
    fn foreign_faa_record_is_classified_outside_scope() {
        let mut data = official("CNEW", runway("09/27", Some(3000.0), Some("ASPH")));
        data.airports[0].country_code = "CA".into();
        let mut drafts = Vec::new();
        let mut out = FaaArtifacts::default();
        add_new_airports(&mut drafts, &data, &mut out);
        assert!(drafts.is_empty());
        assert_eq!(out.statistics.unmatched_out_of_scope, 1);
    }

    #[test]
    fn stable_ids_do_not_depend_on_processing_order() {
        assert_eq!(stable_faa_id("123.:A"), stable_faa_id("123.:A"));
        assert_ne!(stable_faa_id("123.:A"), stable_faa_id("124.:A"));
        assert_eq!(stable_faa_id("123.:A") >> 60, 0xe);
    }
}

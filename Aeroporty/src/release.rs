//! Phase 6: audited, atomic publication of the final dataset release.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::{Error, Result, io_error},
    export::{ExportFormat, export_dataset, read_dataset_json, verify_exports},
    models::{
        Airport, Dataset, Evidence, EvidenceMethod, Report, Runway, RunwayKind, SourcePolicyRecord,
    },
    runway_classification::is_unknown_surface,
    validation::validate_airport,
};

const RELEASE_DATE: &str = "2026-10-12";
const EXPORT_NAMES: [&str; 4] = [
    "airports.csv",
    "airports.json",
    "airports.toml",
    "airports.yaml",
];
const AIRPORT_FIELDS: [&str; 11] = [
    "icao",
    "latitude",
    "longitude",
    "status",
    "type",
    "country",
    "elevation_ft",
    "timezone",
    "runways_count",
    "id",
    "name",
];
const RUNWAY_FIELDS: [&str; 3] = ["id", "length_ft", "surface"];
const ALLOWED_TYPES: [&str; 6] = [
    "small_airport",
    "medium_airport",
    "large_airport",
    "closed",
    "closed_airport",
    "airport",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub application: String,
    pub application_version: String,
    pub release_date: String,
    pub source_dataset: String,
    pub airports: usize,
    pub physical_runways: usize,
    pub independently_verified_values: usize,
    pub timezone_database: String,
    pub export_schema: ExportSchema,
    pub reproduction_command: String,
    pub current_source_update_command: String,
    pub sources: Vec<crate::models::SourceInfo>,
    pub source_policies: Vec<SourcePolicyRecord>,
    pub files: Vec<ReleaseFile>,
    pub manifest_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSchema {
    pub airport_fields: Vec<String>,
    pub runway_fields: Vec<String>,
    pub units: BTreeMap<String, String>,
    pub csv_shape: String,
    pub nested_shape: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseFile {
    pub name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalValidationReport {
    pub release_date: String,
    pub passed: bool,
    pub airports: usize,
    pub physical_runways: usize,
    pub mandatory_values_checked: usize,
    pub independently_verified_values: usize,
    pub unique_icao: bool,
    pub unique_id: bool,
    pub export_structural_equivalence: bool,
    pub unresolved_serious_conflicts: usize,
    pub resolved_conflicts_checked: usize,
    pub resolved_conflicts_with_history_preserved: usize,
    pub checks: Vec<ValidationCheck>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub name: String,
    pub passed: bool,
    pub checked: usize,
    pub failures: usize,
}

#[derive(Debug, Deserialize)]
struct ConflictResolution {
    icao: String,
    runway: String,
    confirmed_value: f64,
}

#[derive(Default)]
struct AuditCounters {
    invalid_airports: usize,
    duplicate_icao: usize,
    duplicate_id: usize,
    invalid_coordinates: usize,
    invalid_types: usize,
    missing_country: usize,
    unmeasured_elevation: usize,
    invalid_timezone: usize,
    invalid_status_evidence: usize,
    runway_count_mismatch: usize,
    invalid_runway_length: usize,
    invalid_surface: usize,
    non_land_runway: usize,
    duplicate_runway: usize,
    missing_evidence: usize,
    unresolved_evidence: usize,
    missing_provenance: usize,
}

pub fn create(root: &Path, input: &Path, destination: &Path) -> Result<ReleaseManifest> {
    ensure_inside(root, input)?;
    ensure_inside(root, destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Data("release destination has no parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    let stage = parent.join(".release-stage");
    let backup = parent.join(".release-previous");
    remove_internal_dir(root, &stage)?;
    remove_internal_dir(root, &backup)?;
    std::fs::create_dir(&stage).map_err(|e| io_error(&stage, e))?;

    let dataset = read_dataset_json(&input.join("airports.json"))?;
    let report: Report = read_json(&input.join("report.json"))?;
    let mut validation = audit_dataset(root, input, &dataset, &report)?;
    export_dataset(&dataset, &stage, &ExportFormat::ALL)?;
    let round_trip = verify_exports(&stage)?;
    validation.export_structural_equivalence = round_trip == dataset;
    validation.passed &= validation.export_structural_equivalence;
    if !validation.passed {
        return Err(Error::Data(
            "финальный аудит не пройден; release не опубликован".into(),
        ));
    }
    write_json(&stage.join("validation_report.json"), &validation)?;

    let mut files = Vec::new();
    for name in EXPORT_NAMES
        .into_iter()
        .chain(std::iter::once("validation_report.json"))
    {
        files.push(file_record(&stage.join(name), name)?);
    }
    let source_policies = read_source_policies(&input.join("sources_and_licenses.csv"))?;
    let manifest = ReleaseManifest {
        application: "Aeroporty".into(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        release_date: RELEASE_DATE.into(),
        source_dataset: "output/candidate-phase5".into(),
        airports: dataset.airports.len(),
        physical_runways: dataset.airports.iter().map(|a| a.runways.len()).sum(),
        independently_verified_values: report.statistics.attributes.independently_verified,
        timezone_database: report.timezone_dataset_version,
        export_schema: schema(),
        reproduction_command:
            "cargo run --release -- run --offline && cargo run --release -- release".into(),
        current_source_update_command:
            "cargo run --release -- update (may produce a newer, different snapshot)".into(),
        sources: normalize_sources(root, report.sources),
        source_policies,
        files,
        manifest_note:
            "manifest.json is not self-hashed; every other release file is covered below".into(),
    };
    write_json(&stage.join("manifest.json"), &manifest)?;
    verify_manifest(&stage, &manifest)?;
    publish(root, &stage, destination, &backup)?;
    Ok(manifest)
}

fn audit_dataset(
    root: &Path,
    input: &Path,
    dataset: &Dataset,
    report: &Report,
) -> Result<FinalValidationReport> {
    let mut counters = AuditCounters::default();
    let mut icaos = HashSet::new();
    let mut ids = HashSet::new();
    let mut mandatory = 0usize;
    for airport in &dataset.airports {
        counters.invalid_airports += usize::from(!validate_airport(airport).is_empty());
        counters.duplicate_icao += usize::from(!icaos.insert(airport.icao.as_str()));
        counters.duplicate_id += usize::from(!ids.insert(airport.id));
        counters.invalid_coordinates += usize::from(!crate::geo::valid_coordinate(
            airport.latitude,
            airport.longitude,
        ));
        counters.invalid_types +=
            usize::from(!ALLOWED_TYPES.contains(&airport.airport_type.as_str()));
        counters.missing_country += usize::from(airport.country.trim().is_empty());
        counters.unmeasured_elevation += usize::from(!has_measured_elevation(airport));
        counters.invalid_timezone +=
            usize::from(airport.timezone.parse::<chrono_tz::Tz>().is_err());
        counters.invalid_status_evidence += usize::from(!has_status_evidence(airport));
        counters.runway_count_mismatch +=
            usize::from(airport.runways_count != airport.runways.len());
        counters.missing_provenance += usize::from(airport.provenance.is_empty());
        mandatory += AIRPORT_FIELDS.len();
        audit_evidence(
            &airport.evidence,
            &AIRPORT_FIELDS,
            &mut counters.missing_evidence,
            &mut counters.unresolved_evidence,
        );
        audit_runways(airport, &mut counters);
        mandatory += RUNWAY_FIELDS.len() * airport.runways.len();
    }
    let (resolved, history_preserved) = audit_resolved_conflicts(root, input, dataset)?;
    let unresolved_serious = report.conflicts.len()
        + report
            .phase5
            .as_ref()
            .map_or(0, |value| value.unresolved_conflicts)
        + counters.unresolved_evidence;
    let physical_runways = dataset.airports.iter().map(|a| a.runways.len()).sum();
    let mut checks = vec![
        check(
            "strict airport validation",
            dataset.airports.len(),
            counters.invalid_airports,
        ),
        check(
            "unique ICAO",
            dataset.airports.len(),
            counters.duplicate_icao,
        ),
        check(
            "unique airport ID",
            dataset.airports.len(),
            counters.duplicate_id,
        ),
        check(
            "geographic ranges",
            dataset.airports.len(),
            counters.invalid_coordinates,
        ),
        check(
            "supported airport types",
            dataset.airports.len(),
            counters.invalid_types,
        ),
        check(
            "non-empty country",
            dataset.airports.len(),
            counters.missing_country,
        ),
        check(
            "measured elevation evidence",
            dataset.airports.len(),
            counters.unmeasured_elevation,
        ),
        check(
            "IANA timezone",
            dataset.airports.len(),
            counters.invalid_timezone,
        ),
        check(
            "documented open/close semantics",
            dataset.airports.len(),
            counters.invalid_status_evidence,
        ),
        check(
            "physical runway count",
            dataset.airports.len(),
            counters.runway_count_mismatch,
        ),
        check(
            "positive runway lengths",
            physical_runways,
            counters.invalid_runway_length,
        ),
        check(
            "known land surfaces",
            physical_runways,
            counters.invalid_surface,
        ),
        check(
            "no helipad/waterway in strict export",
            physical_runways,
            counters.non_land_runway,
        ),
        check(
            "no duplicate physical runway",
            physical_runways,
            counters.duplicate_runway,
        ),
        check(
            "mandatory evidence present",
            mandatory,
            counters.missing_evidence,
        ),
        check(
            "no unresolved mandatory evidence",
            mandatory,
            counters.unresolved_evidence,
        ),
        check(
            "provenance retained",
            dataset.airports.len() + physical_runways,
            counters.missing_provenance,
        ),
        check(
            "eight resolved conflicts still match",
            8,
            8usize.saturating_sub(resolved),
        ),
        check(
            "historical conflict evidence retained",
            8,
            8usize.saturating_sub(history_preserved),
        ),
    ];
    let stats_failures = usize::from(report.statistics.accepted != dataset.airports.len())
        + usize::from(report.statistics.physical_runways != physical_runways)
        + usize::from(report.statistics.attributes.filled != mandatory);
    checks.push(check("report statistics reconciliation", 3, stats_failures));
    let passed = checks.iter().all(|value| value.passed) && unresolved_serious == 0;
    Ok(FinalValidationReport {
        release_date: RELEASE_DATE.into(),
        passed,
        airports: dataset.airports.len(),
        physical_runways,
        mandatory_values_checked: mandatory,
        independently_verified_values: report.statistics.attributes.independently_verified,
        unique_icao: counters.duplicate_icao == 0,
        unique_id: counters.duplicate_id == 0,
        export_structural_equivalence: false,
        unresolved_serious_conflicts: unresolved_serious,
        resolved_conflicts_checked: resolved,
        resolved_conflicts_with_history_preserved: history_preserved,
        checks,
        limitations: report.limitations.clone(),
    })
}

fn audit_runways(airport: &Airport, counters: &mut AuditCounters) {
    let mut ids = HashSet::new();
    let mut designations = HashSet::new();
    for runway in &airport.runways {
        counters.invalid_runway_length +=
            usize::from(!runway.length_ft.is_finite() || runway.length_ft <= 0.0);
        counters.invalid_surface += usize::from(invalid_surface(runway));
        counters.non_land_runway += usize::from(runway.kind != RunwayKind::LandRunway);
        counters.duplicate_runway += usize::from(!ids.insert(runway.id.as_str()));
        if let Some(designation) = runway_designation(runway) {
            counters.duplicate_runway += usize::from(!designations.insert(designation));
        }
        counters.missing_provenance += usize::from(runway.provenance.is_empty());
        audit_evidence(
            &runway.evidence,
            &RUNWAY_FIELDS,
            &mut counters.missing_evidence,
            &mut counters.unresolved_evidence,
        );
    }
}

fn audit_evidence(
    evidence: &crate::models::EvidenceMap,
    fields: &[&str],
    missing: &mut usize,
    unresolved: &mut usize,
) {
    for field in fields {
        match evidence.get(*field) {
            None => *missing += 1,
            Some(items) => {
                if !items.iter().any(is_resolved) {
                    *unresolved += 1;
                }
                if items.iter().any(|item| {
                    matches!(
                        item.method,
                        EvidenceMethod::Conflicting | EvidenceMethod::Unresolved
                    )
                }) {
                    *unresolved += 1;
                }
            }
        }
    }
}

fn is_resolved(evidence: &Evidence) -> bool {
    matches!(
        evidence.method,
        EvidenceMethod::SourceReported
            | EvidenceMethod::Derived
            | EvidenceMethod::IndependentlyVerified
            | EvidenceMethod::Recovered
    )
}

fn has_measured_elevation(airport: &Airport) -> bool {
    airport.evidence.get("elevation_ft").is_some_and(|items| {
        items.iter().any(|item| {
            !item.raw_value.trim().is_empty()
                && matches!(
                    item.method,
                    EvidenceMethod::SourceReported
                        | EvidenceMethod::IndependentlyVerified
                        | EvidenceMethod::Recovered
                )
        })
    })
}

fn has_status_evidence(airport: &Airport) -> bool {
    let expected = match airport.status {
        crate::models::AirportStatus::Open => "open",
        crate::models::AirportStatus::Close => "close",
    };
    airport.evidence.get("status").is_some_and(|items| {
        items.iter().any(|item| {
            is_resolved(item)
                && item.normalized_value.eq_ignore_ascii_case(expected)
                && !item.validation.trim().is_empty()
        })
    })
}

fn invalid_surface(runway: &Runway) -> bool {
    let surface = runway.surface.trim().to_ascii_uppercase();
    is_unknown_surface(&surface)
        || surface.contains("WATER")
        || matches!(surface.as_str(), "WTR" | "SEA" | "LAKE" | "RIVER")
}

fn audit_resolved_conflicts(
    _root: &Path,
    input: &Path,
    dataset: &Dataset,
) -> Result<(usize, usize)> {
    let records: Vec<ConflictResolution> =
        csv::Reader::from_path(input.join("conflict_resolution.csv"))?
            .deserialize()
            .collect::<std::result::Result<_, _>>()?;
    if records.len() != 8 {
        return Err(Error::Data(format!(
            "expected 8 Phase 5 conflict resolutions, found {}",
            records.len()
        )));
    }
    let mut matched = 0;
    let mut history = 0;
    for record in records {
        let Some(airport) = dataset.airports.iter().find(|a| a.icao == record.icao) else {
            continue;
        };
        let wanted = canonical_designation(&record.runway);
        let Some(runway) = airport
            .runways
            .iter()
            .find(|r| runway_designation(r).as_deref() == Some(wanted.as_str()))
        else {
            continue;
        };
        if (runway.length_ft - record.confirmed_value).abs() < 0.02 {
            matched += 1;
        }
        let evidence = runway.evidence.get("length_ft");
        if evidence.is_some_and(|items| {
            items
                .iter()
                .any(|item| item.method == EvidenceMethod::NotComparable)
                && items
                    .iter()
                    .any(|item| item.method == EvidenceMethod::Recovered)
        }) {
            history += 1;
        }
    }
    Ok((matched, history))
}

fn runway_designation(runway: &Runway) -> Option<String> {
    Some(canonical_pair(
        runway.le_ident.as_deref()?,
        runway.he_ident.as_deref()?,
    ))
}

fn canonical_designation(value: &str) -> String {
    let mut parts = value.split('/');
    canonical_pair(parts.next().unwrap_or(""), parts.next().unwrap_or(""))
}

fn canonical_pair(a: &str, b: &str) -> String {
    let mut values = [a.trim().to_ascii_uppercase(), b.trim().to_ascii_uppercase()];
    values.sort();
    values.join("/")
}

fn check(name: &str, checked: usize, failures: usize) -> ValidationCheck {
    ValidationCheck {
        name: name.into(),
        passed: failures == 0,
        checked,
        failures,
    }
}

fn schema() -> ExportSchema {
    ExportSchema {
        airport_fields: AIRPORT_FIELDS.iter().map(|value| (*value).into()).collect(),
        runway_fields: RUNWAY_FIELDS.iter().map(|value| (*value).into()).collect(),
        units: BTreeMap::from([
            ("latitude/longitude".into(), "decimal degrees (WGS84)".into()),
            ("elevation_ft".into(), "feet above mean sea level".into()),
            ("runway.length_ft".into(), "feet".into()),
            ("runways_count".into(), "physical land runways".into()),
        ]),
        csv_shape: "one row per physical runway; airport columns repeat; provenance and evidence are lossless JSON columns".into(),
        nested_shape: "JSON, TOML and YAML contain airports with nested runways".into(),
    }
}

fn read_source_policies(path: &Path) -> Result<Vec<SourcePolicyRecord>> {
    Ok(csv::Reader::from_path(path)?
        .deserialize()
        .collect::<std::result::Result<_, _>>()?)
}

fn normalize_sources(
    root: &Path,
    mut sources: Vec<crate::models::SourceInfo>,
) -> Vec<crate::models::SourceInfo> {
    for source in &mut sources {
        let path = Path::new(&source.local_file);
        if let Ok(relative) = path.strip_prefix(root) {
            source.local_file = relative.to_string_lossy().replace('\\', "/");
        }
    }
    sources
}

fn file_record(path: &Path, name: &str) -> Result<ReleaseFile> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    Ok(ReleaseFile {
        name: name.into(),
        size_bytes: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn verify_manifest(directory: &Path, manifest: &ReleaseManifest) -> Result<()> {
    for expected in &manifest.files {
        let actual = file_record(&directory.join(&expected.name), &expected.name)?;
        if actual.size_bytes != expected.size_bytes || actual.sha256 != expected.sha256 {
            return Err(Error::Data(format!(
                "release checksum mismatch for {}",
                expected.name
            )));
        }
    }
    Ok(())
}

fn publish(root: &Path, stage: &Path, destination: &Path, backup: &Path) -> Result<()> {
    ensure_inside(root, stage)?;
    ensure_inside(root, destination)?;
    ensure_inside(root, backup)?;
    if destination.exists() {
        std::fs::rename(destination, backup).map_err(|e| io_error(destination, e))?;
    }
    if let Err(error) = std::fs::rename(stage, destination) {
        if backup.exists() {
            let _ = std::fs::rename(backup, destination);
        }
        return Err(io_error(destination, error));
    }
    remove_internal_dir(root, backup)
}

fn remove_internal_dir(root: &Path, path: &Path) -> Result<()> {
    ensure_inside(root, path)?;
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|e| io_error(path, e))?;
    }
    Ok(())
}

fn ensure_inside(root: &Path, path: &Path) -> Result<()> {
    let root = root.canonicalize().map_err(|e| io_error(root, e))?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let parent: PathBuf = absolute
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| Error::Data(format!("cannot resolve {}", path.display())))?
        .canonicalize()
        .map_err(|e| io_error(path, e))?;
    if !parent.starts_with(&root) {
        return Err(Error::Data(format!(
            "path must remain inside {}: {}",
            root.display(),
            path.display()
        )));
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, bytes).map_err(|e| io_error(path, e))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_runway_designation_is_order_independent() {
        assert_eq!(canonical_designation("29/11"), "11/29");
    }

    #[test]
    fn water_surface_is_rejected_from_release() {
        let runway = Runway {
            id: "W1".into(),
            length_ft: 1_000.0,
            surface: "WATER".into(),
            le_ident: None,
            he_ident: None,
            closed: false,
            kind: RunwayKind::LandRunway,
            provenance: BTreeMap::new(),
            evidence: BTreeMap::new(),
        };
        assert!(invalid_surface(&runway));
    }
}

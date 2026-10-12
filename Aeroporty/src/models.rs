use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AirportStatus {
    Open,
    Close,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceMethod {
    SourceReported,
    Derived,
    IndependentlyVerified,
    Recovered,
    NotComparable,
    #[default]
    Unresolved,
    Conflicting,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_record_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at: Option<String>,
    pub raw_value: String,
    pub normalized_value: String,
    pub method: EvidenceMethod,
    pub validation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
}

impl Evidence {
    pub fn source_reported(
        source: impl Into<String>,
        record_id: impl Into<String>,
        raw: impl Into<String>,
        normalized: impl Into<String>,
        validation: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            source_record_id: Some(record_id.into()),
            retrieved_at: None,
            effective_at: None,
            raw_value: raw.into(),
            normalized_value: normalized.into(),
            method: EvidenceMethod::SourceReported,
            validation: validation.into(),
            conflict: None,
        }
    }

    pub fn derived(
        source: impl Into<String>,
        record_id: impl Into<String>,
        raw: impl Into<String>,
        normalized: impl Into<String>,
        rule: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            source_record_id: Some(record_id.into()),
            retrieved_at: None,
            effective_at: None,
            raw_value: raw.into(),
            normalized_value: normalized.into(),
            method: EvidenceMethod::Derived,
            validation: rule.into(),
            conflict: None,
        }
    }

    pub fn unresolved(
        source: impl Into<String>,
        record_id: impl Into<String>,
        raw: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            source_record_id: Some(record_id.into()),
            retrieved_at: None,
            effective_at: None,
            raw_value: raw.into(),
            normalized_value: String::new(),
            method: EvidenceMethod::Unresolved,
            validation: reason.into(),
            conflict: None,
        }
    }
}

pub type EvidenceMap = BTreeMap<String, Vec<Evidence>>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunwayKind {
    LandRunway,
    Waterway,
    Helipad,
    #[default]
    Unknown,
    Invalid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Warning,
    Error,
    InsufficientData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeographicIssue {
    pub icao: String,
    pub runway_id: Option<String>,
    pub severity: IssueSeverity,
    pub check: String,
    pub message: String,
    #[serde(default)]
    pub measured_value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineCategory {
    MalformedCsv,
    InvalidReference,
    Waterway,
    Helipad,
    UnknownType,
    InvalidRecord,
    IncompleteLandRunway,
    Duplicate,
    GeographicConflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuarantineRecord {
    pub source: String,
    pub row_number: Option<u64>,
    pub airport_id: Option<u64>,
    pub icao: Option<String>,
    pub record_id: Option<String>,
    pub category: QuarantineCategory,
    pub reason: String,
    pub affects_airport: bool,
    #[serde(default)]
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Runway {
    pub id: String,
    pub length_ft: f64,
    pub surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub le_ident: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub he_ident: Option<String>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default = "default_land_runway")]
    pub kind: RunwayKind,
    #[serde(default)]
    pub provenance: BTreeMap<String, String>,
    #[serde(default)]
    pub evidence: EvidenceMap,
}

fn default_land_runway() -> RunwayKind {
    RunwayKind::LandRunway
}

#[derive(Debug, Clone, Default)]
pub struct RunwayDraft {
    pub id: String,
    pub length_ft: Option<f64>,
    pub width_ft: Option<f64>,
    pub surface: Option<String>,
    pub le_ident: Option<String>,
    pub he_ident: Option<String>,
    pub le_latitude: Option<f64>,
    pub le_longitude: Option<f64>,
    pub he_latitude: Option<f64>,
    pub he_longitude: Option<f64>,
    pub closed: bool,
    pub kind: RunwayKind,
    pub classification_reason: String,
    pub provenance: BTreeMap<String, String>,
    pub evidence: EvidenceMap,
}

#[derive(Debug, Clone, Default)]
pub struct TimezoneResolution {
    pub candidates: Vec<String>,
    pub dataset_version: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Default)]
pub struct AirportDraft {
    pub icao: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub status: Option<AirportStatus>,
    pub airport_type: String,
    pub country: Option<String>,
    pub elevation_ft: Option<i32>,
    pub timezone: Option<String>,
    pub timezone_resolution: TimezoneResolution,
    pub id: Option<u64>,
    pub name: String,
    pub runways: Vec<RunwayDraft>,
    pub quarantine: Vec<QuarantineRecord>,
    pub geographic_issues: Vec<GeographicIssue>,
    pub provenance: BTreeMap<String, String>,
    pub evidence: EvidenceMap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Airport {
    pub icao: String,
    pub latitude: f64,
    pub longitude: f64,
    pub status: AirportStatus,
    #[serde(rename = "type")]
    pub airport_type: String,
    pub country: String,
    pub elevation_ft: i32,
    pub timezone: String,
    pub runways_count: usize,
    pub id: u64,
    pub name: String,
    pub runways: Vec<Runway>,
    #[serde(default)]
    pub provenance: BTreeMap<String, String>,
    #[serde(default)]
    pub evidence: EvidenceMap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dataset {
    pub airports: Vec<Airport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedAirport {
    pub id: Option<u64>,
    pub icao: Option<String>,
    pub name: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AttributeStatistics {
    pub filled: usize,
    pub structurally_valid: usize,
    pub source_reported: usize,
    pub derived: usize,
    pub independently_verified: usize,
    pub conflicting: usize,
    pub unresolved: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunwayStatistics {
    pub source_rows: usize,
    pub not_classified_outside_icao_scope: usize,
    pub land_runways: usize,
    pub waterways: usize,
    pub helipads: usize,
    pub unknown: usize,
    pub invalid_or_duplicate: usize,
    pub quarantined: usize,
    pub included_in_dataset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessingStats {
    pub source_airports: usize,
    pub airports_with_icao: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub recovered_records: usize,
    pub recovered_attributes: usize,
    pub conflicts: usize,
    pub physical_runways: usize,
    #[serde(default)]
    pub uncertain_status_records: usize,
    #[serde(default)]
    pub geographic_warnings: usize,
    #[serde(default)]
    pub geographic_errors: usize,
    #[serde(default)]
    pub malformed_source_rows: usize,
    #[serde(default)]
    pub attributes: AttributeStatistics,
    #[serde(default)]
    pub runway_rows: RunwayStatistics,
    pub by_country: BTreeMap<String, usize>,
    pub by_type: BTreeMap<String, usize>,
    pub rejection_reasons: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
    pub url: String,
    pub local_file: String,
    pub retrieved_at: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub icao: String,
    pub field: String,
    pub primary_value: String,
    pub secondary_value: String,
    pub secondary_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub generated_at: String,
    pub sources: Vec<SourceInfo>,
    pub statistics: ProcessingStats,
    pub conflicts: Vec<Conflict>,
    pub limitations: Vec<String>,
    pub timezone_dataset_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faa: Option<FaaStatistics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase3: Option<Phase3Statistics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anac: Option<AnacStatistics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase4: Option<Phase4Statistics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase5: Option<Phase5Statistics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnacStatistics {
    pub release: String,
    pub public_airports: usize,
    pub private_airports: usize,
    pub siros_airports: usize,
    pub airports_with_icao: usize,
    pub matched_airports: usize,
    pub ambiguous_matches: usize,
    pub matched_runways: usize,
    pub renamed_runway_matches: usize,
    pub independently_verified_values: usize,
    pub recovered_values: usize,
    pub conflicts: usize,
    pub unresolved_conflicts: usize,
    pub unmatched_airports: usize,
    pub new_airport_candidates: usize,
    pub new_airports_added: usize,
    pub restored_airports: usize,
    pub added_runways: usize,
    pub incomplete_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase4Statistics {
    pub phase3_accepted: usize,
    pub accepted: usize,
    pub added_new_airports: usize,
    pub restored_phase3_exclusions: usize,
    pub new_exclusions: usize,
    pub runway_delta: isize,
    pub recovered_values_in_strict_dataset: usize,
    pub independently_verified_values: usize,
    pub unresolved_conflicts: usize,
    pub brazil_airports: usize,
    pub brazil_runways: usize,
    pub remaining_brazil_rejections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase5Statistics {
    pub phase4_accepted: usize,
    pub accepted: usize,
    pub restored_conflict_airports: usize,
    pub conflicts_resolved: usize,
    pub unresolved_conflicts: usize,
    pub runway_delta: isize,
    pub public_records_assessed: usize,
    pub incomplete_records_assessed: usize,
    pub anac_new_icao_audited: usize,
    pub historical_observations: usize,
    pub version_changes: usize,
    pub new_national_sources_integrated: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FaaStatistics {
    pub release: String,
    pub airports: usize,
    pub airports_with_icao: usize,
    pub matched_airports: usize,
    pub ambiguous_matches: usize,
    pub matched_runways: usize,
    pub independently_verified_values: usize,
    pub recovered_values: usize,
    pub conflicts: usize,
    pub automatically_resolved_conflicts: usize,
    pub unresolved_conflicts: usize,
    pub recovered_airports: usize,
    pub new_exclusions: usize,
    pub unmatched_faa_airports: usize,
    #[serde(default)]
    pub unmatched_total: usize,
    #[serde(default)]
    pub unmatched_out_of_scope: usize,
    #[serde(default)]
    pub unmatched_existing_out_of_scope: usize,
    #[serde(default)]
    pub new_airport_candidates: usize,
    #[serde(default)]
    pub new_airports_added: usize,
    #[serde(default)]
    pub unmatched_excluded_type: usize,
    #[serde(default)]
    pub unmatched_insufficient: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FaaArtifacts {
    pub matches: Vec<FaaMatchRow>,
    pub recovered: Vec<FaaDecisionRow>,
    pub conflicts: Vec<FaaDecisionRow>,
    pub unmatched: Vec<FaaUnmatchedRow>,
    #[serde(default)]
    pub candidates: Vec<CandidateAssessment>,
    pub statistics: FaaStatistics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaaMatchRow {
    pub icao: String,
    pub site_no: String,
    pub arpt_id: String,
    pub distance_km: f64,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaaDecisionRow {
    pub icao: String,
    pub runway_id: Option<String>,
    pub field: String,
    pub ourairports_value: String,
    pub faa_value: String,
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaaUnmatchedRow {
    pub icao: String,
    pub site_no: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase3Statistics {
    pub phase2_accepted: usize,
    pub accepted: usize,
    pub added_new_airports: usize,
    pub recovered_phase2_exclusions: usize,
    pub new_exclusions: usize,
    pub runway_delta: isize,
    pub phase2_added_runways_audited: usize,
    pub phase2_added_runway_duplicates: usize,
    pub phase2_removed_runways: usize,
    pub recovered_values_in_strict_dataset: usize,
    pub independently_verified_values: usize,
    pub unresolved_conflicts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAssessment {
    pub icao: String,
    pub source_id: String,
    pub country_code: String,
    pub facility_type: String,
    pub category: String,
    pub priority: u8,
    pub reason: String,
    pub added: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunwayAdditionAudit {
    pub icao: String,
    pub runway_id: String,
    pub faa_identifier: String,
    pub source: String,
    pub reason_absent_in_phase1: String,
    pub match_result: String,
    pub evidence_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePolicyRecord {
    pub source: String,
    pub official_url: String,
    pub access: String,
    pub license_or_terms: String,
    pub integrated: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ValidationOutcome {
    pub airports: Vec<Airport>,
    pub rejected: Vec<RejectedAirport>,
    pub quarantine: Vec<QuarantineRecord>,
    pub geographic_issues: Vec<GeographicIssue>,
}

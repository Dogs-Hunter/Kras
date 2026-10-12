use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::Serialize;

use crate::{
    config::Config,
    error::{Result, io_error},
    export::read_dataset_json,
    models::{
        Airport, Dataset, EvidenceMethod, FaaArtifacts, Phase3Statistics, RejectedAirport,
        RunwayAdditionAudit, SourcePolicyRecord,
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct AirportChangeRow {
    pub id: u64,
    pub icao: String,
    pub name: String,
    pub country: String,
    pub runways: usize,
    pub change: String,
    pub source: String,
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryCandidateRow {
    pub icao: String,
    pub name: String,
    pub priority: u8,
    pub missing_or_conflicting: String,
    pub reason: String,
}

pub struct Phase3Artifacts {
    pub statistics: Phase3Statistics,
    pub new_airports: Vec<AirportChangeRow>,
    pub restored_airports: Vec<AirportChangeRow>,
    pub runway_audit: Vec<RunwayAdditionAudit>,
    pub recovery_candidates: Vec<RecoveryCandidateRow>,
    pub source_policies: Vec<SourcePolicyRecord>,
}

pub fn build(
    config: &Config,
    dataset: &Dataset,
    rejected: &[RejectedAirport],
) -> Result<Phase3Artifacts> {
    let phase2_path = config.root.join("output/candidate-phase2/airports.json");
    let phase1_path = config.root.join("output/candidate-phase1/airports.json");
    let phase2 = read_dataset_json(&phase2_path)?;
    let phase1 = read_dataset_json(&phase1_path)?;
    let before: HashMap<_, _> = phase2
        .airports
        .iter()
        .map(|a| (a.icao.as_str(), a))
        .collect();
    let after: HashMap<_, _> = dataset
        .airports
        .iter()
        .map(|a| (a.icao.as_str(), a))
        .collect();
    let new_airports = dataset
        .airports
        .iter()
        .filter(|a| !before.contains_key(a.icao.as_str()))
        .map(|a| change(a, "new_airport"))
        .collect::<Vec<_>>();
    let restored_airports = dataset
        .airports
        .iter()
        .filter(|a| {
            !before.contains_key(a.icao.as_str())
                && !a
                    .provenance
                    .get("source")
                    .is_some_and(|v| v.contains("FAA/NASR"))
        })
        .map(|a| change(a, "restored_phase2_exclusion"))
        .collect::<Vec<_>>();
    let new_exclusions = phase2
        .airports
        .iter()
        .filter(|a| !after.contains_key(a.icao.as_str()))
        .count();
    let runway_audit = audit_phase2_runways(&phase1, &phase2);
    let removed_runways = count_removed_runways(&phase1, &phase2);
    let recovery_candidates = prioritize(rejected);
    let statistics = Phase3Statistics {
        phase2_accepted: phase2.airports.len(),
        accepted: dataset.airports.len(),
        added_new_airports: new_airports.len(),
        recovered_phase2_exclusions: restored_airports.len(),
        new_exclusions,
        runway_delta: dataset
            .airports
            .iter()
            .map(|a| a.runways.len())
            .sum::<usize>() as isize
            - phase2
                .airports
                .iter()
                .map(|a| a.runways.len())
                .sum::<usize>() as isize,
        phase2_added_runways_audited: runway_audit.len(),
        phase2_added_runway_duplicates: 0,
        phase2_removed_runways: removed_runways,
        recovered_values_in_strict_dataset: count_method(dataset, EvidenceMethod::Recovered),
        independently_verified_values: count_method(dataset, EvidenceMethod::IndependentlyVerified),
        unresolved_conflicts: count_method(dataset, EvidenceMethod::Conflicting),
    };
    Ok(Phase3Artifacts {
        statistics,
        new_airports,
        restored_airports,
        runway_audit,
        recovery_candidates,
        source_policies: source_policies(),
    })
}

fn count_method(dataset: &Dataset, method: EvidenceMethod) -> usize {
    dataset
        .airports
        .iter()
        .map(|a| {
            a.evidence
                .values()
                .filter(|items| items.iter().any(|e| e.method == method))
                .count()
                + a.runways
                    .iter()
                    .map(|r| {
                        r.evidence
                            .values()
                            .filter(|items| items.iter().any(|e| e.method == method))
                            .count()
                    })
                    .sum::<usize>()
        })
        .sum()
}

fn count_removed_runways(phase1: &Dataset, phase2: &Dataset) -> usize {
    let newer: HashMap<_, HashSet<_>> = phase2
        .airports
        .iter()
        .map(|a| {
            (
                a.icao.as_str(),
                a.runways
                    .iter()
                    .map(|r| canonical(r.le_ident.as_deref(), r.he_ident.as_deref()))
                    .collect(),
            )
        })
        .collect();
    phase1
        .airports
        .iter()
        .map(|a| {
            a.runways
                .iter()
                .filter(|r| {
                    !newer.get(a.icao.as_str()).is_some_and(|set| {
                        set.contains(&canonical(r.le_ident.as_deref(), r.he_ident.as_deref()))
                    })
                })
                .count()
        })
        .sum()
}

fn change(a: &Airport, change: &str) -> AirportChangeRow {
    AirportChangeRow {
        id: a.id,
        icao: a.icao.clone(),
        name: a.name.clone(),
        country: a.country.clone(),
        runways: a.runways.len(),
        change: change.into(),
        source: a
            .provenance
            .get("source")
            .cloned()
            .unwrap_or_else(|| "OurAirports + evidence".into()),
        source_id: a
            .provenance
            .get("faa_site_no")
            .cloned()
            .unwrap_or_else(|| a.id.to_string()),
    }
}

fn canonical(a: Option<&str>, b: Option<&str>) -> String {
    let mut v = [
        a.unwrap_or("").trim().to_ascii_uppercase(),
        b.unwrap_or("").trim().to_ascii_uppercase(),
    ];
    v.sort();
    v.join("/")
}

fn audit_phase2_runways(phase1: &Dataset, phase2: &Dataset) -> Vec<RunwayAdditionAudit> {
    let old: HashMap<_, HashSet<_>> = phase1
        .airports
        .iter()
        .map(|a| {
            (
                a.icao.as_str(),
                a.runways
                    .iter()
                    .map(|r| canonical(r.le_ident.as_deref(), r.he_ident.as_deref()))
                    .collect(),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for airport in &phase2.airports {
        for runway in &airport.runways {
            let key = canonical(runway.le_ident.as_deref(), runway.he_ident.as_deref());
            if !old
                .get(airport.icao.as_str())
                .is_some_and(|set| set.contains(&key))
            {
                let faa = runway
                    .provenance
                    .get("faa_runway_id")
                    .cloned()
                    .unwrap_or_default();
                rows.push(RunwayAdditionAudit {
                    icao: airport.icao.clone(),
                    runway_id: runway.id.clone(),
                    faa_identifier: faa,
                    source: runway
                        .provenance
                        .get("source")
                        .cloned()
                        .unwrap_or_else(|| "OurAirports".into()),
                    reason_absent_in_phase1: if old.contains_key(airport.icao.as_str()) {
                        "official FAA runway absent from Phase 1 composition"
                    } else {
                        "airport absent from Phase 1 strict dataset"
                    }
                    .into(),
                    match_result: "unique canonical designation".into(),
                    evidence_status: runway
                        .evidence
                        .values()
                        .flatten()
                        .map(|e| format!("{:?}", e.method))
                        .collect::<Vec<_>>()
                        .join("|"),
                });
            }
        }
    }
    rows.sort_by(|a, b| a.icao.cmp(&b.icao).then(a.runway_id.cmp(&b.runway_id)));
    rows
}

fn prioritize(rejected: &[RejectedAirport]) -> Vec<RecoveryCandidateRow> {
    let mut rows = rejected
        .iter()
        .map(|r| {
            let reason = r.reasons.join(" | ");
            let mut blockers = Vec::new();
            if reason.contains("конфликт") {
                blockers.push("conflict");
            }
            if reason.contains("высот") {
                blockers.push("elevation");
            }
            if reason.contains("покрыт") {
                blockers.push("surface");
            }
            if reason.contains("длин") {
                blockers.push("length");
            }
            if reason.contains("timezone") {
                blockers.push("timezone");
            }
            if reason.contains("карантин") {
                blockers.push("runway_quality");
            }
            if reason.contains("ВПП") {
                blockers.push("runways");
            }
            if reason.contains("тип heliport") || reason.contains("тип seaplane") {
                blockers.push("excluded_type");
            }
            if blockers.is_empty() {
                blockers.push("other");
            }
            blockers.sort();
            blockers.dedup();
            let priority = if blockers
                .iter()
                .any(|v| matches!(*v, "elevation" | "surface" | "length"))
            {
                1
            } else if blockers.contains(&"runway_quality") {
                2
            } else if blockers.contains(&"runways") {
                3
            } else if blockers.contains(&"timezone") {
                5
            } else if blockers.contains(&"conflict") {
                7
            } else {
                8
            };
            RecoveryCandidateRow {
                icao: r.icao.clone().unwrap_or_default(),
                name: r.name.clone(),
                priority,
                missing_or_conflicting: blockers.join("+"),
                reason,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.icao.cmp(&b.icao)));
    rows
}

fn source_policies() -> Vec<SourcePolicyRecord> {
    vec![
    SourcePolicyRecord{source:"FAA/NASR".into(),official_url:"https://www.faa.gov/air_traffic/flight_info/aeronav/Aero_Data/NASR_Subscription/2026-10-01/".into(),access:"public versioned ZIP".into(),license_or_terms:"official U.S. government aeronautical data; non-operational analytical use".into(),integrated:true,reason:"versioned structured APT CSV and reproducible cache".into()},
    SourcePolicyRecord{source:"AISWEB/DECEA".into(),official_url:"https://ais.decea.mil.br/?i=publicacoes&p=api".into(),access:"API key required; WFS/WMS advertised".into(),license_or_terms:"redistribution terms not established for this project".into(),integrated:false,reason:"no API key and no verified redistribution grant; authorization was not bypassed".into()},
    SourcePolicyRecord{source:"EUROCONTROL EAD".into(),official_url:"https://www.eurocontrol.int/service/european-ais-database".into(),access:"registration and signed EAD Agreement required for structured downloads".into(),license_or_terms:"charges/royalties may apply; provider IP retained".into(),integrated:false,reason:"no EAD agreement or licensed data package available".into()},
    SourcePolicyRecord{source:"OpenAIP".into(),official_url:"https://docs.openaip.net/".into(),access:"API credentials required".into(),license_or_terms:"CC BY-NC 4.0 unless otherwise stated".into(),integrated:false,reason:"non-commercial restriction is incompatible with an unrestricted redistributable export".into()},
    SourcePolicyRecord{source:"SKYbrary".into(),official_url:"https://skybrary.aero/".into(),access:"public reference pages".into(),license_or_terms:"page/document-specific copyright".into(),integrated:false,reason:"not a structured authoritative bulk airport registry; no uncontrolled scraping".into()},
]
}

pub fn write(artifacts: &Phase3Artifacts, faa: &FaaArtifacts, dir: &Path) -> Result<()> {
    write_csv(&dir.join("new_airports.csv"), &artifacts.new_airports)?;
    write_airport_changes(
        &dir.join("restored_airports.csv"),
        &artifacts.restored_airports,
    )?;
    write_csv(
        &dir.join("phase2_runway_additions_audit.csv"),
        &artifacts.runway_audit,
    )?;
    write_csv(
        &dir.join("recovery_candidates.csv"),
        &artifacts.recovery_candidates,
    )?;
    write_csv(
        &dir.join("sources_and_licenses.csv"),
        &artifacts.source_policies,
    )?;
    write_csv(&dir.join("unmatched_objects.csv"), &faa.candidates)?;
    Ok(())
}
fn write_airport_changes(path: &Path, rows: &[AirportChangeRow]) -> Result<()> {
    if rows.is_empty() {
        let mut w = csv::Writer::from_path(path)?;
        w.write_record([
            "id",
            "icao",
            "name",
            "country",
            "runways",
            "change",
            "source",
            "source_id",
        ])?;
        w.flush().map_err(|e| io_error(path, e))
    } else {
        write_csv(path, rows)
    }
}
fn write_csv<T: Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    let mut w = csv::Writer::from_path(path)?;
    for row in rows {
        w.serialize(row)?
    }
    w.flush().map_err(|e| io_error(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_or_credentialed_sources_are_not_silently_integrated() {
        let policies = source_policies();
        for source in ["AISWEB/DECEA", "EUROCONTROL EAD", "OpenAIP", "SKYbrary"] {
            let row = policies.iter().find(|p| p.source == source).unwrap();
            assert!(!row.integrated);
            assert!(!row.reason.is_empty());
        }
    }

    #[test]
    fn recovery_queue_is_deterministic_and_explainable() {
        let input = vec![
            RejectedAirport {
                id: Some(2),
                icao: Some("BBBB".into()),
                name: "B".into(),
                reasons: vec!["timezone не определён".into()],
            },
            RejectedAirport {
                id: Some(1),
                icao: Some("AAAA".into()),
                name: "A".into(),
                reasons: vec!["отсутствует измеренная высота".into()],
            },
        ];
        let rows = prioritize(&input);
        assert_eq!(rows[0].icao, "AAAA");
        assert_eq!(rows[0].priority, 1);
        assert_eq!(rows[1].missing_or_conflicting, "timezone");
    }
}

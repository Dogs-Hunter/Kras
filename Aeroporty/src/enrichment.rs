use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use serde::Deserialize;

use crate::{
    error::{Result, io_error},
    models::{
        AirportDraft, AirportStatus, Conflict, Evidence, EvidenceMap, EvidenceMethod, RunwayDraft,
    },
};

pub const METERS_TO_FEET: f64 = 3.280_839_895;

#[derive(Debug, Deserialize)]
struct EnrichmentFile {
    #[serde(default)]
    airports: Vec<AirportPatch>,
}

#[derive(Debug, Deserialize)]
struct AirportPatch {
    icao: String,
    source: String,
    #[serde(default)]
    source_record_id: Option<String>,
    #[serde(default)]
    retrieved_at: Option<String>,
    #[serde(default)]
    effective_at: Option<String>,
    #[serde(default)]
    elevation_ft: Option<i32>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    status: Option<AirportStatus>,
    #[serde(default)]
    runways: Vec<RunwayPatch>,
}

#[derive(Debug, Deserialize)]
struct RunwayPatch {
    #[serde(default)]
    id: String,
    #[serde(default)]
    length_ft: Option<f64>,
    #[serde(default)]
    length_m: Option<f64>,
    #[serde(default)]
    surface: Option<String>,
    #[serde(default)]
    le_ident: Option<String>,
    #[serde(default)]
    he_ident: Option<String>,
}

#[derive(Debug, Default)]
pub struct EnrichmentOutcome {
    pub recovered_records: usize,
    pub recovered_attributes: usize,
    pub conflicts: Vec<Conflict>,
}

pub fn meters_to_feet(meters: f64) -> f64 {
    meters * METERS_TO_FEET
}

pub fn enrich_from_local(airports: &mut [AirportDraft], path: &Path) -> Result<EnrichmentOutcome> {
    if !path.exists() {
        return Ok(EnrichmentOutcome::default());
    }
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    let file: EnrichmentFile = serde_json::from_slice(&bytes)?;
    let mut seen = HashSet::new();
    let mut patches = BTreeMap::new();
    for patch in file.airports {
        let icao = patch.icao.trim().to_ascii_uppercase();
        if !seen.insert(icao.clone()) {
            return Err(crate::Error::Data(format!(
                "дублирующийся ICAO {icao} в enrichment"
            )));
        }
        patches.insert(icao, patch);
    }

    let mut outcome = EnrichmentOutcome::default();
    for airport in airports {
        let Some(patch) = patches.get(&airport.icao) else {
            continue;
        };
        let before = outcome.recovered_attributes;
        let record_id = patch.source_record_id.as_deref().unwrap_or(&airport.icao);
        merge_option(
            &airport.icao,
            "elevation_ft",
            &mut airport.elevation_ft,
            patch.elevation_ft,
            patch,
            record_id,
            &mut airport.provenance,
            &mut airport.evidence,
            &mut outcome,
        );
        merge_option(
            &airport.icao,
            "timezone",
            &mut airport.timezone,
            patch.timezone.clone(),
            patch,
            record_id,
            &mut airport.provenance,
            &mut airport.evidence,
            &mut outcome,
        );
        merge_option(
            &airport.icao,
            "status",
            &mut airport.status,
            patch.status,
            patch,
            record_id,
            &mut airport.provenance,
            &mut airport.evidence,
            &mut outcome,
        );
        for runway_patch in &patch.runways {
            if let Some(index) = find_runway_index(&airport.runways, runway_patch)? {
                let runway = &mut airport.runways[index];
                let length = runway_patch
                    .length_ft
                    .or_else(|| runway_patch.length_m.map(meters_to_feet));
                merge_option(
                    &airport.icao,
                    &format!("runway.{}.length_ft", runway.id),
                    &mut runway.length_ft,
                    length,
                    patch,
                    record_id,
                    &mut runway.provenance,
                    &mut runway.evidence,
                    &mut outcome,
                );
                merge_option(
                    &airport.icao,
                    &format!("runway.{}.surface", runway.id),
                    &mut runway.surface,
                    runway_patch.surface.clone(),
                    patch,
                    record_id,
                    &mut runway.provenance,
                    &mut runway.evidence,
                    &mut outcome,
                );
            }
        }
        if outcome.recovered_attributes > before {
            outcome.recovered_records += 1;
        }
    }
    Ok(outcome)
}

fn find_runway_index(runways: &[RunwayDraft], patch: &RunwayPatch) -> Result<Option<usize>> {
    let patch_designation = designation(patch.le_ident.as_deref(), patch.he_ident.as_deref());
    let matches: Vec<_> = runways
        .iter()
        .enumerate()
        .filter(|(_, runway)| {
            (!patch.id.is_empty() && runway.id == patch.id)
                || (!patch.id.is_empty()
                    && designation(runway.le_ident.as_deref(), runway.he_ident.as_deref())
                        .as_deref()
                        == Some(patch.id.as_str()))
                || patch_designation.as_ref().is_some_and(|candidate| {
                    designation(runway.le_ident.as_deref(), runway.he_ident.as_deref()).as_ref()
                        == Some(candidate)
                })
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [index] => Ok(Some(*index)),
        _ => Err(crate::Error::Data(format!(
            "неоднозначное сопоставление ВПП enrichment id={}",
            patch.id
        ))),
    }
}

fn designation(low: Option<&str>, high: Option<&str>) -> Option<String> {
    let (Some(low), Some(high)) = (low, high) else {
        return None;
    };
    if low.trim().is_empty() || high.trim().is_empty() {
        return None;
    }
    let mut ends = [
        low.trim().to_ascii_uppercase(),
        high.trim().to_ascii_uppercase(),
    ];
    ends.sort();
    Some(format!("{}/{}", ends[0], ends[1]))
}

#[allow(clippy::too_many_arguments)]
fn merge_option<T>(
    icao: &str,
    field: &str,
    current: &mut Option<T>,
    incoming: Option<T>,
    patch: &AirportPatch,
    record_id: &str,
    provenance: &mut BTreeMap<String, String>,
    evidence: &mut EvidenceMap,
    outcome: &mut EnrichmentOutcome,
) where
    T: Clone + PartialEq + std::fmt::Debug,
{
    let Some(incoming) = incoming else { return };
    let normalized = format!("{incoming:?}");
    let mut observation = Evidence::source_reported(
        &patch.source,
        record_id,
        &normalized,
        &normalized,
        "локально импортированное свидетельство; не помечается independently_verified автоматически",
    );
    observation.retrieved_at.clone_from(&patch.retrieved_at);
    observation.effective_at.clone_from(&patch.effective_at);
    match current {
        None => {
            *current = Some(incoming);
            provenance.insert(field.to_owned(), patch.source.clone());
            evidence
                .entry(field.to_owned())
                .or_default()
                .push(observation);
            outcome.recovered_attributes += 1;
        }
        Some(existing) if *existing != incoming => {
            let detail = format!("primary={existing:?}; secondary={incoming:?}");
            observation.method = EvidenceMethod::Conflicting;
            observation.conflict = Some(detail.clone());
            evidence
                .entry(field.to_owned())
                .or_default()
                .push(observation);
            outcome.conflicts.push(Conflict {
                icao: icao.to_owned(),
                field: field.to_owned(),
                primary_value: format!("{existing:?}"),
                secondary_value: format!("{incoming:?}"),
                secondary_source: patch.source.clone(),
            });
        }
        _ => evidence
            .entry(field.to_owned())
            .or_default()
            .push(observation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_designations_never_match() {
        let runway = RunwayDraft {
            id: "R1".into(),
            ..Default::default()
        };
        let patch = RunwayPatch {
            id: "missing".into(),
            length_ft: None,
            length_m: None,
            surface: None,
            le_ident: None,
            he_ident: None,
        };
        assert_eq!(find_runway_index(&[runway], &patch).expect("match"), None);
    }
}

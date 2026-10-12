pub mod ourairports;

use crate::models::{AirportDraft, Conflict, Evidence};

/// Универсальный контракт будущих FAA/NASR, eAIP и других адаптеров.
/// Адаптер выдаёт наблюдения и конфликты, но не перезаписывает основное значение сам.
pub trait EvidenceSource {
    fn source_name(&self) -> &str;
    fn observations_for(&self, airport: &AirportDraft) -> crate::Result<Vec<SourceObservation>>;
}

#[derive(Debug, Clone)]
pub struct SourceObservation {
    pub airport_icao: String,
    pub alternate_airport_ids: Vec<String>,
    pub runway_id: Option<String>,
    pub runway_designation: Option<(String, String)>,
    pub attribute: String,
    pub evidence: Evidence,
}

#[derive(Debug, Default)]
pub struct SourceComparison {
    pub accepted_observations: Vec<SourceObservation>,
    pub conflicts: Vec<Conflict>,
}

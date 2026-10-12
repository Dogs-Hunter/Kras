use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum GeographicBinding {
    StoreBound,
    AddressBound,
    Generic,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateObservation {
    pub source: String,
    pub product_id: Option<String>,
    pub authorization_mode: String,
    pub binding: GeographicBinding,
    pub store_id: Option<String>,
    pub store_address: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfirmedObservation {
    pub candidate: CandidateObservation,
    pub geographic_evidence: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Validation {
    CandidateOnly { reason: String },
    Confirmed(ConfirmedObservation),
}

pub trait SourceAdapter {
    fn source_name(&self) -> &str;
    fn candidate(&self, product_id: Option<String>) -> CandidateObservation;
}

pub fn validate_candidate(candidate: CandidateObservation) -> Validation {
    match candidate.binding {
        GeographicBinding::Generic => Validation::CandidateOnly {
            reason: "generic price has no geographic evidence".to_owned(),
        },
        GeographicBinding::AddressBound if candidate.store_address.is_none() => {
            Validation::CandidateOnly {
                reason: "AddressBound requires address evidence".to_owned(),
            }
        }
        GeographicBinding::StoreBound if candidate.store_id.is_none() => {
            Validation::CandidateOnly {
                reason: "StoreBound requires a stable store id".to_owned(),
            }
        }
        _ => Validation::Confirmed(ConfirmedObservation {
            candidate,
            geographic_evidence: "explicit source binding".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(binding: GeographicBinding) -> CandidateObservation {
        CandidateObservation {
            source: "fixture".to_owned(),
            product_id: Some("1".to_owned()),
            authorization_mode: "Anonymous".to_owned(),
            binding,
            store_id: None,
            store_address: None,
        }
    }

    #[test]
    fn generic_candidate_cannot_be_confirmed() {
        assert!(matches!(
            validate_candidate(candidate(GeographicBinding::Generic)),
            Validation::CandidateOnly { .. }
        ));
    }

    #[test]
    fn store_binding_requires_store_id() {
        assert!(matches!(
            validate_candidate(candidate(GeographicBinding::StoreBound)),
            Validation::CandidateOnly { .. }
        ));
    }

    #[test]
    fn address_binding_requires_address() {
        assert!(matches!(
            validate_candidate(candidate(GeographicBinding::AddressBound)),
            Validation::CandidateOnly { .. }
        ));
    }
}

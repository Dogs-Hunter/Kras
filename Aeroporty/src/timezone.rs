use tzf_rs::DefaultFinder;

use crate::models::{AirportDraft, Evidence};

pub struct TimezoneResolver {
    finder: DefaultFinder,
}

impl TimezoneResolver {
    pub fn new() -> Self {
        Self {
            finder: DefaultFinder::new(),
        }
    }

    pub fn data_version(&self) -> &str {
        self.finder.data_version()
    }

    pub fn enrich(&self, airports: &mut [AirportDraft]) {
        for airport in airports {
            let (Some(longitude), Some(latitude)) = (airport.longitude, airport.latitude) else {
                airport.timezone_resolution.dataset_version = self.data_version().to_owned();
                airport.timezone_resolution.outcome = "insufficient_coordinates".into();
                airport
                    .evidence
                    .entry("timezone".into())
                    .or_default()
                    .push(Evidence::unresolved(
                        "tzf-rs",
                        airport.id.map(|v| v.to_string()).unwrap_or_default(),
                        "",
                        "timezone нельзя вычислить без обеих координат",
                    ));
                continue;
            };
            let names = self.finder.get_tz_names(longitude, latitude);
            airport.timezone_resolution.candidates =
                names.iter().map(|value| (*value).to_owned()).collect();
            airport.timezone_resolution.dataset_version = self.data_version().to_owned();
            if names.len() == 1 {
                airport.timezone = Some(names[0].to_owned());
                airport.timezone_resolution.outcome = "unique_polygon_match".into();
                airport.provenance.insert(
                    "timezone".into(),
                    "tzf-rs: точное попадание в полигон".into(),
                );
                airport.evidence.entry("timezone".into()).or_default().push(Evidence::derived(
                    format!("tzf-rs bundled polygons {}", self.data_version()),
                    airport.id.map(|v| v.to_string()).unwrap_or_default(),
                    format!("longitude={longitude};latitude={latitude};candidates={:?}", names),
                    names[0],
                    "ровно один полигон timezone; это географический вывод, а не независимое авиационное подтверждение",
                ));
            } else {
                airport.timezone_resolution.outcome = if names.is_empty() {
                    "no_polygon_match"
                } else {
                    "ambiguous_polygon_match"
                }
                .into();
                airport
                    .evidence
                    .entry("timezone".into())
                    .or_default()
                    .push(Evidence::unresolved(
                        format!("tzf-rs bundled polygons {}", self.data_version()),
                        airport.id.map(|v| v.to_string()).unwrap_or_default(),
                        format!(
                            "longitude={longitude};latitude={latitude};candidates={:?}",
                            names
                        ),
                        if names.is_empty() {
                            "полигон timezone не найден"
                        } else {
                            "координата попала в несколько полигонов; произвольный выбор запрещён"
                        },
                    ));
            }
        }
    }
}

impl Default for TimezoneResolver {
    fn default() -> Self {
        Self::new()
    }
}

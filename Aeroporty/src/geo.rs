use crate::models::{AirportDraft, GeographicIssue, IssueSeverity, RunwayDraft};

const EARTH_RADIUS_M: f64 = 6_371_008.8;
const METERS_TO_FEET: f64 = 3.280_839_895;

pub fn validate_airport_geography(airport: &AirportDraft) -> Vec<GeographicIssue> {
    let mut issues = Vec::new();
    let (Some(airport_lat), Some(airport_lon)) = (airport.latitude, airport.longitude) else {
        return issues;
    };
    for runway in &airport.runways {
        check_endpoint(
            airport,
            runway,
            "low",
            runway.le_latitude,
            runway.le_longitude,
            (airport_lat, airport_lon),
            &mut issues,
        );
        check_endpoint(
            airport,
            runway,
            "high",
            runway.he_latitude,
            runway.he_longitude,
            (airport_lat, airport_lon),
            &mut issues,
        );
        check_runway_length(airport, runway, &mut issues);
    }
    issues
}

fn check_endpoint(
    airport: &AirportDraft,
    runway: &RunwayDraft,
    end: &str,
    latitude: Option<f64>,
    longitude: Option<f64>,
    airport_position: (f64, f64),
    issues: &mut Vec<GeographicIssue>,
) {
    let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
        return;
    };
    if !valid_coordinate(latitude, longitude) {
        issues.push(issue(
            airport,
            runway,
            IssueSeverity::Error,
            "endpoint_coordinate",
            format!("координаты {end}-конца вне допустимого диапазона"),
            None,
            None,
        ));
        return;
    }
    let distance_km =
        haversine_m(airport_position.0, airport_position.1, latitude, longitude) / 1000.0;
    let severity = if distance_km > 50.0 {
        Some(IssueSeverity::Error)
    } else if distance_km > 10.0 {
        Some(IssueSeverity::Warning)
    } else {
        None
    };
    if let Some(severity) = severity {
        issues.push(issue(
            airport,
            runway,
            severity,
            "airport_to_endpoint",
            format!("{end}-конец ВПП находится в {distance_km:.1} км от reference point аэропорта"),
            Some(distance_km),
            Some("km"),
        ));
    }
}

fn check_runway_length(
    airport: &AirportDraft,
    runway: &RunwayDraft,
    issues: &mut Vec<GeographicIssue>,
) {
    let (Some(le_lat), Some(le_lon), Some(he_lat), Some(he_lon)) = (
        runway.le_latitude,
        runway.le_longitude,
        runway.he_latitude,
        runway.he_longitude,
    ) else {
        return;
    };
    if !valid_coordinate(le_lat, le_lon) || !valid_coordinate(he_lat, he_lon) {
        return;
    }
    let geometric_ft = haversine_m(le_lat, le_lon, he_lat, he_lon) * METERS_TO_FEET;
    let Some(reported_ft) = runway
        .length_ft
        .filter(|value| value.is_finite() && *value > 0.0)
    else {
        return;
    };
    let absolute = (geometric_ft - reported_ft).abs();
    let ratio = geometric_ft.max(reported_ft) / geometric_ft.min(reported_ft);
    let severity = if absolute > 16_404.0 && ratio > 5.0 {
        Some(IssueSeverity::Error)
    } else if absolute > 1_000.0 && ratio > 1.5 {
        Some(IssueSeverity::Warning)
    } else {
        None
    };
    if let Some(severity) = severity {
        issues.push(issue(airport, runway, severity, "geometric_length", format!("геометрическая длина {geometric_ft:.0} ft расходится с заявленной {reported_ft:.0} ft"), Some(geometric_ft), Some("ft")));
    }
}

fn issue(
    airport: &AirportDraft,
    runway: &RunwayDraft,
    severity: IssueSeverity,
    check: &str,
    message: String,
    measured_value: Option<f64>,
    unit: Option<&str>,
) -> GeographicIssue {
    GeographicIssue {
        icao: airport.icao.clone(),
        runway_id: Some(runway.id.clone()),
        severity,
        check: check.into(),
        message,
        measured_value,
        unit: unit.map(str::to_owned),
    }
}

pub fn valid_coordinate(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let lat1 = lat1.to_radians();
    let lat2 = lat2.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().atan2((1.0 - a).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RunwayKind;

    #[test]
    fn detects_vaam_regression() {
        let airport = AirportDraft {
            icao: "VAAM".into(),
            latitude: Some(20.814608),
            longitude: Some(77.717765),
            runways: vec![RunwayDraft {
                id: "610195".into(),
                length_ft: Some(6070.0),
                kind: RunwayKind::LandRunway,
                le_latitude: Some(20.81205),
                le_longitude: Some(77.01274),
                he_latitude: Some(20.81632),
                he_longitude: Some(77.72992),
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = validate_airport_geography(&airport);
        assert!(
            issues
                .iter()
                .any(|issue| issue.severity == IssueSeverity::Error)
        );
        assert!(issues.iter().any(|issue| issue.check == "geometric_length"));
    }
}

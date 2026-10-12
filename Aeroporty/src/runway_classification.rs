use crate::models::{RunwayDraft, RunwayKind};

const UNKNOWN_SURFACES: &[&str] = &["", "UNK", "UNKNOWN", "N/A", "NA", "NULL", "?"];

pub fn classify_runway(airport_type: &str, runway: &RunwayDraft) -> (RunwayKind, String) {
    let surface = normalized_surface(runway.surface.as_deref());
    let low = runway.le_ident.as_deref().unwrap_or("");
    let high = runway.he_ident.as_deref().unwrap_or("");

    if airport_type == "seaplane_base" || is_water_surface(&surface) {
        return (
            RunwayKind::Waterway,
            "тип аэродрома или покрытие явно обозначает водную посадочную поверхность".into(),
        );
    }
    if airport_type == "heliport"
        || is_helipad_ident(low)
        || is_helipad_ident(high)
        || is_short_single_end_h_pad(low, high, runway.length_ft)
        || is_short_single_end_h_pad(high, low, runway.length_ft)
    {
        return (
            RunwayKind::Helipad,
            "тип аэродрома или обозначение конца явно соответствует helipad".into(),
        );
    }

    let positive_length = runway.length_ft.is_some_and(|v| v.is_finite() && v > 0.0);
    let positive_width = runway.width_ft.is_some_and(|v| v.is_finite() && v > 0.0);
    let known_surface = !is_unknown_surface(&surface);
    let runway_designation = is_runway_ident(low) || is_runway_ident(high);

    if runway.id.trim().is_empty() {
        return (
            RunwayKind::Invalid,
            "отсутствует идентификатор исходной строки".into(),
        );
    }
    if !positive_length && !known_surface && !runway_designation {
        return (
            RunwayKind::Invalid,
            "нет длины, покрытия и распознаваемого обозначения ВПП".into(),
        );
    }
    if runway_designation || (positive_length && known_surface) {
        return (
            RunwayKind::LandRunway,
            if positive_width {
                "распознаваемое обозначение ВПП или согласованные размеры и невода-поверхность"
            } else {
                "положительная длина и явно невода-поверхность; отсутствие ширины не меняет тип объекта"
            }.into(),
        );
    }
    (
        RunwayKind::Unknown,
        "имеющихся признаков недостаточно для надёжной классификации".into(),
    )
}

pub fn is_unknown_surface(surface: &str) -> bool {
    UNKNOWN_SURFACES.contains(&surface.trim().to_ascii_uppercase().as_str())
}

fn normalized_surface(surface: Option<&str>) -> String {
    surface.unwrap_or("").trim().to_ascii_uppercase()
}

fn is_water_surface(surface: &str) -> bool {
    surface.contains("WATER") || matches!(surface, "WTR" | "SEA" | "LAKE" | "RIVER")
}

fn is_helipad_ident(value: &str) -> bool {
    let value = value.trim().to_ascii_uppercase();
    value == "H"
        || value == "HEL"
        || value == "HELIPAD"
        || value == "HB"
        || value == "HF"
        || value.starts_with("H-")
        || value.strip_prefix('H').is_some_and(|suffix| {
            !suffix.is_empty()
                && (suffix.bytes().all(|byte| byte.is_ascii_digit()) || is_runway_ident(suffix))
        })
}

fn is_short_single_end_h_pad(
    identified_end: &str,
    other_end: &str,
    length_ft: Option<f64>,
) -> bool {
    let identified_end = identified_end.trim().to_ascii_uppercase();
    other_end.trim().is_empty()
        && identified_end.ends_with('H')
        && is_runway_ident(identified_end.trim_end_matches('H'))
        && length_ft.is_some_and(|length| length.is_finite() && length <= 500.0)
}

fn is_runway_ident(value: &str) -> bool {
    let value = value.trim().to_ascii_uppercase();
    if value.is_empty() {
        return false;
    }
    let (digits, suffix) = if value.len() > 2 {
        value.split_at(2)
    } else {
        (value.as_str(), "")
    };
    let Ok(number) = digits.parse::<u8>() else {
        return false;
    };
    (1..=36).contains(&number) && matches!(suffix, "" | "L" | "R" | "C")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(low: &str, high: &str, surface: &str, length: f64) -> RunwayDraft {
        RunwayDraft {
            id: "1".into(),
            le_ident: Some(low.into()),
            he_ident: Some(high.into()),
            surface: Some(surface.into()),
            length_ft: Some(length),
            width_ft: Some(50.0),
            ..Default::default()
        }
    }

    #[test]
    fn classifies_water_helipad_and_short_land_runway() {
        assert_eq!(
            classify_runway("small_airport", &draft("09W", "27W", "WATER", 5000.0)).0,
            RunwayKind::Waterway
        );
        assert_eq!(
            classify_runway("large_airport", &draft("HB", "HF", "CONC", 80.0)).0,
            RunwayKind::Helipad
        );
        assert_eq!(
            classify_runway("small_airport", &draft("09", "27", "GRASS", 450.0)).0,
            RunwayKind::LandRunway
        );
        assert_eq!(
            classify_runway("small_airport", &draft("14H", "", "CONC", 69.0)).0,
            RunwayKind::Helipad
        );
    }
}

use std::path::PathBuf;

use serde::Deserialize;

pub const BASE_URL: &str = "https://davidmegginson.github.io/ourairports-data";

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: &'static str,
    pub url: String,
    pub path: PathBuf,
}

pub fn source_files(cache_dir: &std::path::Path) -> Vec<SourceFile> {
    ["airports.csv", "runways.csv", "countries.csv"]
        .into_iter()
        .map(|name| SourceFile {
            name,
            url: format!("{BASE_URL}/{name}"),
            path: cache_dir.join(name),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct RawAirport {
    pub id: String,
    pub ident: String,
    #[serde(rename = "type")]
    pub airport_type: String,
    pub name: String,
    pub latitude_deg: String,
    pub longitude_deg: String,
    pub elevation_ft: String,
    pub iso_country: String,
    pub icao_code: String,
}

#[derive(Debug, Deserialize)]
pub struct RawRunway {
    pub id: String,
    pub airport_ref: String,
    #[serde(default)]
    pub airport_ident: String,
    pub length_ft: String,
    #[serde(default)]
    pub width_ft: String,
    pub surface: String,
    #[serde(default)]
    pub closed: String,
    #[serde(default)]
    pub le_ident: String,
    #[serde(default)]
    pub le_latitude_deg: String,
    #[serde(default)]
    pub le_longitude_deg: String,
    #[serde(default)]
    pub he_ident: String,
    #[serde(default)]
    pub he_latitude_deg: String,
    #[serde(default)]
    pub he_longitude_deg: String,
}

#[derive(Debug, Deserialize)]
pub struct RawCountry {
    pub code: String,
    pub name: String,
}

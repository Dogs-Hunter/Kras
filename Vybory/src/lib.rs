pub mod api;
pub mod db;
pub mod districts;
pub mod export;
pub mod model;
pub mod pipeline;
pub mod validation;

pub const CAMPAIGN_ID: i64 = 587_813_923;
pub const ELECTION_DATE: &str = "2026-09-20";
pub const ELECTION_NAME: &str = "Выборы депутатов Государственной Думы Федерального Собрания Российской Федерации девятого созыва";
pub const DEFAULT_API_BASE: &str = "http://apps.cikrf.ru/service/ik-inp-service-pbcopy";

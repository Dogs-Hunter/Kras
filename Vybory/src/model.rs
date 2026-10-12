use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefValue {
    pub id: Option<i64>,
    pub external_id: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Election {
    pub id: i64,
    pub vrn: Option<String>,
    pub external_id: String,
    pub name: String,
    pub voting_date: String,
    pub start_voting_day: Option<String>,
    pub finish_voting_day: Option<String>,
    pub is_multi_day_voting: Option<bool>,
    pub election_level: Option<RefValue>,
    pub kind: Option<RefValue>,
    pub system_type: Option<RefValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommissionNode {
    pub id: Option<i64>,
    pub external_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub commission_type: i64,
    pub number: Option<i64>,
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub has_children: bool,
    #[serde(default)]
    pub children: Vec<CommissionNode>,
    pub protocol1_signed: Option<bool>,
    pub protocol2_signed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiReport {
    pub report_type: String,
    pub id: String,
    pub created_at: Option<String>,
    pub body: ReportBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportBody {
    pub elections_id: String,
    pub commission_classifier_id: String,
    pub protocol_id: Option<String>,
    pub protocol_num: i64,
    #[serde(default)]
    pub signed: bool,
    pub sign_date_time: Option<String>,
    #[serde(default)]
    pub repeated: bool,
    #[serde(default)]
    pub records: Vec<ReportRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRecord {
    pub id: Option<String>,
    pub category: Option<String>,
    pub descr_category: Option<String>,
    pub info_num: Option<i64>,
    pub info_print_num: Option<String>,
    pub value: Option<String>,
    pub percent_value: Option<String>,
    pub info_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    Present,
    Missing,
    ParseError,
    Unpublished,
    Untrusted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolValue {
    pub line_number: u16,
    pub value: Option<i64>,
    pub raw_value: Option<String>,
    pub official_text: String,
    pub state: ValueState,
    pub party_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedProtocol {
    pub report_id: String,
    pub official_protocol_id: Option<String>,
    pub commission_id: String,
    pub protocol_num: i64,
    pub signed: bool,
    pub repeated: bool,
    pub sign_datetime: Option<String>,
    pub source_created_at: Option<String>,
    pub lines: BTreeMap<u16, ProtocolValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationOutcome {
    pub strict_valid: bool,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Debug, Clone)]
pub struct PrecinctJob {
    pub election_id: i64,
    pub uik_id: String,
    pub uik_number: Option<i64>,
    pub region_name: Option<String>,
    pub tik_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HierarchyContext {
    pub region_id: Option<String>,
    pub region_name: Option<String>,
    pub federal_district: Option<String>,
    pub iksrf_id: Option<String>,
    pub iksrf_name: Option<String>,
    pub oik_id: Option<String>,
    pub oik_name: Option<String>,
    pub tik_id: Option<String>,
    pub tik_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolSchemaLine {
    pub line_number: u16,
    pub official_text: String,
    pub kind: String,
    pub party_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolSchema {
    pub election_id: i64,
    pub election_external_id: String,
    pub protocol_number: i64,
    pub report_number: i64,
    pub source_url: String,
    pub confirmed_at: String,
    pub lines: Vec<ProtocolSchemaLine>,
}

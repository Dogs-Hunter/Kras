use std::collections::{BTreeMap, btree_map::Entry};

use crate::model::{
    ApiReport, ParsedProtocol, ProtocolValue, ValidationIssue, ValidationOutcome, ValueState,
};
use crate::{CAMPAIGN_ID, db::Database};

pub fn parse_report(report: &ApiReport) -> (ParsedProtocol, Vec<ValidationIssue>) {
    let mut lines = BTreeMap::new();
    let mut issues = Vec::new();
    for record in &report.body.records {
        let Some(raw_number) = record.info_print_num.as_deref() else {
            continue;
        };
        let Ok(line_number) = raw_number.trim().parse::<u16>() else {
            issues.push(issue(
                "error",
                "invalid_line_number",
                format!("Некорректный номер строки: {raw_number}"),
            ));
            continue;
        };
        let raw_value = record.value.clone();
        let (value, state) = match raw_value.as_deref() {
            None => (None, ValueState::Missing),
            Some(value) => match value.trim().parse::<i64>() {
                Ok(parsed) if parsed >= 0 => (Some(parsed), ValueState::Present),
                _ => (None, ValueState::ParseError),
            },
        };
        let official_text = record
            .info_text
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let party_name = (line_number >= 13).then(|| official_text.clone());
        let parsed = ProtocolValue {
            line_number,
            value,
            raw_value,
            official_text,
            state,
            party_name,
        };
        match lines.entry(line_number) {
            Entry::Vacant(entry) => {
                entry.insert(parsed);
            }
            Entry::Occupied(_) => issues.push(issue(
                "error",
                "duplicate_line",
                format!("Строка {line_number} встречается повторно"),
            )),
        }
    }
    (
        ParsedProtocol {
            report_id: report.id.clone(),
            official_protocol_id: report.body.protocol_id.clone(),
            commission_id: report.body.commission_classifier_id.clone(),
            protocol_num: report.body.protocol_num,
            signed: report.body.signed,
            repeated: report.body.repeated,
            sign_datetime: report.body.sign_date_time.clone(),
            source_created_at: report.created_at.clone(),
            lines,
        },
        issues,
    )
}

pub fn validate_protocol(
    protocol: &ParsedProtocol,
    mut issues: Vec<ValidationIssue>,
) -> ValidationOutcome {
    if protocol.protocol_num != 2 {
        issues.push(issue(
            "error",
            "wrong_protocol_type",
            format!("Ожидался протокол № 2, получен № {}", protocol.protocol_num),
        ));
    }
    for line in 1..=22 {
        match protocol.lines.get(&line) {
            None => issues.push(issue(
                "error",
                "missing_line",
                format!("Отсутствует обязательная строка {line}"),
            )),
            Some(value) if value.state != ValueState::Present => issues.push(issue(
                "error",
                "invalid_value",
                format!("Строка {line} не содержит корректного целого значения"),
            )),
            _ => {}
        }
    }
    if !protocol.signed {
        issues.push(issue(
            "warning",
            "unsigned_protocol",
            "Источник помечает протокол как неподписанный; это не трактуется как отмена без отдельного официального статуса".to_owned(),
        ));
    }
    if let Some(values) = required_values(protocol) {
        let expected_2 = values[2] + values[3] + values[4] + values[5] + values[10] - values[11];
        if values[1] != expected_2 {
            issues.push(issue(
                "error",
                "control_relation_1",
                format!(
                    "Нарушено соотношение 2 = 3+4+5+6+11−12: {} != {expected_2}",
                    values[1]
                ),
            ));
        }
        if values[6] + values[7] != values[8] + values[9] {
            issues.push(issue(
                "error",
                "control_relation_2",
                format!(
                    "Нарушено соотношение 7+8 = 9+10: {} != {}",
                    values[6] + values[7],
                    values[8] + values[9]
                ),
            ));
        }
        let party_sum: i64 = values[12..22].iter().sum();
        if values[9] != party_sum {
            issues.push(issue(
                "error",
                "control_relation_3",
                format!(
                    "Нарушено соотношение 10 = сумма строк 13–22: {} != {party_sum}",
                    values[9]
                ),
            ));
        }
    }
    let strict_valid = !issues.iter().any(|issue| issue.severity == "error");
    ValidationOutcome {
        strict_valid,
        issues,
    }
}

fn required_values(protocol: &ParsedProtocol) -> Option<[i64; 22]> {
    let mut values = [0_i64; 22];
    for (index, line) in (1_u16..=22).enumerate() {
        values[index] = protocol.lines.get(&line)?.value?;
    }
    Some(values)
}

fn issue(severity: &str, code: &str, message: String) -> ValidationIssue {
    ValidationIssue {
        severity: severity.to_owned(),
        code: code.to_owned(),
        message,
    }
}

pub fn revalidate_database(db: &mut Database) -> anyhow::Result<(usize, usize)> {
    let protocols = {
        let mut stmt = db.conn.prepare(
            "SELECT pr.id, pv.id, pv.official_report_id, pv.official_protocol_id,
               p.uik_id, pr.protocol_num, pv.signed, pv.repeated,
               pv.sign_datetime, COALESCE(psm.source_created_at,pv.published_at)
             FROM protocols pr JOIN protocol_versions pv ON pv.id=pr.current_version_id
             JOIN precincts p ON p.election_id=pr.election_id AND p.uik_id=pr.uik_id
             LEFT JOIN protocol_source_metadata psm ON psm.version_id=pv.id
             WHERE pr.election_id=?1",
        )?;
        stmt.query_map([CAMPAIGN_ID], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                ParsedProtocol {
                    report_id: row.get(2)?,
                    official_protocol_id: row.get(3)?,
                    commission_id: row.get(4)?,
                    protocol_num: row.get(5)?,
                    signed: row.get(6)?,
                    repeated: row.get(7)?,
                    sign_datetime: row.get(8)?,
                    source_created_at: row.get(9)?,
                    lines: BTreeMap::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut valid = 0;
    let mut invalid = 0;
    for (protocol_id, version_id, mut protocol) in protocols {
        {
            let mut stmt = db.conn.prepare(
                "SELECT line_number, value, raw_value, official_text, parse_status, party_name
                 FROM protocol_values WHERE version_id=?1 ORDER BY line_number",
            )?;
            let rows = stmt.query_map([version_id], |row| {
                let state_text: String = row.get(4)?;
                let state = match state_text.as_str() {
                    "present" => ValueState::Present,
                    "missing" => ValueState::Missing,
                    "parse_error" => ValueState::ParseError,
                    "unpublished" => ValueState::Unpublished,
                    _ => ValueState::Untrusted,
                };
                let line_number: u16 = row.get(0)?;
                Ok(ProtocolValue {
                    line_number,
                    value: row.get(1)?,
                    raw_value: row.get(2)?,
                    official_text: row.get(3)?,
                    state,
                    party_name: row.get(5)?,
                })
            })?;
            for value in rows {
                let value = value?;
                protocol.lines.insert(value.line_number, value);
            }
        }
        let outcome = validate_protocol(&protocol, Vec::new());
        let tx = db.conn.transaction()?;
        tx.execute(
            "DELETE FROM validation_results WHERE version_id=?1",
            [version_id],
        )?;
        for issue in &outcome.issues {
            tx.execute(
                "INSERT OR IGNORE INTO validation_results(version_id,severity,code,message)
                 VALUES(?1,?2,?3,?4)",
                rusqlite::params![version_id, issue.severity, issue.code, issue.message],
            )?;
        }
        tx.execute(
            "UPDATE protocols SET status=?2,is_strict_valid=?3 WHERE id=?1",
            rusqlite::params![
                protocol_id,
                if outcome.strict_valid {
                    "valid"
                } else {
                    "invalid"
                },
                outcome.strict_valid
            ],
        )?;
        tx.commit()?;
        if outcome.strict_valid {
            valid += 1
        } else {
            invalid += 1
        };
    }
    Ok((valid, invalid))
}

#[cfg(test)]
mod tests {
    use super::{parse_report, validate_protocol};
    use crate::model::ApiReport;

    fn fixture() -> ApiReport {
        serde_json::from_str(include_str!("../tests/fixtures/uik_1.json")).unwrap()
    }

    #[test]
    fn parses_all_22_lines_and_preserves_real_zero() {
        let (parsed, issues) = parse_report(&fixture());
        assert!(issues.is_empty());
        assert_eq!(parsed.lines.len(), 22);
        assert_eq!(parsed.lines[&3].value, Some(0));
        assert_eq!(parsed.lines[&3].raw_value.as_deref(), Some("0"));
        assert_eq!(parsed.lines[&13].party_name.as_deref(), Some("Партия 1"));
        assert!(validate_protocol(&parsed, issues).strict_valid);
    }

    #[test]
    fn missing_and_duplicate_lines_are_errors() {
        let mut report = fixture();
        report
            .body
            .records
            .retain(|r| r.info_print_num.as_deref() != Some("4"));
        report.body.records.push(report.body.records[0].clone());
        let (parsed, issues) = parse_report(&report);
        let result = validate_protocol(&parsed, issues);
        assert!(!result.strict_valid);
        assert!(result.issues.iter().any(|i| i.code == "missing_line"));
        assert!(result.issues.iter().any(|i| i.code == "duplicate_line"));
    }

    #[test]
    fn rejects_protocol_one() {
        let mut report = fixture();
        report.body.protocol_num = 1;
        let (parsed, issues) = parse_report(&report);
        let result = validate_protocol(&parsed, issues);
        assert!(
            result
                .issues
                .iter()
                .any(|i| i.code == "wrong_protocol_type")
        );
    }

    #[test]
    fn party_line_is_taken_from_print_number_not_internal_number() {
        let mut report = fixture();
        let party = report
            .body
            .records
            .iter_mut()
            .find(|record| record.info_print_num.as_deref() == Some("13"))
            .unwrap();
        party.info_num = Some(99);
        let (parsed, issues) = parse_report(&report);
        assert!(issues.is_empty());
        assert_eq!(parsed.lines[&13].official_text, "Партия 1");
    }

    #[test]
    fn corrupted_numeric_value_is_not_zero() {
        let mut report = fixture();
        let value = report
            .body
            .records
            .iter_mut()
            .find(|record| record.info_print_num.as_deref() == Some("3"))
            .unwrap();
        value.value = Some("н/д".to_owned());
        let (parsed, issues) = parse_report(&report);
        assert_eq!(parsed.lines[&3].value, None);
        assert_eq!(parsed.lines[&3].state, crate::model::ValueState::ParseError);
        assert!(!validate_protocol(&parsed, issues).strict_valid);
    }

    #[test]
    fn arithmetic_contradiction_is_reported_without_correction() {
        let mut report = fixture();
        let value = report
            .body
            .records
            .iter_mut()
            .find(|record| record.info_print_num.as_deref() == Some("10"))
            .unwrap();
        value.value = Some("84".to_owned());
        let (parsed, issues) = parse_report(&report);
        let outcome = validate_protocol(&parsed, issues);
        assert_eq!(parsed.lines[&10].value, Some(84));
        assert!(
            outcome
                .issues
                .iter()
                .any(|i| i.code == "control_relation_2")
        );
        assert!(
            outcome
                .issues
                .iter()
                .any(|i| i.code == "control_relation_3")
        );
    }

    #[test]
    fn extra_official_line_is_preserved_but_not_required() {
        let mut report = fixture();
        let mut extra = report.body.records[0].clone();
        extra.info_print_num = Some("23".to_owned());
        extra.value = Some("7".to_owned());
        extra.info_text = Some("Дополнительная строка".to_owned());
        report.body.records.push(extra);
        let (parsed, issues) = parse_report(&report);
        assert_eq!(parsed.lines[&23].value, Some(7));
        assert!(validate_protocol(&parsed, issues).strict_valid);
    }

    #[test]
    fn unsigned_flag_is_reported_without_inventing_cancellation() {
        let mut report = fixture();
        report.body.signed = false;
        let (parsed, issues) = parse_report(&report);
        let outcome = validate_protocol(&parsed, issues);
        assert!(outcome.strict_valid);
        assert!(
            outcome
                .issues
                .iter()
                .any(|issue| issue.code == "unsigned_protocol")
        );
    }
}

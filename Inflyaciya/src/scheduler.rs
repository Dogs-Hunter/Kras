use serde::Serialize;
use std::{fs, path::Path, process::Command};

const TASK_NAME: &str = "Inflyaciya Weekly Monitor";

#[derive(Debug, Serialize)]
pub struct ScheduleResult {
    pub task_name: String,
    pub status: String,
    pub definition_path: String,
    pub installed: bool,
}

pub fn export_definition(
    project_directory: &str,
    day: &str,
    time: &str,
) -> Result<ScheduleResult, String> {
    validate_day(day)?;
    validate_time(time)?;
    let project = Path::new(project_directory)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let executable = project
        .join("target")
        .join("release")
        .join("inflyaciya-phase0.exe");
    let definition_path = project.join("config").join("inflyaciya-weekly-task.xml");
    let xml = task_xml(
        executable
            .to_str()
            .ok_or("путь executable не является Unicode")?,
        project.to_str().ok_or("путь проекта не является Unicode")?,
        day,
        time,
    );
    fs::write(&definition_path, xml).map_err(|error| error.to_string())?;
    Ok(ScheduleResult {
        task_name: TASK_NAME.to_owned(),
        status: "exported_not_installed".to_owned(),
        definition_path: definition_path.to_string_lossy().to_string(),
        installed: false,
    })
}

pub fn install(project_directory: &str, day: &str, time: &str) -> Result<ScheduleResult, String> {
    let mut result = export_definition(project_directory, day, time)?;
    let output = Command::new("schtasks.exe")
        .args([
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            &result.definition_path,
            "/F",
        ])
        .output()
        .map_err(|error| format!("не удалось запустить schtasks.exe: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Планировщик заданий отклонил установку: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    result.status = "installed".to_owned();
    result.installed = true;
    Ok(result)
}

pub fn status() -> Result<serde_json::Value, String> {
    let output = Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME, "/FO", "LIST"])
        .output()
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "task_name": TASK_NAME,
        "installed": output.status.success(),
        "details": String::from_utf8_lossy(if output.status.success() { &output.stdout } else { &output.stderr }).trim()
    }))
}

pub fn remove() -> Result<serde_json::Value, String> {
    let output = Command::new("schtasks.exe")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "не удалось удалить задание: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(serde_json::json!({"task_name": TASK_NAME, "removed": true}))
}

fn task_xml(executable: &str, working_directory: &str, day: &str, time: &str) -> String {
    let weekday = match day {
        "monday" => "Monday",
        "tuesday" => "Tuesday",
        "wednesday" => "Wednesday",
        "thursday" => "Thursday",
        "friday" => "Friday",
        "saturday" => "Saturday",
        "sunday" => "Sunday",
        _ => "Monday",
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Еженедельный read-only мониторинг цен ВкусВилла</Description></RegistrationInfo>
  <Triggers><CalendarTrigger><StartBoundary>2026-01-05T{time}:00+03:00</StartBoundary><Enabled>true</Enabled><ScheduleByWeek><DaysOfWeek><{weekday}/></DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek></CalendarTrigger></Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT30M</ExecutionTimeLimit></Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>collect weekly</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>
"#,
        xml_escape(executable),
        xml_escape(working_directory)
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn validate_day(value: &str) -> Result<(), String> {
    if matches!(
        value,
        "monday" | "tuesday" | "wednesday" | "thursday" | "friday" | "saturday" | "sunday"
    ) {
        Ok(())
    } else {
        Err("день недели должен быть monday..sunday".to_owned())
    }
}

fn validate_time(value: &str) -> Result<(), String> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() == 2
        && parts[0].parse::<u8>().is_ok_and(|hour| hour < 24)
        && parts[1].parse::<u8>().is_ok_and(|minute| minute < 60)
    {
        Ok(())
    } else {
        Err("время должно иметь формат HH:MM".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_quotes_paths_and_never_installs_during_generation() {
        let xml = task_xml(
            "C:\\Program Files\\Inflation\\app.exe",
            "C:\\My Project",
            "monday",
            "09:00",
        );
        assert!(xml.contains("<Command>C:\\Program Files\\Inflation\\app.exe</Command>"));
        assert!(xml.contains("<WorkingDirectory>C:\\My Project</WorkingDirectory>"));
        assert!(xml.contains("collect weekly"));
    }

    #[test]
    fn schedule_parameters_are_strictly_validated() {
        assert!(validate_day("monday").is_ok());
        assert!(validate_day("monday & calc").is_err());
        assert!(validate_time("09:00").is_ok());
        assert!(validate_time("25:00").is_err());
    }
}

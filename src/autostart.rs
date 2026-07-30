use std::fs;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const TASK_NAME: &str = "JoyMapperRust";
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn task_xml(exe_path: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>JoyMapper Rust - auto-start on logon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <Delay>PT10S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{}"</Command>
    </Exec>
  </Actions>
</Task>"#,
        exe_path.display()
    )
}

pub fn enable(exe_path: &Path) -> Result<(), String> {
    let xml = task_xml(exe_path);
    let xml_path = std::env::temp_dir().join("joymapper_task.xml");
    fs::write(&xml_path, &xml).map_err(|e| format!("Failed to write temp XML: {e}"))?;

    let output = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            xml_path.to_str().unwrap(),
            "/F",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to run schtasks: {e}"))?;

    let _ = fs::remove_file(&xml_path);

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!("schtasks create error: {stderr} | {stdout}"))
    }
}

pub fn disable() -> Result<(), String> {
    let output = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to run schtasks: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("does not exist") && !stderr.contains("cannot find") {
            Err(format!("schtasks delete error: {stderr}"))
        } else {
            Ok(())
        }
    }
}

pub fn is_enabled() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn query_task_exe() -> Option<String> {
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/FO", "LIST"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Task To Run:") {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

pub fn validate_and_fix(current_exe: &Path) {
    let task_exe_str = match query_task_exe() {
        Some(p) => p,
        None => {
            if let Err(e) = enable(current_exe) {
                eprintln!("[joymapper] Auto-start enable error: {e}");
            }
            return;
        }
    };

    let task_exe = Path::new(task_exe_str.trim_matches('"'));

    if task_exe == current_exe {
        return;
    }

    if let Err(e) = enable(current_exe) {
        eprintln!("[joymapper] Auto-start fix error: {e}");
    }
}

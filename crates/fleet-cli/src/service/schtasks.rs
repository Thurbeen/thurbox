//! A Task Scheduler task that starts at the user's logon, driven through
//! `schtasks`. Fleet keeps the task's XML, which names its store in the
//! task's `Documentation`, under the local application data directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Install, QUICK_BOUND, Refusal, STORE_KEY, State, Tool, Unit, Verb, xml};

pub const KIND: &str = "task-scheduler";
const SCHTASKS: Tool = Tool {
    program: "schtasks",
    lead: &[],
};
const DOCUMENTATION: &str = "<Documentation>";
const EX_CONFIG: &str = "78";

pub fn unit_name(name: &str) -> Result<String, Refusal> {
    super::valid_name(name, ".xml", "")
}

pub fn file_of_unit(unit: &str) -> String {
    format!("{unit}.xml")
}

pub fn unit_of_file(file: &str) -> Option<String> {
    file.strip_suffix(".xml").map(str::to_string)
}

pub fn unit_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|d| !d.is_empty())
                .map(|h| PathBuf::from(h).join("AppData").join("Local"))
        })?;
    Some(local.join("fleet").join("service"))
}

fn utf16(text: &str) -> Vec<u8> {
    std::iter::once('\u{feff}')
        .chain(text.chars())
        .collect::<String>()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn from_utf16(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16(&units).ok()
}

pub fn invocation(_path: &Path) -> Option<super::Invocation> {
    None
}

pub fn unit_store(path: &Path) -> Option<String> {
    let text = from_utf16(&std::fs::read(path).ok()?)?;
    xml::between(
        &text,
        &format!("{DOCUMENTATION}{STORE_KEY}"),
        "</Documentation>",
    )
}

fn user() -> Option<String> {
    let name = std::env::var("USERNAME").ok().filter(|n| !n.is_empty())?;
    Some(
        match std::env::var("USERDOMAIN").ok().filter(|d| !d.is_empty()) {
            Some(domain) => format!("{domain}\\{name}"),
            None => name,
        },
    )
}

pub fn render(install: &Install, _name: &str) -> Result<Vec<u8>, Refusal> {
    let exe = super::printable(install.exe, "fleet")?;
    let config = super::printable(install.config, "config")?;
    let store = super::printable(install.data_dir, "data directory")?;
    let dir = install
        .exe
        .parent()
        .map(|d| super::printable(d, "fleet's directory"))
        .transpose()?
        .unwrap_or_default();
    let user_id = user()
        .map(|u| format!("<UserId>{}</UserId>", xml::escaped(&u)))
        .unwrap_or_default();
    Ok(utf16(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
         <!-- Written by `fleet service install`: it rewrites this file. -->\r\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
         \x20 <RegistrationInfo>\r\n\
         \x20   <Description>Fleet manager (fleet up)</Description>\r\n\
         \x20   {DOCUMENTATION}{STORE_KEY}{store}</Documentation>\r\n\
         \x20 </RegistrationInfo>\r\n\
         \x20 <Triggers>\r\n\
         \x20   <LogonTrigger>\r\n\
         \x20     <Enabled>true</Enabled>\r\n\
         \x20     {user_id}\r\n\
         \x20   </LogonTrigger>\r\n\
         \x20 </Triggers>\r\n\
         \x20 <Principals>\r\n\
         \x20   <Principal id=\"Author\">\r\n\
         \x20     {user_id}\r\n\
         \x20     <LogonType>InteractiveToken</LogonType>\r\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\r\n\
         \x20   </Principal>\r\n\
         \x20 </Principals>\r\n\
         \x20 <Settings>\r\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\r\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
         \x20   <AllowHardTerminate>true</AllowHardTerminate>\r\n\
         \x20   <StartWhenAvailable>true</StartWhenAvailable>\r\n\
         \x20   <AllowStartOnDemand>true</AllowStartOnDemand>\r\n\
         \x20   <Enabled>true</Enabled>\r\n\
         \x20   <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\r\n\
         \x20   <RestartOnFailure>\r\n\
         \x20     <Interval>PT1M</Interval>\r\n\
         \x20     <Count>999</Count>\r\n\
         \x20   </RestartOnFailure>\r\n\
         \x20 </Settings>\r\n\
         \x20 <Actions Context=\"Author\">\r\n\
         \x20   <Exec>\r\n\
         \x20     <Command>{exe}</Command>\r\n\
         \x20     <Arguments>{arguments}</Arguments>\r\n\
         \x20     <WorkingDirectory>{dir}</WorkingDirectory>\r\n\
         \x20   </Exec>\r\n\
         \x20 </Actions>\r\n\
         </Task>\r\n",
        store = xml::escaped(&store),
        exe = xml::escaped(&exe),
        arguments = xml::escaped(&format!("--config \"{config}\" up")),
        dir = xml::escaped(&dir),
    )))
}

pub fn register(unit: &Unit, now: bool) -> Result<(), Refusal> {
    let xml = unit.path.to_string_lossy();
    SCHTASKS.text(
        &["/Create", "/TN", &unit.name, "/XML", &xml, "/F"],
        QUICK_BOUND,
    )?;
    if now {
        run(unit)?;
    }
    Ok(())
}

pub fn deregister(unit: &Unit, stop_bound: Duration) -> Result<(), Refusal> {
    drive(unit, Verb::Stop, stop_bound)?;
    SCHTASKS
        .text(&["/Delete", "/TN", &unit.name, "/F"], QUICK_BOUND)
        .map(drop)
}

pub fn forget(_unit: &Unit) -> Result<(), Refusal> {
    Ok(())
}

fn query(unit: &Unit) -> Result<String, Refusal> {
    SCHTASKS.text(
        &["/Query", "/TN", &unit.name, "/V", "/FO", "LIST"],
        QUICK_BOUND,
    )
}

fn field<'a>(report: &'a str, key: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

fn state_of(report: &str) -> State {
    let running = field(report, "Status") == Some("Running");
    let last = field(report, "Last Result").unwrap_or_default();
    State {
        running,
        failed: (!running && last == EX_CONFIG).then(|| format!("last result {last}")),
    }
}

pub fn state(unit: &Unit) -> Result<State, Refusal> {
    query(unit).map(|report| state_of(&report))
}

fn run(unit: &Unit) -> Result<(), Refusal> {
    SCHTASKS
        .text(&["/Run", "/TN", &unit.name], QUICK_BOUND)
        .map(drop)
}

fn end(unit: &Unit) -> Result<(), Refusal> {
    SCHTASKS
        .text(&["/End", "/TN", &unit.name], QUICK_BOUND)
        .map(drop)
}

pub fn try_restart(unit: &Unit) -> Result<(), Refusal> {
    end(unit)?;
    run(unit)
}

pub fn drive(unit: &Unit, verb: Verb, _stop_bound: Duration) -> Result<(), Refusal> {
    match verb {
        Verb::Start => run(unit),
        Verb::Stop if state(unit)?.running => end(unit),
        Verb::Stop => Ok(()),
        Verb::Restart if state(unit)?.running => try_restart(unit),
        Verb::Restart => run(unit),
    }
}

/// `schtasks /Query /V`: its report, and 0 while the task runs, 3 while it
/// does not.
pub fn status(unit: &Unit) -> Result<(String, i32), Refusal> {
    let (report, code) = SCHTASKS.report(&["/Query", "/TN", &unit.name, "/V", "/FO", "LIST"])?;
    let code = match code {
        0 if state_of(&report).running => 0,
        0 => 3,
        other => other,
    };
    Ok((report, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_whether_the_task_runs_or_refused() {
        let report = |status: &str, last: &str| {
            state_of(&format!(
                "TaskName:      \\fleet\r\nStatus:        {status}\r\nLast Result:   {last}\r\n"
            ))
        };
        assert!(report("Running", "267009").running);
        assert_eq!(
            report("Ready", "78").failed.as_deref(),
            Some("last result 78")
        );
        assert!(report("Ready", "1").failed.is_none());
    }

    #[test]
    fn the_task_file_is_utf16_and_names_its_store() {
        let bytes = utf16("<Documentation>X-Fleet-Store=C:\\a &amp; b</Documentation>");
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        let dir = std::env::temp_dir().join(format!("fleet-task-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.xml");
        std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(unit_store(&path).as_deref(), Some("C:\\a & b"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

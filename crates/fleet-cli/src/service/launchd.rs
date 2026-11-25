use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Install, QUICK_BOUND, Refusal, State, Tool, Unit, Verb, xml};

pub const KIND: &str = "launchd";
const LAUNCHCTL: Tool = Tool {
    program: "launchctl",
    lead: &[],
};
// launchd ignores a property list key it does not know.
const STORE: &str = "<key>XFleetStore</key>\n  <string>";
const EX_CONFIG: &str = "78";

pub fn unit_name(name: &str) -> Result<String, Refusal> {
    super::valid_name(name, ".plist", "")
}

pub fn file_of_unit(unit: &str) -> String {
    format!("{unit}.plist")
}

pub fn unit_of_file(file: &str) -> Option<String> {
    file.strip_suffix(".plist").map(str::to_string)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

pub fn unit_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Library/LaunchAgents"))
}

fn log_path(label: &str) -> Option<PathBuf> {
    home().map(|h| h.join("Library/Logs").join(format!("{label}.log")))
}

pub fn invocation(_path: &Path) -> Option<super::Invocation> {
    None
}

pub fn unit_store(path: &Path) -> Option<String> {
    xml::between(&std::fs::read_to_string(path).ok()?, STORE, "</string>")
}

fn string(text: &str) -> String {
    format!("<string>{}</string>", xml::escaped(text))
}

pub fn render(install: &Install, label: &str) -> Result<Vec<u8>, Refusal> {
    let exe = super::printable(install.exe, "fleet")?;
    let config = super::printable(install.config, "config")?;
    let store = super::printable(install.data_dir, "data directory")?;
    let path_env = super::search_path(install, ":")?;
    let log = log_path(label)
        .map(|p| super::printable(&p, "log"))
        .transpose()?
        .map(|log| {
            format!(
                "  <key>StandardOutPath</key>\n  {log}\n  <key>StandardErrorPath</key>\n  {log}\n",
                log = string(&log)
            )
        })
        .unwrap_or_default();
    let stop = install.stop_timeout.as_millis().div_ceil(1000).max(1);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by `fleet service install`: it rewrites this file. -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n\
         \x20 {label}\n\
         \x20 {STORE}{store}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   {exe}\n\
         \x20   <string>--config</string>\n\
         \x20   {config}\n\
         \x20   <string>up</string>\n\
         \x20 </array>\n\
         \x20 <key>EnvironmentVariables</key>\n\
         \x20 <dict>\n\
         \x20   <key>FLEET_CONFIG</key>\n\
         \x20   {config}\n\
         \x20   <key>PATH</key>\n\
         \x20   {path_env}\n\
         \x20 </dict>\n\
         \x20 <key>RunAtLoad</key>\n\
         \x20 <true/>\n\
         \x20 <key>KeepAlive</key>\n\
         \x20 <dict>\n\
         \x20   <key>SuccessfulExit</key>\n\
         \x20   <false/>\n\
         \x20 </dict>\n\
         \x20 <key>ThrottleInterval</key>\n\
         \x20 <integer>10</integer>\n\
         \x20 <key>ExitTimeOut</key>\n\
         \x20 <integer>{stop}</integer>\n\
         \x20 <key>AbandonProcessGroup</key>\n\
         \x20 <true/>\n\
         \x20 <key>ProcessType</key>\n\
         \x20 <string>Background</string>\n\
         {log}\
         </dict>\n\
         </plist>\n",
        label = string(label),
        store = xml::escaped(&store),
        exe = string(&exe),
        config = string(&config),
        path_env = string(&path_env),
    )
    .into_bytes())
}

fn domain() -> String {
    format!("gui/{}", rustix::process::getuid().as_raw())
}

fn service_target(unit: &Unit) -> String {
    format!("{}/{}", domain(), unit.name)
}

fn loaded(unit: &Unit) -> Result<Option<String>, Refusal> {
    match LAUNCHCTL.report(&["print", &service_target(unit)])? {
        (text, 0) => Ok(Some(text)),
        _ => Ok(None),
    }
}

fn bootstrap(unit: &Unit) -> Result<(), Refusal> {
    if let Some(dir) = log_path(&unit.name).as_deref().and_then(Path::parent) {
        std::fs::create_dir_all(dir).map_err(|e| Refusal::Io(format!("{}: {e}", dir.display())))?;
    }
    let path = unit.path.to_string_lossy();
    LAUNCHCTL
        .text(&["bootstrap", &domain(), &path], QUICK_BOUND)
        .map(drop)
}

pub fn register(unit: &Unit, now: bool) -> Result<(), Refusal> {
    LAUNCHCTL.text(&["enable", &service_target(unit)], QUICK_BOUND)?;
    if now && loaded(unit)?.is_none() {
        bootstrap(unit)?;
    }
    Ok(())
}

pub fn deregister(unit: &Unit, stop_bound: Duration) -> Result<(), Refusal> {
    drive(unit, Verb::Stop, stop_bound)
}

pub fn forget(_unit: &Unit) -> Result<(), Refusal> {
    Ok(())
}

fn field<'a>(report: &'a str, key: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(" = "))
}

fn state_of(report: &str) -> State {
    let running = field(report, "state") == Some("running");
    let exit = field(report, "last exit code")
        .and_then(|c| c.split(':').next())
        .unwrap_or_default();
    State {
        running,
        failed: (!running && exit == EX_CONFIG).then(|| format!("last exit code {exit}")),
    }
}

pub fn state(unit: &Unit) -> Result<State, Refusal> {
    Ok(loaded(unit)?.map_or(
        State {
            running: false,
            failed: None,
        },
        |report| state_of(&report),
    ))
}

pub fn try_restart(unit: &Unit) -> Result<(), Refusal> {
    LAUNCHCTL
        .text(&["kickstart", "-k", &service_target(unit)], QUICK_BOUND)
        .map(drop)
}

pub fn drive(unit: &Unit, verb: Verb, stop_bound: Duration) -> Result<(), Refusal> {
    let target = service_target(unit);
    let loaded = loaded(unit)?.is_some();
    match (verb, loaded) {
        (Verb::Start | Verb::Restart, false) => bootstrap(unit),
        (Verb::Start, true) => LAUNCHCTL
            .text(&["kickstart", &target], QUICK_BOUND)
            .map(drop),
        (Verb::Restart, true) => try_restart(unit),
        (Verb::Stop, true) => LAUNCHCTL.text(&["bootout", &target], stop_bound).map(drop),
        (Verb::Stop, false) => Ok(()),
    }
}

pub fn status(unit: &Unit) -> Result<(String, i32), Refusal> {
    Ok(match loaded(unit)? {
        Some(report) => {
            let code = if state_of(&report).running { 0 } else { 3 };
            (report, code)
        }
        None => (format!("{} is not loaded\n", unit.name), 3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_whether_the_agent_runs_or_refused() {
        let running = state_of("gui/1/fleet = {\n\tstate = running\n\tpid = 4\n}\n");
        assert!(running.running && running.failed.is_none());
        let refused = state_of("x = {\n\tstate = not running\n\tlast exit code = 78: EX_CONFIG\n}");
        assert!(!refused.running);
        assert_eq!(refused.failed.as_deref(), Some("last exit code 78"));
        let restarting = state_of("x = {\n\tstate = spawn scheduled\n\tlast exit code = 1\n}");
        assert!(!restarting.running && restarting.failed.is_none());
    }
}

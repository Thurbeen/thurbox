//! The Windows release executables are Authenticode-signed through SignPath
//! before they are packaged, and the release is checked to carry the signature.
//!
//! Unsigned binaries start every release at zero reputation, which is what let
//! Defender ASR block a provisioned `thurbox-cli.exe` in a loop (#1576). The
//! signing lives in `cd.yml`, which no other test reads, and its value depends
//! on order: a signature applied after `Compress-Archive`, or a checksum taken
//! before it, ships unsigned bytes while every job is green. So this pins the
//! order, the gate that keeps a fork or a not-yet-approved project releasing
//! unsigned, and the post-publish check — see `docs/CODE-SIGNING.md`.

use std::path::Path;

use regex::Regex;

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|e| format!("<unreadable {path}: {e}>"))
}

/// The lines of job `name` under `jobs:`, up to the next job.
fn job(workflow: &str, name: &str) -> String {
    let header = format!("  {name}:");
    let mut lines = workflow.lines().skip_while(|l| *l != header);
    let mut body = String::new();
    if lines.next().is_none() {
        return body;
    }
    for line in lines {
        let is_next_job = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim_start().starts_with('#');
        if is_next_job {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// A job's steps as `(name, text)`, in order.
fn steps(job: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in job.lines() {
        if let Some(name) = line.strip_prefix("      - name: ") {
            out.push((name.trim().to_string(), String::new()));
        }
        if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

fn step<'a>(steps: &'a [(String, String)], name: &str) -> &'a str {
    steps
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| panic!("no step {name:?} in {:?}", names(steps)))
}

fn names(steps: &[(String, String)]) -> Vec<&str> {
    steps.iter().map(|(n, _)| n.as_str()).collect()
}

const GATE: &str = "Read the SignPath configuration";
const UPLOAD: &str = "Upload the unsigned Windows executables";
const SIGN: &str = "Sign the Windows executables with SignPath";
const STAGE: &str = "Verify and stage the signed executables";
const ARCHIVE: &str = "Create archive (Windows)";
const SIGNED: &str = "steps.signpath.outputs.enabled == 'true'";

#[test]
fn windows_executables_are_signed_before_they_are_packaged() {
    let build = steps(&job(&read(".github/workflows/cd.yml"), "build"));
    let order: Vec<&str> = names(&build)
        .into_iter()
        .filter(|n| [GATE, UPLOAD, SIGN, STAGE, ARCHIVE].contains(n))
        .collect();
    assert_eq!(order, [GATE, UPLOAD, SIGN, STAGE, ARCHIVE], "{build:#?}");

    let gate = step(&build, GATE);
    assert!(gate.contains("id: signpath"), "{gate}");
    assert!(gate.contains("runner.os == 'Windows'"), "{gate}");
    assert!(gate.contains("scripts/ci/signpath-config.sh"), "{gate}");
    for name in [UPLOAD, SIGN, STAGE] {
        let text = step(&build, name);
        assert!(text.contains(SIGNED), "{name} is not gated:\n{text}");
    }

    let upload = step(&build, UPLOAD);
    assert!(upload.contains("id: unsigned"), "{upload}");
    assert!(upload.contains("thurbox.exe") && upload.contains("thurbox-cli.exe"));

    let sign = step(&build, SIGN);
    let pinned =
        Regex::new(r"uses: signpath/github-action-submit-signing-request@[0-9a-f]{40} # v\d")
            .unwrap();
    assert!(pinned.is_match(sign), "not pinned by commit SHA:\n{sign}");
    for input in [
        "api-token: ${{ secrets.SIGNPATH_API_TOKEN }}",
        "organization-id: ${{ vars.SIGNPATH_ORGANIZATION_ID }}",
        "project-slug: ${{ vars.SIGNPATH_PROJECT_SLUG }}",
        "signing-policy-slug: ${{ vars.SIGNPATH_SIGNING_POLICY_SLUG }}",
        "github-artifact-id: ${{ steps.unsigned.outputs.artifact-id }}",
        "wait-for-completion: true",
    ] {
        assert!(sign.contains(input), "missing {input:?}:\n{sign}");
    }

    let stage = step(&build, STAGE);
    assert!(stage.contains("Get-AuthenticodeSignature"), "{stage}");

    // The archive packs what the release directory holds, which the staging
    // step overwrote with the signed files.
    let archive = step(&build, ARCHIVE);
    assert!(archive.contains("$rel/thurbox.exe"), "{archive}");
}

#[test]
fn the_published_windows_release_is_verified() {
    let job = job(&read(".github/workflows/cd.yml"), "verify-windows-release");
    assert!(job.contains("runs-on: windows-latest"), "{job}");
    assert!(job.contains("publish"), "must run after publish:\n{job}");
    let steps = steps(&job);
    let check = step(&steps, "Verify the published Windows archive");
    assert!(check.contains("./scripts/install.ps1"), "{check}");
    assert!(check.contains("Get-AuthenticodeSignature"), "{check}");
    let gate = step(&steps, GATE);
    assert!(gate.contains("scripts/ci/signpath-config.sh"), "{gate}");
}

#[test]
fn the_code_signing_policy_is_published_and_linked() {
    // Markdown wraps the attribution wherever the line ends.
    let policy = read("docs/CODE-SIGNING.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(policy.contains("# Code signing policy"), "{policy}");
    assert!(policy.contains(
        "Free code signing provided by [SignPath.io](https://signpath.io), \
         certificate by [SignPath Foundation](https://signpath.org)"
    ));
    let readme = read("README.md");
    assert!(
        readme.contains("[Code signing policy](docs/CODE-SIGNING.md)"),
        "README does not link the policy"
    );
}

/// Runs the gate with exactly `vars` set: (success, `GITHUB_OUTPUT`, log).
#[cfg(unix)]
fn gate(vars: &[(&str, &str)]) -> (bool, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("output");
    std::fs::write(&output, "").unwrap();
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci/signpath-config.sh"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GITHUB_OUTPUT", &output);
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let run = cmd.output().unwrap();
    let log =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    (
        run.status.success(),
        std::fs::read_to_string(&output).unwrap(),
        log,
    )
}

#[cfg(unix)]
const ALL: [(&str, &str); 4] = [
    ("SIGNPATH_API_TOKEN", "token"),
    ("SIGNPATH_ORGANIZATION_ID", "org"),
    ("SIGNPATH_PROJECT_SLUG", "thurbox"),
    ("SIGNPATH_SIGNING_POLICY_SLUG", "release-signing"),
];

#[cfg(unix)]
#[test]
fn signing_is_skipped_until_signpath_is_configured() {
    let (ok, out, log) = gate(&[]);
    assert!(ok, "{log}");
    assert_eq!(out.trim(), "enabled=false", "{log}");
}

#[cfg(unix)]
#[test]
fn signing_runs_once_every_setting_is_present() {
    let (ok, out, log) = gate(&ALL);
    assert!(ok, "{log}");
    assert_eq!(out.trim(), "enabled=true", "{log}");
    assert!(!log.contains("token"), "the gate echoed the token:\n{log}");
}

/// Half a configuration is a mistake, not an opt-out: releasing unsigned then
/// would hide it until a user's Defender noticed.
#[cfg(unix)]
#[test]
fn a_partial_signpath_configuration_fails_and_names_what_is_missing() {
    let (ok, out, log) = gate(&ALL[..2]);
    assert!(!ok, "{log}");
    assert!(!out.contains("enabled=true"), "{out}");
    assert!(log.contains("SIGNPATH_PROJECT_SLUG"), "{log}");
    assert!(log.contains("SIGNPATH_SIGNING_POLICY_SLUG"), "{log}");
}

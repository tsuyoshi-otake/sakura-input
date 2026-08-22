use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::hash::toml_quoted;
use crate::types::{err, Error, REQUIRED_MODEL, REQUIRED_REASONING};

#[derive(Debug)]
pub struct CodexExec {
    pub argv: Vec<OsString>,
}

#[derive(Debug)]
pub struct ExecOutcome {
    pub result_json: String,
    pub elapsed: Duration,
}

pub fn plan_exec(
    isolation_dir: &Path,
    developer_instructions: &str,
    model: &str,
    reasoning: &str,
) -> Result<CodexExec, Error> {
    if model != REQUIRED_MODEL {
        return Err(err(format!(
            "Judge environment invalid: required model {REQUIRED_MODEL}, actual {model}"
        )));
    }
    crate::identity::refuse_effort_downgrade(REQUIRED_REASONING, reasoning)?;
    let mut argv = Vec::new();
    argv.push(OsString::from("codex"));
    argv.push(OsString::from("exec"));
    argv.push(OsString::from("--skip-git-repo-check"));
    argv.push(OsString::from("--ignore-user-config"));
    argv.push(OsString::from("--ignore-rules"));
    argv.push(OsString::from("--ephemeral"));
    argv.push(OsString::from("--sandbox"));
    argv.push(OsString::from("read-only"));
    argv.push(OsString::from("--cd"));
    argv.push(isolation_dir.as_os_str().to_owned());
    argv.push(OsString::from("-m"));
    argv.push(OsString::from(REQUIRED_MODEL));
    argv.push(OsString::from("-c"));
    argv.push(OsString::from(format!(
        "model_reasoning_effort=\"{REQUIRED_REASONING}\""
    )));
    argv.push(OsString::from("-c"));
    argv.push(OsString::from("web_search=\"disabled\""));
    argv.push(OsString::from("-c"));
    argv.push(OsString::from("history.persistence=\"none\""));
    argv.push(OsString::from("-c"));
    argv.push(OsString::from("features.shell_tool=false"));
    argv.push(OsString::from("-c"));
    argv.push(OsString::from(format!(
        "developer_instructions={}",
        toml_quoted(developer_instructions)?
    )));
    argv.push(OsString::from("--disable"));
    argv.push(OsString::from("shell_tool"));
    argv.push(OsString::from("--output-schema"));
    argv.push(OsString::from("result.schema.json"));
    argv.push(OsString::from("-o"));
    argv.push(OsString::from("result.json"));
    argv.push(OsString::from("-"));
    if argv.iter().any(|arg| arg == "resume") {
        return Err(err("Codex invocation must be a fresh exec, not resume"));
    }
    Ok(CodexExec { argv })
}

pub fn run_exec(
    plan: &CodexExec,
    isolation_dir: &Path,
    user_prompt: &str,
    timeout: Duration,
) -> Result<ExecOutcome, Error> {
    if plan.argv.len() < 2 {
        return Err(err("Codex invocation has no executable"));
    }
    if !plan.argv.iter().any(|arg| arg == "-") {
        return Err(err("Codex invocation must read the user prompt from stdin"));
    }
    if plan.argv.iter().any(|arg| arg == "resume") {
        return Err(err("Codex invocation must be a fresh exec, not resume"));
    }

    let mut codex = codex_process_command(&plan.argv[1..]);
    let mut child = codex
        .current_dir(isolation_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| err(format!("spawn Codex Judge: {error}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(user_prompt.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err(format!("write Judge prompt to stdin: {error}")));
        }
    }

    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|error| err(format!("poll Codex Judge: {error}")))?
        {
            Some(status) => {
                if !status.success() {
                    let stderr = child
                        .stderr
                        .take()
                        .map(|mut stream| {
                            let mut text = String::new();
                            let _ = stream.read_to_string(&mut text);
                            text.trim().to_owned()
                        })
                        .unwrap_or_default();
                    return Err(if stderr.is_empty() {
                        err(format!("Codex Judge exited unsuccessfully: {status}"))
                    } else {
                        err(format!(
                            "Codex Judge exited unsuccessfully: {status}: {stderr}"
                        ))
                    });
                }
                let result_path = isolation_dir.join("result.json");
                let result_json = std::fs::read_to_string(&result_path).map_err(|error| {
                    err(format!(
                        "Codex Judge produced no readable result.json: {error}"
                    ))
                })?;
                if result_json.trim().is_empty() {
                    return Err(err("Codex Judge produced an empty result.json"));
                }
                return Ok(ExecOutcome {
                    result_json,
                    elapsed: started.elapsed(),
                });
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(format!(
                    "Codex Judge timed out after {} ms",
                    timeout.as_millis()
                )));
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

pub fn detect_version() -> Result<String, Error> {
    let output = codex_process_command(&[OsString::from("--version")])
        .output()
        .map_err(|error| err(format!("spawn codex --version: {error}")))?;
    if !output.status.success() {
        return Err(err("codex --version failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_version(&text)
}

fn codex_process_command(args: &[OsString]) -> Command {
    #[cfg(windows)]
    {
        if let Some((node, script)) = find_node_launcher() {
            let mut command = Command::new(node);
            command.arg(script).args(args);
            return command;
        }
        if let Some(executable) = find_on_path("codex.exe") {
            let mut command = Command::new(executable);
            command.args(args);
            return command;
        }
        if let Some(script) = find_on_path("codex.ps1") {
            let mut command = Command::new("powershell.exe");
            command
                .args([
                    OsString::from("-NoProfile"),
                    OsString::from("-NonInteractive"),
                    OsString::from("-ExecutionPolicy"),
                    OsString::from("Bypass"),
                    OsString::from("-File"),
                ])
                .arg(script)
                .args(args);
            return command;
        }
    }
    let mut command = Command::new("codex");
    command.args(args);
    command
}

#[cfg(windows)]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn find_node_launcher() -> Option<(PathBuf, PathBuf)> {
    for launcher_name in ["codex.cmd", "codex.ps1"] {
        let Some(launcher) = find_on_path(launcher_name) else {
            continue;
        };
        let Some(directory) = launcher.parent() else {
            continue;
        };
        let node = directory.join("node.exe");
        let script = directory
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        let node = if node.is_file() {
            Some(node)
        } else {
            find_on_path("node.exe")
        };
        if script.is_file() {
            if let Some(node) = node {
                return Some((node, script));
            }
        }
    }
    None
}

pub fn parse_version(text: &str) -> Result<String, Error> {
    let trimmed = text.trim();
    let version = trimmed.strip_prefix("codex-cli ").unwrap_or(trimmed).trim();
    if version.is_empty() {
        return Err(err("could not parse Codex CLI version"));
    }
    Ok(version.to_owned())
}

pub fn require_pinned_version(actual: &str, pinned: &str) -> Result<(), Error> {
    if actual != pinned {
        return Err(err(format!(
            "Judge environment invalid: pinned Codex CLI {pinned}, actual {actual}"
        )));
    }
    Ok(())
}

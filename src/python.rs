use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::{
    Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent},
    Threading::CREATE_NEW_PROCESS_GROUP,
};

const BRIDGE: &str = r#"
import ast
import base64
import binascii
import hashlib
import io
import json
import math
import pathlib
import re
import signal
import struct
import sys
import zlib
from contextlib import redirect_stderr, redirect_stdout

snapshot = pathlib.Path(sys.argv[1])
selection_start = int(sys.argv[2])
selection_end = int(sys.argv[3])
buffer = bytearray(snapshot.read_bytes())
selected = memoryview(buffer)[selection_start:selection_end + 1]
if hasattr(signal, "SIGBREAK"):
    signal.signal(signal.SIGBREAK, signal.default_int_handler)

namespace = {
    "ast": ast,
    "base64": base64,
    "binascii": binascii,
    "buffer": buffer,
    "hashlib": hashlib,
    "math": math,
    "pathlib": pathlib,
    "re": re,
    "signal": signal,
    "selected": selected,
    "selection_start": selection_start,
    "selection_end": selection_end,
    "struct": struct,
    "zlib": zlib,
}

for raw in sys.stdin:
    try:
        request = json.loads(raw)
        if request["kind"] == "apply":
            snapshot.write_bytes(buffer)
            response = {"output": "Applied Python buffer to rexedit.", "error": None, "applied": True}
        else:
            source = request["source"]
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                try:
                    expression = compile(source, "<rexedit-python>", "eval")
                except SyntaxError:
                    exec(compile(source, "<rexedit-python>", "exec"), namespace, namespace)
                else:
                    value = eval(expression, namespace, namespace)
                    if value is not None:
                        print(repr(value))
            output = stdout.getvalue() + stderr.getvalue()
            response = {"output": output.rstrip(), "error": None, "applied": False}
    except BaseException as error:
        response = {"output": "", "error": f"{type(error).__name__}: {error}", "applied": False}
    print(json.dumps(response), flush=True)
"#;

#[derive(Debug)]
pub enum PythonCommand {
    Execute(String),
    Apply,
}

#[derive(Debug)]
pub struct PythonResponse {
    pub output: String,
    pub error: Option<String>,
    pub applied: bool,
}

#[derive(Debug)]
pub struct PythonSession {
    commands: Sender<PythonCommand>,
    pub responses: Receiver<PythonResponse>,
    pub snapshot: PathBuf,
    process_id: u32,
    process_running: Arc<AtomicBool>,
}

impl PythonSession {
    pub fn start(
        bytes: &[u8],
        selection_start: usize,
        selection_end: usize,
    ) -> Result<Self, String> {
        let executable = find_python().ok_or_else(|| {
            "Python was not found. Install Python 3 or set the PYTHON environment variable."
                .to_string()
        })?;
        let snapshot = snapshot_path();
        fs::write(&snapshot, bytes)
            .map_err(|error| format!("Could not create Python buffer snapshot: {error}"))?;

        let (command_tx, command_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let worker_snapshot = snapshot.clone();
        let mut child = match spawn_python(
            &executable,
            &worker_snapshot,
            selection_start,
            selection_end,
        ) {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&snapshot);
                return Err(error);
            }
        };
        let process_id = child.id();
        let process_running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&process_running);

        thread::spawn(move || {
            let result = exchange_commands(&mut child, command_rx, &response_tx);
            worker_running.store(false, Ordering::Release);
            if let Err(error) = result {
                let _ = response_tx.send(PythonResponse {
                    output: String::new(),
                    error: Some(error),
                    applied: false,
                });
            }
            let _ = fs::remove_file(worker_snapshot);
        });

        Ok(Self {
            commands: command_tx,
            responses: response_rx,
            snapshot,
            process_id,
            process_running,
        })
    }

    pub fn execute(&self, source: String) -> Result<(), String> {
        self.commands
            .send(PythonCommand::Execute(source))
            .map_err(|_| "Python interpreter is no longer running".into())
    }

    pub fn apply(&self) -> Result<(), String> {
        self.commands
            .send(PythonCommand::Apply)
            .map_err(|_| "Python interpreter is no longer running".into())
    }

    pub fn interrupt(&self) -> Result<(), String> {
        if !self.process_running.load(Ordering::Acquire) {
            return Err("Python interpreter is no longer running".into());
        }
        interrupt_process(self.process_id)
    }
}

impl Drop for PythonSession {
    fn drop(&mut self) {
        if !self.process_running.swap(false, Ordering::AcqRel) {
            return;
        }
        #[cfg(unix)]
        // SAFETY: Sending SIGKILL to the recorded child PID releases a worker blocked on Python.
        unsafe {
            libc::kill(self.process_id as i32, libc::SIGKILL);
        }

        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.process_id.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum BridgeRequest<'a> {
    Execute { source: &'a str },
    Apply,
}

#[derive(Deserialize)]
struct BridgeResponse {
    output: String,
    error: Option<String>,
    applied: bool,
}

fn spawn_python(
    executable: &str,
    snapshot: &Path,
    selection_start: usize,
    selection_end: usize,
) -> Result<Child, String> {
    let mut command = Command::new(executable);
    command
        .args([
            "-u",
            "-c",
            BRIDGE,
            &snapshot.display().to_string(),
            &selection_start.to_string(),
            &selection_end.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    command
        .spawn()
        .map_err(|error| format!("Could not start Python: {error}"))
}

#[cfg(unix)]
fn interrupt_process(process_id: u32) -> Result<(), String> {
    // SAFETY: SIGINT is sent to the known live Python child process.
    let result = unsafe { libc::kill(process_id as i32, libc::SIGINT) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "Could not interrupt Python: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(windows)]
fn interrupt_process(process_id: u32) -> Result<(), String> {
    // SAFETY: The Python child is created as its own process group, and this targets that group.
    let result = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_id) };
    if result != 0 {
        Ok(())
    } else {
        Err(format!(
            "Could not interrupt Python: {}",
            std::io::Error::last_os_error()
        ))
    }
}

fn exchange_commands(
    child: &mut Child,
    commands: Receiver<PythonCommand>,
    responses: &Sender<PythonResponse>,
) -> Result<(), String> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Could not open Python standard input".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not open Python standard output".to_string())?;
    let mut stdout = BufReader::new(stdout);

    for command in commands {
        let request = match &command {
            PythonCommand::Execute(source) => BridgeRequest::Execute { source },
            PythonCommand::Apply => BridgeRequest::Apply,
        };
        serde_json::to_writer(&mut stdin, &request)
            .map_err(|error| format!("Could not send Python command: {error}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("Could not send Python command: {error}"))?;

        let mut line = String::new();
        if stdout
            .read_line(&mut line)
            .map_err(|error| format!("Could not read Python output: {error}"))?
            == 0
        {
            return Err("Python exited unexpectedly".into());
        }
        let response: BridgeResponse = serde_json::from_str(&line)
            .map_err(|error| format!("Invalid response from Python: {error}"))?;
        responses
            .send(PythonResponse {
                output: response.output,
                error: response.error,
                applied: response.applied,
            })
            .map_err(|_| "Python pane was closed".to_string())?;
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn find_python() -> Option<String> {
    let candidates = env::var("PYTHON")
        .ok()
        .into_iter()
        .chain(["python", "python3"].into_iter().map(str::to_owned));
    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn snapshot_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("rexedit-python-{}-{nonce}.bin", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn persistent_python_session_evaluates_and_applies_buffer_edits() {
        if find_python().is_none() {
            return;
        }
        let session = PythonSession::start(&[1, 2, 3], 0, 1).unwrap();
        session.execute("sum(selected)".into()).unwrap();
        let response = session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(response.output, "3");

        session.execute("buffer[0] = 255".into()).unwrap();
        session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        session.apply().unwrap();
        let response = session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(response.applied);
        assert_eq!(fs::read(&session.snapshot).unwrap(), [255, 2, 3]);
    }

    #[test]
    fn interrupts_a_running_python_command() {
        if find_python().is_none() {
            return;
        }
        let session = PythonSession::start(&[0], 0, 0).unwrap();
        session.execute("while True: pass".into()).unwrap();
        thread::sleep(Duration::from_millis(100));
        session.interrupt().unwrap();
        let response = session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(response.error.as_deref(), Some("KeyboardInterrupt: "));
    }
}

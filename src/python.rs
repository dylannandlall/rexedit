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

MAX_CAPTURED_OUTPUT = 24_000

class LimitedOutput(io.StringIO):
    def __init__(self):
        super().__init__()
        self.truncated = False

    def write(self, text):
        remaining = MAX_CAPTURED_OUTPUT - self.tell()
        if remaining > 0:
            super().write(text[:remaining])
        if len(text) > remaining:
            self.truncated = True
        return len(text)

documents = json.loads(sys.argv[1])
active_index = int(sys.argv[2])
buffers = {}
selected_buffers = {}
selected_range_buffers = {}
selection_ranges_buffers = {}
for document in documents:
    key = f"buffer_{document['index']}"
    buffer_value = bytearray(pathlib.Path(document['snapshot']).read_bytes())
    ranges = document.get("selections") or [{
        "start": document["selection_start"],
        "end": document["selection_end"],
    }]
    selected_ranges = [
        memoryview(buffer_value)[selected_range["start"]:selected_range["end"] + 1]
        for selected_range in ranges
    ]
    selected_value = selected_ranges[0] if len(selected_ranges) == 1 else selected_ranges
    buffers[key] = buffer_value
    selected_buffers[key] = selected_value
    selected_range_buffers[key] = selected_ranges
    selection_ranges_buffers[key] = [
        (selected_range["start"], selected_range["end"])
        for selected_range in ranges
    ]
    globals()[key] = buffer_value
    globals()[f"selected_{document['index']}"] = selected_value
    globals()[f"selected_ranges_{document['index']}"] = selected_ranges
    globals()[f"selection_ranges_{document['index']}"] = selection_ranges_buffers[key]
    globals()[f"selection_start_{document['index']}"] = document['selection_start']
    globals()[f"selection_end_{document['index']}"] = document['selection_end']
buffer = buffers[f"buffer_{active_index}"]
selected = selected_buffers[f"buffer_{active_index}"]
selected_ranges = selected_range_buffers[f"buffer_{active_index}"]
selection_ranges = selection_ranges_buffers[f"buffer_{active_index}"]
selection_start = documents[active_index]['selection_start']
selection_end = documents[active_index]['selection_end']
if hasattr(signal, "SIGBREAK"):
    signal.signal(signal.SIGBREAK, signal.default_int_handler)

namespace = {
    "ast": ast,
    "base64": base64,
    "binascii": binascii,
    "buffer": buffer,
    "buffers": buffers,
    "hashlib": hashlib,
    "math": math,
    "pathlib": pathlib,
    "re": re,
    "signal": signal,
    "selected": selected,
    "selected_buffers": selected_buffers,
    "selected_ranges": selected_ranges,
    "selected_range_buffers": selected_range_buffers,
    "selection_start": selection_start,
    "selection_end": selection_end,
    "selection_ranges": selection_ranges,
    "selection_ranges_buffers": selection_ranges_buffers,
    "struct": struct,
    "zlib": zlib,
}
for document in documents:
    index = document["index"]
    key = f"buffer_{index}"
    namespace[key] = buffers[key]
    namespace[f"selected_{index}"] = selected_buffers[key]
    namespace[f"selected_ranges_{index}"] = selected_range_buffers[key]
    namespace[f"selection_start_{index}"] = document["selection_start"]
    namespace[f"selection_end_{index}"] = document["selection_end"]
    namespace[f"selection_ranges_{index}"] = selection_ranges_buffers[key]

def rexedit_help():
    print("Active: buffer, selected, selected_ranges, selection_start/end, selection_ranges")
    print("All binaries: buffers['buffer_N'], selected_buffers['buffer_N'], selected_range_buffers['buffer_N']")
    print("Imports: ast, base64, binascii, hashlib, math, pathlib, re, signal, struct, zlib")
    print("Use :apply in rexedit to apply same-length changes to every open buffer.")

namespace["rexedit_help"] = rexedit_help

for raw in sys.stdin:
    try:
        request = json.loads(raw)
        if request["kind"] == "apply":
            for document in documents:
                key = f"buffer_{document['index']}"
                pathlib.Path(document['snapshot']).write_bytes(buffers[key])
            response = {"output": "Applied Python buffers to rexedit.", "error": None, "applied": True}
        else:
            source = request["source"]
            stdout = LimitedOutput()
            stderr = LimitedOutput()
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
            if stdout.truncated or stderr.truncated:
                output += "\n[Python output truncated; print a slice or summary for more detail.]"
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
pub struct PythonDocument {
    pub index: usize,
    pub bytes: Vec<u8>,
    pub selection_start: usize,
    pub selection_end: usize,
    pub selections: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct PythonSnapshot {
    pub index: usize,
    pub path: PathBuf,
    pub baseline: Vec<u8>,
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
    pub snapshots: Vec<PythonSnapshot>,
    process_id: u32,
    process_running: Arc<AtomicBool>,
}

impl PythonSession {
    pub fn start(documents: Vec<PythonDocument>, active_index: usize) -> Result<Self, String> {
        let executable = find_python().ok_or_else(|| {
            "Python was not found. Install Python 3 or set the PYTHON environment variable."
                .to_string()
        })?;
        let snapshots = documents
            .iter()
            .map(|document| {
                let path = snapshot_path(document.index);
                fs::write(&path, &document.bytes)
                    .map_err(|error| format!("Could not create Python buffer snapshot: {error}"))?;
                Ok(PythonSnapshot {
                    index: document.index,
                    path,
                    baseline: document.bytes.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if snapshots.get(active_index).is_none() {
            return Err("Active Python document is unavailable".into());
        }
        let bridge_documents = documents
            .iter()
            .zip(&snapshots)
            .map(|(document, snapshot)| {
                serde_json::json!({
                    "index": document.index,
                    "snapshot": snapshot.path,
                    "selection_start": document.selection_start,
                    "selection_end": document.selection_end,
                    "selections": document
                        .selections
                        .iter()
                        .map(|(start, end)| serde_json::json!({ "start": start, "end": end }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let bridge_documents = serde_json::to_string(&bridge_documents)
            .map_err(|error| format!("Could not prepare Python buffers: {error}"))?;

        let (command_tx, command_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let worker_snapshots = snapshots.clone();
        let mut child = match spawn_python(&executable, &bridge_documents, active_index) {
            Ok(child) => child,
            Err(error) => {
                for snapshot in snapshots_for_cleanup(&worker_snapshots) {
                    let _ = fs::remove_file(snapshot);
                }
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
            for snapshot in snapshots_for_cleanup(&worker_snapshots) {
                let _ = fs::remove_file(snapshot);
            }
        });

        Ok(Self {
            commands: command_tx,
            responses: response_rx,
            snapshots,
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

fn spawn_python(executable: &str, documents: &str, active_index: usize) -> Result<Child, String> {
    let mut command = Command::new(executable);
    command
        .args(["-u", "-c", BRIDGE, documents, &active_index.to_string()])
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

fn snapshot_path(index: usize) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!(
        "rexedit-python-{}-{index}-{nonce}.bin",
        std::process::id()
    ))
}

fn snapshots_for_cleanup(snapshots: &[PythonSnapshot]) -> impl Iterator<Item = &Path> {
    snapshots.iter().map(|snapshot| snapshot.path.as_path())
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
        let session = PythonSession::start(
            vec![PythonDocument {
                index: 0,
                bytes: vec![1, 2, 3],
                selection_start: 0,
                selection_end: 1,
                selections: vec![(0, 1)],
            }],
            0,
        )
        .unwrap();
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
        assert_eq!(fs::read(&session.snapshots[0].path).unwrap(), [255, 2, 3]);
    }

    #[test]
    fn exposes_and_applies_every_open_binary_buffer() {
        if find_python().is_none() {
            return;
        }
        let session = PythonSession::start(
            vec![
                PythonDocument {
                    index: 0,
                    bytes: vec![1],
                    selection_start: 0,
                    selection_end: 0,
                    selections: vec![(0, 0)],
                },
                PythonDocument {
                    index: 1,
                    bytes: vec![2],
                    selection_start: 0,
                    selection_end: 0,
                    selections: vec![(0, 0)],
                },
            ],
            0,
        )
        .unwrap();
        session
            .execute("buffer_0[0] = 10\nbuffer_1[0] = 20".into())
            .unwrap();
        session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        session.apply().unwrap();
        assert!(
            session
                .responses
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .applied
        );
        assert_eq!(fs::read(&session.snapshots[0].path).unwrap(), [10]);
        assert_eq!(fs::read(&session.snapshots[1].path).unwrap(), [20]);
    }

    #[test]
    fn exposes_separate_selections_as_individual_python_views() {
        if find_python().is_none() {
            return;
        }
        let session = PythonSession::start(
            vec![PythonDocument {
                index: 0,
                bytes: vec![1, 2, 3, 4],
                selection_start: 0,
                selection_end: 0,
                selections: vec![(0, 0), (2, 3)],
            }],
            0,
        )
        .unwrap();
        session
            .execute(
                "len(selected), len(selected_ranges), sum(len(part) for part in selected)".into(),
            )
            .unwrap();
        let response = session
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(response.output, "(2, 2, 3)");
    }

    #[test]
    fn interrupts_a_running_python_command() {
        if find_python().is_none() {
            return;
        }
        let session = PythonSession::start(
            vec![PythonDocument {
                index: 0,
                bytes: vec![0],
                selection_start: 0,
                selection_end: 0,
                selections: vec![(0, 0)],
            }],
            0,
        )
        .unwrap();
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

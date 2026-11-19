//! Runs one hook invocation (design section 5, "Hook test run"). The invocation comes from the
//! agent's `exec_spec`. The child gets its own process group, the sample input on stdin, and is
//! killed with its group at the timeout. Its output is kept up to a cap.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cm_core::env::ScanEnv;
use cm_core::probe_fs::ProbeFs;
use cm_core::program::{self, Program};
use cm_core::*;

/// Each of stdout and stderr keeps at most this many bytes.
pub const OUTPUT_CAP: usize = 1024 * 1024;
/// A hook with no timeout of its own runs at most this long.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// No hook runs longer than this, whatever it declares.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// How long to wait for the output threads after the process is gone.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

pub fn run(spec: ExecSpec, env: &ScanEnv, timeout: Option<Duration>) -> Result<HookRun, ApiError> {
    let io = |detail: String| ApiError::Io { detail };
    let Some(first) = spec.argv.first() else {
        return Err(io("the invocation is empty".to_owned()));
    };
    let program = match program::resolve(env, &ProbeFs::new(), &spec.cwd, first) {
        Program::Found { path, .. } => path,
        Program::NotFound => return Err(io(format!("cannot find the program {first}"))),
    };
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);

    let mut command = Command::new(program.as_path());
    command
        .args(&spec.argv[1..])
        .current_dir(spec.cwd.as_path())
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| io(format!("cannot start {first}: {e}")))?;
    let group = child.id() as libc::pid_t;

    let stdin = child.stdin.take();
    let input = spec.stdin.clone();
    thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            // A hook that never reads its input closes the pipe. That is not an error.
            let _ = stdin.write_all(input.as_bytes());
        }
    });
    let stdout = Capture::start(child.stdout.take());
    let stderr = Capture::start(child.stderr.take());

    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|e| io(e.to_string()))? {
            Some(status) => break status,
            None if started.elapsed() >= timeout => {
                timed_out = true;
                kill_group(group);
                break child.wait().map_err(|e| io(e.to_string()))?;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    // Whatever the hook left running in its group goes with it.
    kill_group(group);
    let ms = started.elapsed().as_millis() as u64;
    Ok(HookRun {
        spec,
        exit_code: status.code(),
        stdout: stdout.finish(),
        stderr: stderr.finish(),
        ms,
        timed_out,
    })
}

fn kill_group(group: libc::pid_t) {
    // SAFETY: `kill` with a negative pid signals a process group and touches no memory. The
    // group exists because the child was started with `process_group(0)`. An error means the
    // group is already gone.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

/// Reads a pipe on its own thread, keeping the first [`OUTPUT_CAP`] bytes and draining the rest.
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Arc<Mutex<bool>>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Capture {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(Mutex::new(pipe.is_none()));
        if let Some(mut pipe) = pipe {
            let (bytes, done) = (bytes.clone(), done.clone());
            thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut kept = bytes.lock().expect("capture");
                    let room = OUTPUT_CAP.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
                *done.lock().expect("capture") = true;
            });
        }
        Capture { bytes, done }
    }

    /// The text read so far, once the pipe closes or the grace period ends.
    fn finish(self) -> String {
        let deadline = Instant::now() + DRAIN_GRACE;
        while !*self.done.lock().expect("capture") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let bytes = self.bytes.lock().expect("capture");
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

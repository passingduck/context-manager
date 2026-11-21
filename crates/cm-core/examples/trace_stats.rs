//! Reads Claude Code session records with the trace reader and prints numbers and kinds only:
//! never a text, a title, a path or an identifier from the record, so it can run on private
//! transcripts.
//!
//! ```text
//! cargo run --release -p cm-core --example trace_stats -- <transcript.jsonl>
//! cargo run --release -p cm-core --example trace_stats -- --row <transcript.jsonl> ...
//! ```
//!
//! The first form prints, for one record: steps, steps with a request, exact requests, problems
//! by kind, pieces by kind, the time to open, and the peak memory of the process. `--row` prints
//! one line per record, `index requests exact`, to compare with
//! `tools/oracle/claude/session_shape.py --row`.

use std::collections::BTreeMap;
use std::time::Instant;

use cm_core::agents::claude::read_record;
use cm_core::probe_fs::ProbeFs;
use cm_core::trace::OpenTrace;
use cm_core::{AbsPath, PieceKind, TraceProblem};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let row = args.iter().any(|a| a == "--row");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if files.is_empty() {
        eprintln!("usage: trace_stats [--row] <transcript.jsonl> ...");
        std::process::exit(2);
    }
    if row {
        println!("| # | requests | exact |");
        println!("| -: | -: | -: |");
    }
    for (index, file) in files.iter().enumerate() {
        let path = std::fs::canonicalize(file).expect("the file exists");
        let path = AbsPath::parse(path.to_str().expect("a UTF-8 path")).expect("an absolute path");
        let started = Instant::now();
        let data = match read_record(&ProbeFs::new(), &path) {
            Ok(data) => data,
            Err(err) => {
                println!("| {index} | error {} | |", error_kind(&err));
                continue;
            }
        };
        let bytes = data.info.bytes;
        let read_ms = started.elapsed().as_millis();
        let open = OpenTrace::new(data);
        let total_ms = started.elapsed().as_millis();
        let trace = open.trace();
        let requests = trace.steps.iter().filter(|s| s.sent.is_some()).count();
        let exact = trace
            .steps
            .iter()
            .filter(|s| s.sent.as_ref().is_some_and(|s| s.exact))
            .count();
        if row {
            println!("| {index} | {requests} | {exact} |");
            continue;
        }
        println!("file bytes: {bytes}");
        println!("steps: {}", trace.steps.len());
        println!("steps with a request: {requests}");
        println!("exact requests: {exact}");
        println!(
            "steps with usage: {}",
            trace.steps.iter().filter(|s| s.usage.is_some()).count()
        );
        println!("boundaries: {}", trace.boundaries.len());
        println!("parts: {}", trace.parts.len());
        for problem in &trace.problems {
            println!("problem: {}", problem_line(problem));
        }
        let mut kinds: BTreeMap<PieceKind, (u64, u64)> = BTreeMap::new();
        for (i, step) in trace.steps.iter().enumerate() {
            for piece in open.step_pieces(i as u32).unwrap_or_default() {
                kinds.entry(piece.kind).or_default().0 += 1;
            }
            for total in step.sent.iter().flat_map(|s| &s.by_kind) {
                kinds.entry(total.kind).or_default().1 += u64::from(total.fresh);
            }
        }
        println!("pieces by kind (pieces over all requests, fresh tokens):");
        for (kind, (pieces, fresh)) in kinds {
            println!("  {kind:?}: {pieces} {fresh}");
        }
        println!("read ms: {read_ms}");
        println!("open ms (read, count, project): {total_ms}");
        let peak = peak_bytes();
        println!("peak memory bytes: {peak}");
        if bytes > 0 {
            println!(
                "peak memory over file size: {:.2}",
                peak as f64 / bytes as f64
            );
        }
    }
}

fn problem_line(problem: &TraceProblem) -> String {
    match problem {
        TraceProblem::NoRequests { steps } => format!("noRequests steps {steps}"),
        TraceProblem::Inexact { steps } => format!("inexact steps {steps}"),
        TraceProblem::UnreadableLines { lines } => format!("unreadableLines lines {lines}"),
        TraceProblem::RecordTooLarge { bytes, limit } => {
            format!("recordTooLarge bytes {bytes} limit {limit}")
        }
    }
}

fn error_kind(err: &cm_core::ApiError) -> String {
    serde_json::to_value(err)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The peak resident set of this process. `ru_maxrss` is bytes on macOS and KiB on Linux.
fn peak_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes into the struct it is given and reads nothing else.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let max = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        max
    } else {
        max * 1024
    }
}

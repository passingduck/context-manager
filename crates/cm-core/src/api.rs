//! The operation table (design section 6). Every operation is declared once, here.
//!
//! `api!` expands to `enum Request`, `trait Api` (one async method per operation), `dispatch`,
//! and `operations()`. The desktop shell and the serve shell forward a `Request` to `dispatch`
//! and know nothing about individual operations.

use std::future::Future;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::*;

/// One row of the table, for the generated `api-map.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// The wire name, the `op` tag of a request.
    pub name: &'static str,
    /// The TypeScript type of the arguments.
    pub args: String,
    /// The TypeScript type of the result.
    pub result: String,
    /// Output path (relative to the export directory) of each named type the two types use.
    pub imports: Vec<(String, String)>,
}

macro_rules! api {
    ($( $variant:ident => $method:ident($args:ty) -> $ret:ty; )*) => {
        /// A request as it crosses the boundary: `{"op": "scan", "args": {...}}`.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "op", content = "args", rename_all = "snake_case")]
        #[ts(export)]
        pub enum Request {
            $( $variant($args), )*
        }

        pub trait Api: Send + Sync {
            $(
                fn $method(&self, args: $args) -> impl Future<Output = Result<$ret, ApiError>> + Send;
            )*
        }

        /// Runs one request and returns its result as JSON.
        pub async fn dispatch<A: Api>(api: &A, request: Request) -> Result<serde_json::Value, ApiError> {
            match request {
                $(
                    Request::$variant(args) => {
                        let result = api.$method(args).await?;
                        serde_json::to_value(result).map_err(|e| ApiError::Io { detail: e.to_string() })
                    }
                )*
            }
        }

        /// Every operation with its argument and result types.
        pub fn operations() -> Vec<Operation> {
            let cfg = ts_rs::Config::from_env();
            vec![$(
                Operation {
                    name: stringify!($method),
                    args: <$args as TS>::name(&cfg),
                    result: <$ret as TS>::name(&cfg),
                    imports: {
                        let mut deps = <$args as TS>::dependencies(&cfg);
                        deps.extend(<$ret as TS>::dependencies(&cfg));
                        deps.extend(ts_rs::Dependency::from_ty::<$args>(&cfg));
                        deps.extend(ts_rs::Dependency::from_ty::<$ret>(&cfg));
                        deps.sort();
                        deps.dedup();
                        deps.into_iter()
                            .map(|d| (d.ts_name, d.output_path.to_string_lossy().into_owned()))
                            .collect()
                    },
                },
            )*]
        }
    };
}

api! {
    // version, shell, read_only, launch_cwd, home, os, pending changes
    Session => session(()) -> Session;
    // id, display name, RuleSpecs, HookSchema
    Agents => agents(()) -> Vec<AgentInfo>;
    // path, exists, which agents know it
    Projects => projects(()) -> Vec<KnownProject>;
    Scan => scan(ScanReq) -> Snapshot;
    // state plus text; the path must be in the snapshot
    ReadFile => read_file(ReadReq) -> FileContent;
    // literal session-start text
    Render => render(RenderReq) -> Vec<RenderedBlock>;
    // per load of one file: the text the model receives there
    LoadTexts => load_texts(LoadTextsReq) -> LoadTexts;
    // per agent: fired loads, extra loads, added tokens, notes
    Simulate => simulate(SimReq) -> Vec<Simulation>;
    // at most 500 hits
    Search => search(SearchReq) -> SearchResult;
    // long poll, at most 25 s
    Watch => watch(WatchReq) -> Fingerprint;
    Plan => plan(PlanReq) -> Preview;
    Apply => apply(ApplyReq) -> ApplyResult;
    History => history(HistoryReq) -> Vec<ChangeRecord>;
    // the record plus before and after text per file
    Change => change(ChangeReq) -> ChangeDetail;
    HookSample => hook_sample(SampleReq) -> serde_json::Value;
    // ExecSpec, exit code, stdout, stderr, ms, timed_out
    RunHook => run_hook(RunReq) -> HookRun;
    // every agent's recorded sessions that started in cwd or below, newest first
    Traces => traces(TracesReq) -> Vec<TraceInfo>;
    // the calls of one session, or of one subagent of it
    Trace => trace(TraceReq) -> Trace;
    // the pieces of one request; Stale when the record changed under the trace
    TraceStep => trace_step(TraceStepReq) -> StepPieces;
    // the text of one piece, at most 1 MiB
    TracePiece => trace_piece(TracePieceReq) -> PieceText;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_names_are_the_wire_tags() {
        let request = Request::ReadFile(ReadReq {
            cwd: AbsPath::parse("/p").unwrap(),
            path: AbsPath::parse("/p/a").unwrap(),
        });
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["op"], "read_file");
        let names: Vec<_> = operations().iter().map(|o| o.name).collect();
        assert!(names.contains(&"read_file") && names.contains(&"run_hook"));
        let hook_sample = Request::HookSample(SampleReq {
            cwd: AbsPath::parse("/p").unwrap(),
            agent: AgentId::parse("codex").unwrap(),
            event: "Stop".into(),
        });
        assert_eq!(
            serde_json::to_value(&hook_sample).unwrap()["op"],
            "hook_sample"
        );
    }

    #[test]
    fn a_unit_request_parses_from_null_args() {
        let request: Request = serde_json::from_str(r#"{"op":"session","args":null}"#).unwrap();
        assert_eq!(request, Request::Session(()));
    }
}

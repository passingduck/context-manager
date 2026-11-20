//! Agent check for the Claude planners: a settings file that the planners wrote must pass
//! `claude doctor`, the installed Claude Code's own validation of settings and hooks. The test
//! runs against a fixture home and `CLAUDE_CONFIG_DIR`, never `~/.claude`, and skips with a
//! message when `claude` is not installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cm_core::agents::registry;
use cm_core::edit::{ChangeSource, Planner};
use cm_core::model::*;
use cm_core::scan::scan_with_parked;
use cm_core::testing::{Fixture, cli_path};

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|dir| Path::new(dir).join(bin))
        .find(|p| p.is_file())
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let p = Path::new(&home).join(".local/bin").join(bin);
            p.is_file().then_some(p)
        })
}

struct Rig {
    fx: Fixture,
    parked: AbsPath,
}

impl Rig {
    fn plan_and_apply(&self, edit: &Edit) {
        let env = self.fx.env();
        let scanned = scan_with_parked(registry(), &env, Some(&self.parked));
        let preview = Planner {
            agents: registry(),
            scanned: &scanned,
            env: &env,
            parked_dir: &self.parked,
            changes: &NoHistory,
        }
        .plan(edit)
        .unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        for op in &preview.change_set.ops {
            match &op.action {
                Action::Write { text } => self.fx.write(&op.path, text),
                Action::Trash => std::fs::remove_file(op.path.as_path()).unwrap(),
                Action::TrashDir => unreachable!(),
            }
        }
    }

    fn hook(&self, command: &str) -> Hook {
        let env = self.fx.env();
        let scanned = scan_with_parked(registry(), &env, Some(&self.parked));
        scanned
            .snapshot
            .agents
            .into_iter()
            .flat_map(|v| v.hooks)
            .find(|h| h.handler.summary == command)
            .unwrap_or_else(|| panic!("no hook {command}"))
    }

    /// What `claude doctor` prints under the fixture config directory.
    fn doctor(&self, claude: &Path) -> String {
        let out = Command::new(claude)
            .arg("doctor")
            .current_dir(self.fx.project().as_path())
            .env_clear()
            .env(
                "PATH",
                cli_path(
                    &Path::new(self.fx.root().as_str()).join("bin"),
                    &std::env::var("PATH").unwrap_or_default(),
                ),
            )
            .env("HOME", self.fx.home().as_str())
            .env("CLAUDE_CONFIG_DIR", self.fx.home_path(".claude").as_str())
            .stdin(Stdio::null())
            .output()
            .expect("run claude doctor");
        assert!(out.status.success(), "claude doctor failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn add(file: &AbsPath, event: &str, matcher: Option<&str>, handler: serde_json::Value) -> Edit {
    Edit::AddHook {
        agent: AgentId::parse("claude").unwrap(),
        file: file.clone(),
        draft: HookDraft {
            event: event.into(),
            matcher: matcher.map(str::to_owned),
            handler,
        },
    }
}

#[test]
fn settings_the_planners_wrote_pass_claude_doctor() {
    let Some(claude) = which("claude") else {
        eprintln!("SKIP: `claude` is not installed");
        return;
    };
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    let parked = fx.root().join("data/parked");
    let rig = Rig { fx, parked };
    let settings = rig.fx.home_path(".claude/settings.json");
    rig.fx.write(&settings, "{\n  \"model\": \"opus\"\n}\n");

    // The check can fail: a hook Claude Code cannot load is named under "Invalid settings".
    let broken = rig.fx.home_path(".claude/broken/settings.json");
    rig.fx.write(
        &broken,
        "{\"hooks\":{\"PreToolUse\":[{\"matcher\":\"Edit\",\"hooks\":[{\"type\":\"command\"}]}]}}",
    );
    std::fs::copy(broken.as_path(), settings.as_path()).unwrap();
    let report = rig.doctor(&claude);
    assert!(
        report.contains("Invalid settings") && report.contains("hooks.PreToolUse"),
        "the control did not trip:\n{report}"
    );
    rig.fx.write(&settings, "{\n  \"model\": \"opus\"\n}\n");
    assert!(!rig.doctor(&claude).contains("Invalid settings"));

    for edit in [
        add(
            &settings,
            "PreToolUse",
            Some("Bash"),
            serde_json::json!({"type": "command", "command": "echo checked", "timeout": 5}),
        ),
        add(
            &settings,
            "Stop",
            None,
            serde_json::json!({"type": "prompt", "prompt": "Did the tests pass?"}),
        ),
        add(
            &settings,
            "PostToolUse",
            Some("Edit|Write"),
            serde_json::json!({"type": "http", "url": "https://example.com/hook"}),
        ),
    ] {
        rig.plan_and_apply(&edit);
        let report = rig.doctor(&claude);
        assert!(
            !report.contains("Invalid settings"),
            "after {edit:?}:\n{}\n{report}",
            std::fs::read_to_string(settings.as_path()).unwrap()
        );
    }

    // Park a hook, update another, and put the parked one back.
    let parked_hook = rig.hook("echo checked");
    rig.plan_and_apply(&Edit::SetHookEnabled {
        hook: parked_hook.id,
        enabled: false,
    });
    assert!(!rig.doctor(&claude).contains("Invalid settings"));
    let stop = rig.hook("Did the tests pass?");
    rig.plan_and_apply(&Edit::UpdateHook {
        hook: stop.id,
        draft: HookDraft {
            event: "Stop".into(),
            matcher: None,
            handler: serde_json::json!({"type": "prompt", "prompt": "Did every test pass?"}),
        },
    });
    let back = rig.hook("echo checked");
    rig.plan_and_apply(&Edit::SetHookEnabled {
        hook: back.id,
        enabled: true,
    });
    assert!(!rig.doctor(&claude).contains("Invalid settings"));
    rig.plan_and_apply(&Edit::RemoveHook {
        hook: rig.hook("https://example.com/hook").id,
    });
    assert!(!rig.doctor(&claude).contains("Invalid settings"));
}

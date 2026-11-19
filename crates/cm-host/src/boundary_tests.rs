//! The write boundary through the host, with the home directory as the working directory. A hook
//! defined in a file of an installed Gemini extension, Claude plugin or Codex plugin is read only:
//! no edit rewrites that file.

use std::path::Path;
use std::time::Duration;

use cm_core::agents::registry;
use cm_core::testing::Fixture;
use cm_core::*;
use serde_json::json;

use crate::apply::Trasher;
use crate::host::{Host, HostConfig};

struct NoTrash;

impl Trasher for NoTrash {
    fn trash(&self, _: &Path) -> Result<(), String> {
        Err("a hook edit never trashes".into())
    }
}

fn host(fx: &Fixture) -> Host {
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: fx.env().with_cwd(fx.home()),
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        registry(),
        Box::new(NoTrash),
    )
}

fn hooks_json(event: &str, matcher: Option<&str>, command: &str) -> String {
    let mut group = json!({"hooks": [{"type": "command", "command": command}]});
    if let Some(m) = matcher {
        group["matcher"] = json!(m);
    }
    json!({"hooks": {event: [group]}}).to_string()
}

/// An installed extension or plugin of each agent, each with one hook in its `hooks.json`.
/// Returns the three hook files with the command that marks each hook.
fn installed(fx: &Fixture) -> Vec<(AbsPath, &'static str)> {
    let gemini = fx.home_path(".gemini/extensions/foo");
    fx.write(
        &gemini.join("gemini-extension.json"),
        r#"{"name":"foo","version":"1.0.0"}"#,
    );
    let gemini_hooks = gemini.join("hooks/hooks.json");
    fx.write(
        &gemini_hooks,
        &hooks_json("BeforeTool", Some("x"), "echo gemini-ext"),
    );

    let claude = fx.home_path(".claude/plugins/cache/mkt/p/1.0.0");
    fx.write(
        &claude.join(".claude-plugin/plugin.json"),
        r#"{"name":"p"}"#,
    );
    let claude_hooks = claude.join("hooks/hooks.json");
    fx.write(
        &claude_hooks,
        &hooks_json("Stop", None, "echo claude-plugin"),
    );
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"p@mkt": [{"scope": "user", "installPath": claude.as_str()}]}})
            .to_string(),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"enabledPlugins":{"p@mkt":true}}"#,
    );

    let codex = fx.home_path(".codex/plugins/cache/mkt/demo/1.0.0");
    fx.write(
        &codex.join(".codex-plugin/plugin.json"),
        r#"{"name":"demo"}"#,
    );
    let codex_hooks = codex.join("hooks/hooks.json");
    fx.write(&codex_hooks, &hooks_json("Stop", None, "echo codex-plugin"));
    fx.write(
        &fx.home_path(".codex/config.toml"),
        "[plugins.\"demo@mkt\"]\nenabled = true\n",
    );

    vec![
        (gemini_hooks, "echo gemini-ext"),
        (claude_hooks, "echo claude-plugin"),
        (codex_hooks, "echo codex-plugin"),
    ]
}

fn bytes(path: &AbsPath) -> Vec<u8> {
    std::fs::read(path.as_path()).unwrap()
}

/// Plans, then applies with the digest the plan returned, or with a made-up one when the plan
/// failed, so that `apply` runs its own plan.
async fn plan_and_apply(
    host: &Host,
    cwd: &AbsPath,
    edit: &Edit,
) -> (Result<Preview, ApiError>, Result<ApplyResult, ApiError>) {
    let planned = host
        .plan(PlanReq {
            cwd: cwd.clone(),
            edit: edit.clone(),
        })
        .await;
    let digest = planned
        .as_ref()
        .map_or_else(|_| "none".to_owned(), |p| p.digest.clone());
    let applied = host
        .apply(ApplyReq {
            cwd: cwd.clone(),
            edit: edit.clone(),
            digest,
        })
        .await;
    (planned, applied)
}

#[tokio::test]
async fn a_hook_of_an_installed_extension_or_plugin_is_never_rewritten() {
    let fx = Fixture::new();
    let files = installed(&fx);
    let host = host(&fx);
    let cwd = fx.home();
    let snapshot = host.scan(ScanReq { cwd: cwd.clone() }).await.unwrap();

    for (file, command) in &files {
        let original = bytes(file);
        let hook = snapshot
            .agents
            .iter()
            .flat_map(|view| &view.hooks)
            .find(|hook| hook.handler.summary.contains(command))
            .unwrap_or_else(|| panic!("no hook runs {command}"));
        assert_eq!(hook.source.file, *file);

        let draft = HookDraft {
            event: hook.event.clone(),
            matcher: hook.matcher.raw.clone(),
            handler: json!({"type": "command", "command": "echo changed"}),
        };
        for edit in [
            Edit::RemoveHook {
                hook: hook.id.clone(),
            },
            Edit::UpdateHook {
                hook: hook.id.clone(),
                draft,
            },
        ] {
            let (planned, applied) = plan_and_apply(&host, &cwd, &edit).await;
            assert!(
                matches!(planned, Err(ApiError::ReadOnly { .. })),
                "{command}: plan of {edit:?} gave {planned:?}"
            );
            assert!(
                matches!(applied, Err(ApiError::ReadOnly { .. })),
                "{command}: apply of {edit:?} gave {applied:?}"
            );
            assert_eq!(bytes(file), original, "{command}: {edit:?}");
        }

        // An agent may switch such a hook off in a file of its own, never in the hook's file.
        let switch = Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false,
        };
        let (planned, _) = plan_and_apply(&host, &cwd, &switch).await;
        if let Ok(preview) = planned {
            assert!(
                preview.change_set.ops.iter().all(|op| op.path != *file),
                "{command}: the switch plans {:?}",
                preview.change_set.ops
            );
        }
        assert_eq!(bytes(file), original, "{command}: the switch");
        assert!(
            matches!(
                snapshot.files.get(file).map(|entry| &entry.policy),
                Some(EditPolicy::ReadOnly { .. })
            ),
            "{file} is in the snapshot as read only"
        );
    }
}

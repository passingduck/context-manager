//! A behaviour dump for refactors: the scan of five fixture trees and the plan of a fixed list of
//! edits, as JSON. Run it before and after a structural change and compare the two files.
//!
//! ```text
//! CM_DUMP_OUT=/tmp/before.json cargo test -p cm-core --test dump_behaviour -- --ignored
//! ```
//!
//! The fixture trees live at a fixed path under the system temporary directory, so the digests
//! of the plans are the same from run to run. Fields that hold a clock reading are zeroed.

use std::path::{Path, PathBuf};

use cm_core::agents::registry;
use cm_core::edit::{ChangeSource, Planner};
use cm_core::env::ScanEnv;
use cm_core::scan::scan_with_parked;
use cm_core::simulate::simulate;
use cm_core::*;
use serde_json::{Value, json};

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(case: &str) -> Tree {
        let base = std::env::temp_dir().join("cm-dump-trees");
        std::fs::create_dir_all(&base).unwrap();
        let root = base.canonicalize().unwrap().join(case);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("project/.git")).unwrap();
        Tree { root }
    }

    fn abs(&self, rel: &str) -> AbsPath {
        AbsPath::parse(self.root.join(rel).to_str().unwrap()).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn env(&self) -> ScanEnv {
        ScanEnv::for_fixture(self.abs("home"), self.abs("project"))
            .with_var("CONTEXT_MANAGER_MANAGED_DIR", self.abs("managed").as_str())
    }
}

fn claude_key(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude(t: &Tree) {
    let h = "home/.claude";
    t.write(
        &format!("{h}/settings.json"),
        &json!({"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "echo stop"}]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "echo one", "timeout": 5},
                {"type": "command", "command": "echo two", "x-unknown": 1}]}],
        }, "enabledPlugins": {"on@mkt": true}})
        .to_string(),
    );
    t.write(
        "project/.claude/settings.local.json",
        "{\n  \"hooks\": {\"SessionStart\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo local\"}]}]},\n  \"permissions\": {}\n}\n",
    );
    t.write(
        "project/.claude/settings.json",
        "{\n  \"hooks\": {\"PostToolUse\": [{\"matcher\": \"Edit\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo project\"}]}]}\n}\n",
    );
    t.write(&format!("{h}/CLAUDE.md"), "# user\n@notes.md\n");
    t.write(&format!("{h}/notes.md"), "notes\n");
    t.write(
        "project/CLAUDE.md",
        "# project\nSee [a](docs/a.md)\n@docs/b.md\n",
    );
    t.write("project/docs/a.md", "a\n");
    t.write("project/docs/b.md", "b\n");
    t.write("project/sub/CLAUDE.md", "# sub\n");
    t.write("project/sub/deep/CLAUDE.md", "# deep\n");
    t.write(
        "project/.claude/rules/r.md",
        "---\npaths:\n  - \"src/**\"\n---\nrule\n",
    );
    t.write("project/.claude/rules/plain.md", "plain rule\n");
    t.write(
        &format!("{h}/skills/s1/SKILL.md"),
        "---\nname: s1\ndescription: first\n---\nbody\n",
    );
    t.write(
        "project/.claude/commands/c1.md",
        "---\ndescription: cmd\n---\nrun\n",
    );
    t.write(
        "project/.claude/agents/a1.md",
        "---\nname: a1\ndescription: agent\n---\nprompt\n",
    );
    let key = claude_key(t.abs("project").as_str());
    t.write(
        &format!("{h}/projects/{key}/memory/MEMORY.md"),
        "- [Topic](topic.md) - hook\n",
    );
    t.write(
        &format!("{h}/projects/{key}/memory/topic.md"),
        "---\nname: topic\ndescription: d\n---\nbody\n",
    );
    let plugin = t.abs(&format!("{h}/plugins/cache/mkt/on/1.0.0"));
    t.write(
        &format!("{h}/plugins/cache/mkt/on/1.0.0/.claude-plugin/plugin.json"),
        &json!({"name": "on"}).to_string(),
    );
    t.write(
        &format!("{h}/plugins/cache/mkt/on/1.0.0/hooks/hooks.json"),
        &json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "plug.sh"}]}]}})
            .to_string(),
    );
    t.write(
        &format!("{h}/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"on@mkt": [{"scope": "user", "installPath": plugin.as_str()}]}})
            .to_string(),
    );
}

fn codex(t: &Tree) {
    let h = "home/.codex";
    t.write(
        &format!("{h}/config.toml"),
        "# my codex config\nmodel = \"gpt-5\"\n\n[features]\nhooks = true\n\n[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"echo one\"\ntimeout = 5\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo stop\"\n\n[hooks.state.\"some:other:key\"]\ntrusted_hash = \"sha256:abc\"\n\n[plugins.\"demo@mkt\"]\nenabled = true\n",
    );
    t.write(
        &format!("{h}/hooks.json"),
        "{\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo json\"}]}]}\n}\n",
    );
    t.write(&format!("{h}/AGENTS.md"), "# user agents\n");
    t.write("project/AGENTS.md", "# project agents\nsee [x](docs/x.md)\n");
    t.write("project/docs/x.md", "x\n");
    t.write("project/sub/AGENTS.md", "# sub agents\n");
    t.write(
        "project/.codex/config.toml",
        "[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"echo project\"\n",
    );
    t.write(
        &format!("{h}/skills/sk/SKILL.md"),
        "---\nname: sk\ndescription: a skill\n---\nbody\n",
    );
    t.write(&format!("{h}/memories/MEMORY.md"), "# generated\n");
    t.write(&format!("{h}/memories/memory_summary.md"), "s\n");
    t.write(&format!("{h}/memories/notes/n1.md"), "note\n");
    let base = format!("{h}/plugins/cache/mkt/demo/1.0.0");
    t.write(
        &format!("{base}/.codex-plugin/plugin.json"),
        &json!({"name": "demo", "hooks": "./hooks/hooks.json"}).to_string(),
    );
    t.write(
        &format!("{base}/hooks/hooks.json"),
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "plug.sh"}]}]}})
            .to_string(),
    );
    t.write(
        &format!("{base}/skills/lint/SKILL.md"),
        "---\nname: lint\ndescription: l\n---\nb\n",
    );
}

fn gemini(t: &Tree) {
    let h = "home/.gemini";
    t.write(
        &format!("{h}/settings.json"),
        "// my settings\n{\n\t\"hooks\": {\n\t\t\"BeforeTool\": [\n\t\t\t{ \"matcher\": \"^write\", \"hooks\": [ { \"type\": \"command\", \"command\": \"keep.sh\" } ] }\n\t\t],\n\t\t\"SessionStart\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"start.sh\" } ] } ]\n\t},\n\t\"hooksConfig\": { \"notifications\": true }\n}\n",
    );
    t.write(
        "project/.gemini/settings.json",
        "{\"hooks\": {\"AfterTool\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"proj.sh\"}]}]}}\n",
    );
    t.write(&format!("{h}/GEMINI.md"), "# global\n");
    t.write("project/GEMINI.md", "# project\n@docs/g.md\n");
    t.write("project/docs/g.md", "g\n");
    t.write("project/sub/GEMINI.md", "# sub\n");
    t.write("project/sub/deep/GEMINI.md", "# deep\n");
    let ext = format!("{h}/extensions/e");
    t.write(
        &format!("{ext}/gemini-extension.json"),
        r#"{"name":"e","version":"1","contextFileName":"EXT.md"}"#,
    );
    t.write(&format!("{ext}/EXT.md"), "# ext\n");
    t.write(
        &format!("{ext}/hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"ext.sh"}]}]}}"#,
    );
    t.write(
        &format!("{h}/skills/gs/SKILL.md"),
        "---\nname: gs\ndescription: gemini skill\n---\nbody\n",
    );
    t.write(
        "project/.gemini/commands/c.toml",
        "description = \"c\"\nprompt = \"p\"\n",
    );
}

/// Zeroes clock readings so two runs compare.
fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if key == "mtimeMs" || key == "fingerprint" {
                    *inner = Value::Null;
                } else {
                    scrub(inner);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

fn plan_json(planner: &Planner, edit: &Edit) -> Value {
    match planner.plan(edit) {
        Ok(preview) => json!({"ok": preview}),
        Err(err) => json!({"err": err}),
    }
}

fn edits(snapshot: &Snapshot) -> Vec<Edit> {
    let mut out = Vec::new();
    for (path, entry) in &snapshot.files {
        if let FileState::Present { rev, .. } = &entry.state {
            let text = std::fs::read_to_string(path.as_path()).unwrap_or_default();
            out.push(Edit::Save {
                path: path.clone(),
                base: Base::Rev { rev: rev.clone() },
                text: format!("{text}\n"),
            });
            out.push(Edit::Trash {
                path: path.clone(),
                base: rev.clone(),
            });
        }
    }
    out.push(Edit::Save {
        path: snapshot.cwd.join("brand-new.md"),
        base: Base::Absent,
        text: "x\n".into(),
    });
    for view in &snapshot.agents {
        let draft = |event: &str| HookDraft {
            event: event.to_owned(),
            matcher: None,
            handler: json!({"type": "command", "command": "echo added", "x-new": true}),
        };
        for file in &view.hook_files {
            for event in ["Stop", "SessionStart", "NoSuchEvent"] {
                out.push(Edit::AddHook {
                    agent: view.agent.clone(),
                    file: file.file.clone(),
                    draft: draft(event),
                });
            }
        }
        for hook in &view.hooks {
            out.push(Edit::UpdateHook {
                hook: hook.id.clone(),
                draft: HookDraft {
                    event: hook.event.clone(),
                    matcher: hook.matcher.raw.clone(),
                    handler: json!({"type": "command", "command": "echo updated"}),
                },
            });
            out.push(Edit::RemoveHook {
                hook: hook.id.clone(),
            });
            for enabled in [false, true] {
                out.push(Edit::SetHookEnabled {
                    hook: hook.id.clone(),
                    enabled,
                });
            }
        }
        for slot in &view.slots {
            out.push(Edit::NewItem {
                agent: view.agent.clone(),
                slot: slot.id.clone(),
                name: Some("dump-new".into()),
            });
        }
        for store in &view.memory {
            out.push(Edit::AddMemory {
                agent: view.agent.clone(),
                store: store.id.clone(),
                title: "Dump note".into(),
                description: "d".into(),
                body: "body".into(),
            });
        }
    }
    for id in ["claude:nope", "codex:nope", "gemini:nope", "nope"] {
        out.push(Edit::RemoveHook {
            hook: HookId(id.to_owned()),
        });
        out.push(Edit::SetHookEnabled {
            hook: HookId(id.to_owned()),
            enabled: false,
        });
        out.push(Edit::UpdateHook {
            hook: HookId(id.to_owned()),
            draft: HookDraft {
                event: "Stop".into(),
                matcher: None,
                handler: json!({"type": "command", "command": "x"}),
            },
        });
    }
    out
}

fn dirs_below(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        if let Ok(read) = std::fs::read_dir(&out[i]) {
            let mut sub: Vec<_> = read
                .flatten()
                .filter(|e| e.path().is_dir() && e.file_name() != ".git")
                .map(|e| e.path())
                .collect();
            sub.sort();
            out.extend(sub);
        }
        i += 1;
    }
    out
}

fn dump_case(name: &str, build: &[fn(&Tree)], untrusted: bool) -> Value {
    let t = Tree::new(name);
    for f in build {
        f(&t);
    }
    if untrusted {
        t.write(
            "home/.gemini/trustedFolders.json",
            &json!({t.abs("project").as_str(): "DO_NOT_TRUST"}).to_string(),
        );
        t.write(
            "home/.gemini/settings.json",
            &json!({"security": {"folderTrust": {"enabled": true}}}).to_string(),
        );
    }
    let env = t.env();
    let parked = t.abs("data/parked");
    let scanned = scan_with_parked(registry(), &env, Some(&parked));
    let planner = Planner {
        agents: registry(),
        scanned: &scanned,
        env: &env,
        parked_dir: &parked,
        changes: &NoHistory,
    };
    let plans: Vec<Value> = edits(&scanned.snapshot)
        .iter()
        .map(|edit| json!({"edit": edit, "result": plan_json(&planner, edit)}))
        .collect();
    let cx = cm_core::agents::ScanCx {
        env: &env,
        fs: scanned.fs(),
        parked_dir: Some(&parked),
    };
    let sims: Vec<Value> = dirs_below(t.root.join("project").as_path())
        .into_iter()
        .chain(dirs_below(t.root.join("home/.gemini").as_path()))
        .map(|dir| {
            let target = AbsPath::parse(dir.join("probe.rs").to_str().unwrap()).unwrap();
            json!({"target": target, "sim": simulate(registry(), &cx, &scanned.snapshot, &target)})
        })
        .collect();
    let mut out = json!({"snapshot": scanned.snapshot, "plans": plans, "simulations": sims});
    scrub(&mut out);
    out
}

#[test]
#[ignore = "writes a dump for comparison; set CM_DUMP_OUT"]
fn dump() {
    let Ok(out_path) = std::env::var("CM_DUMP_OUT") else {
        eprintln!("CM_DUMP_OUT is not set: nothing dumped");
        return;
    };
    let cases = json!({
        "claude": dump_case("claude", &[claude], false),
        "codex": dump_case("codex", &[codex], false),
        "gemini": dump_case("gemini", &[gemini], false),
        "gemini_untrusted": dump_case("gemini_untrusted", &[gemini], true),
        "all": dump_case("all", &[claude, codex, gemini], false),
    });
    std::fs::write(out_path, serde_json::to_string_pretty(&cases).unwrap()).unwrap();
}

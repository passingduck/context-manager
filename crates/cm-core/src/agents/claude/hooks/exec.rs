//! `exec_spec`: the invocation Claude Code would use for a command hook (spec 3, "Handler
//! types and fields" and "Path placeholders and hook environment").

use serde_json::Value;

use crate::agents::ScanCx;
use crate::agents::claude::extensions::plugins;
use crate::agents::claude::setup::Setup;
use crate::model::*;

fn err(id: &str) -> RuleRef {
    RuleRef::new(id)
}

fn substitute(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = text.to_owned();
    for (name, value) in vars {
        out = out.replace(&format!("${{{name}}}"), value);
    }
    out
}

pub(super) fn exec_spec(hook: &Hook, input: &Value, cx: &ScanCx) -> Result<ExecSpec, RuleRef> {
    if hook.handler.handler_kind != "command" {
        return Err(err("claude.hook.run_unsupported"));
    }
    let raw = &hook.handler.raw;
    let command = raw
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| err("claude.hook.run_unsupported"))?;
    let args: Option<Vec<String>> = raw.get("args").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    });

    let project = cx.env.cwd.as_str().to_owned();
    // Spec 3: `${CLAUDE_PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_DATA}` resolve only in plugin hooks
    // (the root also in skill hooks). In settings.json the hook fails.
    let plugin = match &hook.source.scope {
        Scope::Plugin { id } => {
            let setup = Setup::load(cx);
            plugins::discover(cx, &setup)
                .into_iter()
                .find(|p| &p.id == id)
                .map(|p| {
                    (
                        p.root.as_str().to_owned(),
                        p.data_dir(&setup).as_str().to_owned(),
                    )
                })
        }
        _ => None,
    };
    let mentions =
        |name: &str| command.contains(name) || args.iter().flatten().any(|a| a.contains(name));
    if plugin.is_none() && (mentions("${CLAUDE_PLUGIN_ROOT}") || mentions("${CLAUDE_PLUGIN_DATA}"))
    {
        return Err(err("claude.hook.plugin_variable_outside_plugin"));
    }
    if mentions("${user_config.") && (plugin.is_none() || args.is_none()) {
        return Err(err("claude.hook.user_config_needs_exec_form"));
    }

    let mut vars: Vec<(&str, &str)> = vec![("CLAUDE_PROJECT_DIR", project.as_str())];
    let mut env = vec![
        ("CLAUDE_PROJECT_DIR".to_owned(), project.clone()),
        ("CLAUDECODE".to_owned(), "1".to_owned()),
    ];
    if let Some(id) = input.get("session_id").and_then(Value::as_str) {
        env.push(("CLAUDE_CODE_SESSION_ID".to_owned(), id.to_owned()));
    }
    if let Some((root, data)) = &plugin {
        vars.push(("CLAUDE_PLUGIN_ROOT", root));
        vars.push(("CLAUDE_PLUGIN_DATA", data));
        env.push(("CLAUDE_PLUGIN_ROOT".to_owned(), root.clone()));
        env.push(("CLAUDE_PLUGIN_DATA".to_owned(), data.clone()));
    }

    // Exec form when `args` is present: no shell. Shell form is `sh -c` (spec 3, H5 UNVERIFIED
    // whether `shell: "bash"` uses `$SHELL`).
    let argv = match &args {
        Some(args) => std::iter::once(substitute(command, &vars))
            .chain(args.iter().map(|a| substitute(a, &vars)))
            .collect(),
        None => vec!["sh".to_owned(), "-c".to_owned(), substitute(command, &vars)],
    };
    Ok(ExecSpec {
        argv,
        cwd: cx.env.cwd.clone(),
        env,
        stdin: serde_json::to_string(input).unwrap_or_default(),
    })
}

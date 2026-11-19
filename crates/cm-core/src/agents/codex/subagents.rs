//! Subagents, the custom agent roles (spec 5.3).

use super::out::{NewLoad, Out, delivered, discovery};
use super::setup::{Layer, LayerKind, Setup, get_path, read_lossy};
use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

pub(super) const RULE_FILE: &str = "codex.agents.file";
pub(super) const RULE_ENTRY: &str = "codex.agents.config_entry";
pub(super) const RULE_REJECTED: &str = "codex.agents.rejected";
pub(super) const RULE_UNTRUSTED: &str = "codex.agents.project_untrusted";

struct Role {
    name: String,
    description: Option<String>,
    file: AbsPath,
    scope: Scope,
    rule: &'static str,
    text: Option<String>,
    prompt: Option<String>,
    reject: Option<String>,
    untrusted: bool,
}

fn ext_id(file: &AbsPath, name: &str) -> ExtId {
    ExtId(sha256_hex(
        format!("codex\0subagent\0{file}\0{name}").as_bytes(),
    ))
}

/// Recursive `.toml` files, sorted (spec 5.3).
fn toml_files(cx: &ScanCx, dir: &AbsPath, files: &mut Vec<AbsPath>, depth: usize) {
    let Ok(entries) = cx.fs.list(dir) else { return };
    for entry in entries.iter() {
        let path = dir.join(&entry.name);
        match entry.kind {
            EntryKind::File if entry.name.ends_with(".toml") => files.push(path),
            EntryKind::Dir if depth < 8 => toml_files(cx, &path, files, depth + 1),
            _ => {}
        }
    }
}

fn parse_role(
    cx: &ScanCx,
    file: &AbsPath,
    scope: Scope,
    rule: &'static str,
    untrusted: bool,
) -> Option<Role> {
    let (_, text) = read_lossy(cx, file)?;
    let fallback = file
        .file_name()
        .map(|n| n.trim_end_matches(".toml").to_owned())
        .unwrap_or_default();
    match toml::from_str::<toml::Table>(&text) {
        Ok(table) => {
            let name = table
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .unwrap_or(fallback);
            let description = table
                .get("description")
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            let prompt = table
                .get("developer_instructions")
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            Some(Role {
                reject: description
                    .is_none()
                    .then(|| "missing field `description`".to_owned()),
                name,
                description,
                file: file.clone(),
                scope,
                rule,
                text: Some(text),
                prompt,
                untrusted,
            })
        }
        Err(err) => Some(Role {
            name: fallback,
            description: None,
            file: file.clone(),
            scope,
            rule,
            text: None,
            prompt: None,
            reject: Some(err.message().to_owned()),
            untrusted,
        }),
    }
}

fn layer_roles(cx: &ScanCx, setup: &Setup, layer: &Layer, claimed: &mut Vec<AbsPath>) -> Vec<Role> {
    let mut roles = Vec::new();
    let untrusted = !layer.enabled && matches!(layer.kind, LayerKind::Project { .. });
    // `[agents.<name>]` entries first, so their files are not discovered a second time.
    let mut entries = Vec::new();
    if let Some(table) = layer
        .config
        .as_ref()
        .and_then(|c| get_path(c, &["agents"]))
        .and_then(toml::Value::as_table)
    {
        for (name, entry) in table {
            let Some(entry) = entry.as_table() else {
                continue;
            };
            let file = entry
                .get("config_file")
                .and_then(toml::Value::as_str)
                .map(|f| setup.path_in_layer(layer, f));
            let description = entry
                .get("description")
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            entries.push((name.clone(), file, description));
        }
    }
    for (name, file, description) in entries {
        match file {
            Some(file) => {
                claimed.push(file.clone());
                if let Some(mut role) = parse_role(cx, &file, layer.scope(), RULE_ENTRY, untrusted)
                {
                    role.name = name;
                    if role.description.is_none() {
                        role.description = description;
                        role.reject = role
                            .description
                            .is_none()
                            .then(|| "missing field `description`".to_owned());
                    }
                    roles.push(role);
                } else {
                    roles.push(Role {
                        name,
                        description,
                        file,
                        scope: layer.scope(),
                        rule: RULE_ENTRY,
                        text: None,
                        prompt: None,
                        reject: Some("config_file is missing".to_owned()),
                        untrusted,
                    });
                }
            }
            None => roles.push(Role {
                name,
                reject: description
                    .is_none()
                    .then(|| "missing field `description`".to_owned()),
                description,
                file: layer.config_path.clone(),
                scope: layer.scope(),
                rule: RULE_ENTRY,
                text: None,
                prompt: None,
                untrusted,
            }),
        }
    }
    let dir = layer.folder.join("agents");
    let mut files = Vec::new();
    toml_files(cx, &dir, &mut files, 0);
    for file in files {
        if claimed.contains(&file) {
            continue;
        }
        if let Some(role) = parse_role(cx, &file, layer.scope(), RULE_FILE, untrusted) {
            roles.push(role);
        }
    }
    roles
}

pub(super) fn run(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let mut claimed = Vec::new();
    // Low to high precedence, so a later layer shadows an earlier role of the same name.
    let mut all: Vec<Role> = Vec::new();
    for layer in &setup.layers {
        if layer.kind == LayerKind::LegacyManaged {
            continue;
        }
        out.claim(&layer.folder.join("agents"), EditPolicy::Editable);
        all.extend(layer_roles(cx, setup, layer, &mut claimed));
    }
    let ids: Vec<ExtId> = all.iter().map(|r| ext_id(&r.file, &r.name)).collect();
    let mut shadowed: Vec<Option<ExtId>> = vec![None; all.len()];
    for (i, role) in all.iter().enumerate() {
        if role.untrusted || role.reject.is_some() {
            continue;
        }
        if let Some(later) = (i + 1..all.len())
            .find(|j| all[*j].name == role.name && !all[*j].untrusted && all[*j].reject.is_none())
        {
            shadowed[i] = Some(ids[later].clone());
        }
    }
    for (i, role) in all.into_iter().enumerate() {
        let id = ids[i].clone();
        let (status, load_status, deliv) = if role.untrusted {
            (
                ExtStatus::Disabled {
                    by: RULE_UNTRUSTED.to_owned(),
                },
                LoadStatus::Skipped {
                    skip: Skip::Untrusted,
                },
                None,
            )
        } else if let Some(detail) = &role.reject {
            out.diagnose(
                Severity::Warning,
                Subject::Extension { ext: id.clone() },
                Problem::ExtensionRejected {
                    why: RuleRef::new(RULE_REJECTED),
                },
            );
            (
                ExtStatus::Rejected {
                    why: RuleRef::new(RULE_REJECTED),
                },
                LoadStatus::Skipped {
                    skip: Skip::Invalid {
                        detail: detail.clone(),
                    },
                },
                None,
            )
        } else if let Some(by) = shadowed[i].clone() {
            (
                ExtStatus::Shadowed { by },
                LoadStatus::Skipped {
                    skip: Skip::Shadowed {
                        by: role.file.clone(),
                        how: Shadow::NameCollision,
                    },
                },
                None,
            )
        } else {
            let prompt = role.prompt.clone().unwrap_or_default();
            let deliv = role
                .text
                .as_ref()
                .map(|_| delivered(&prompt, prompt.len() as u64));
            (ExtStatus::Active, LoadStatus::Loaded, deliv)
        };
        let body = out.push_load(NewLoad {
            file: role.file.clone(),
            kind: DocKind::SubagentPrompt,
            scope: role.scope.clone(),
            via: discovery(role.rule),
            when: Activation::OnInvoke,
            status: load_status,
            delivered: deliv,
        });
        out.claim(&role.file, EditPolicy::Editable);
        out.extensions.push(Extension {
            id,
            ext_kind: ExtKind::Subagent,
            name: role.name,
            description: role.description,
            file: role.file,
            scope: role.scope,
            listing: None,
            body,
            status,
        });
    }
}

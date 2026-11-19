//! The passes that run after the adapters (design section 5): the reference graph and the
//! budget. Neither repeats a rule of an agent. They read the loads an adapter returned.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::agents::ScanCx;
use crate::md::refs::{self, RefNode};
use crate::model::*;
use crate::probe_fs::{MAX_READ_BYTES, ProbeError, ProbeErrorKind};
use crate::scan::format_of;

/// At most this many reference nodes per scan, over all agents.
pub const MAX_REFERENCE_NODES: usize = refs::MAX_NODES;

/// Adds a `Via::Reference` load for every markdown file that a load whose text reaches the
/// model names by link or path, to depth 3. A file the agent already loads is not added again.
/// Returns a `WalkCapped` diagnostic when the node cap stopped the graph.
pub fn reference_graph(
    cx: &ScanCx,
    project_root: &AbsPath,
    views: &mut [AgentView],
) -> Vec<Diagnostic> {
    let mut total = 0usize;
    let mut capped = false;
    for view in views.iter_mut() {
        let loaded: HashSet<AbsPath> = view.loads.iter().map(|load| load.file.clone()).collect();
        let mut added: HashMap<AbsPath, LoadId> = HashMap::new();
        let roots = view.loads.len();
        for index in 0..roots {
            let root = view.loads[index].clone();
            if !reaches_model(&root) {
                continue;
            }
            let Ok(bytes) = cx.fs.read(&root.file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let claimed = import_lines(&view.loads[..roots], &root.id, &text);
            let base = if root.file.starts_with(project_root) {
                project_root.clone()
            } else {
                root.file.parent().unwrap_or_else(AbsPath::root)
            };
            let tree = refs::follow(&root.file, &text, &claimed, &base, cx.fs);
            capped |= tree.capped;
            let mut graph = Graph {
                cx,
                loaded: &loaded,
                added: &mut added,
                total: &mut total,
                capped: &mut capped,
                scope: root.scope.clone(),
            };
            graph.add(view, &tree.nodes, &root.id);
        }
    }
    if !capped {
        return Vec::new();
    }
    vec![Diagnostic {
        severity: Severity::Info,
        agent: None,
        at: Subject::Agent,
        problem: Problem::WalkCapped {
            entries: MAX_REFERENCE_NODES as u32,
        },
        fix: None,
    }]
}

/// A load whose file text is in the model's context or may be: delivered, markdown, and not a
/// generated catalog line.
fn reaches_model(load: &Load) -> bool {
    load.delivered.is_some()
        && !matches!(load.via, Via::Listing { .. })
        && format_of(&load.file) == Format::Markdown
}

/// The byte ranges of the lines on which an adapter found an import of `host`. A link on such a
/// line belongs to the adapter.
fn import_lines(loads: &[Load], host: &LoadId, text: &str) -> Vec<Range<usize>> {
    let mut lines: Vec<u32> = loads
        .iter()
        .filter_map(|load| match &load.via {
            Via::Import { host: h, line, .. } if h == host => Some(*line),
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    lines.sort_unstable();
    lines.dedup();
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if lines.contains(&(index as u32 + 1)) {
            ranges.push(start..start + line.len());
        }
        start += line.len();
    }
    ranges
}

struct Graph<'a, 'cx> {
    cx: &'a ScanCx<'cx>,
    loaded: &'a HashSet<AbsPath>,
    added: &'a mut HashMap<AbsPath, LoadId>,
    total: &'a mut usize,
    capped: &'a mut bool,
    scope: Scope,
}

impl Graph<'_, '_> {
    fn add(&mut self, view: &mut AgentView, nodes: &[RefNode], host: &LoadId) {
        for node in nodes {
            // The file is a load of its own, and its own references are followed from it.
            if self.loaded.contains(&node.path) {
                continue;
            }
            let id = match self.added.get(&node.path) {
                Some(id) => id.clone(),
                None => {
                    if *self.total >= MAX_REFERENCE_NODES {
                        *self.capped = true;
                        return;
                    }
                    *self.total += 1;
                    let id = LoadId(view.loads.len() as u32);
                    view.loads.push(self.load(id.clone(), node, host));
                    self.added.insert(node.path.clone(), id.clone());
                    id
                }
            };
            self.add(view, &node.children, &id);
        }
    }

    fn load(&self, id: LoadId, node: &RefNode, host: &LoadId) -> Load {
        let (status, delivered) = match self.cx.fs.read(&node.path) {
            Ok(bytes) => (
                LoadStatus::Loaded,
                Some(Delivered {
                    bytes: bytes.len() as u64,
                    tokens: crate::tokens::count_cached(&Revision::of(&bytes), &bytes),
                    transformed: false,
                }),
            ),
            Err(ProbeError {
                kind: ProbeErrorKind::TooLarge { .. },
                ..
            }) => (
                LoadStatus::Skipped {
                    skip: Skip::TooLarge {
                        limit: Limit {
                            kind: LimitKind::MaxFileBytes,
                            value: MAX_READ_BYTES,
                            unit: Unit::Bytes,
                        },
                    },
                },
                None,
            ),
            Err(err) => (
                LoadStatus::Skipped {
                    skip: Skip::Invalid { detail: err.detail },
                },
                None,
            ),
        };
        Load {
            id,
            file: node.path.clone(),
            kind: DocKind::Reference,
            scope: self.scope.clone(),
            via: Via::Reference {
                host: host.clone(),
                line: node.line,
                depth: node.depth as u8,
            },
            when: Activation::ModelMayRead,
            status,
            delivered,
        }
    }
}

/// `session_start` is the sum of the blocks, `by_label` groups them, and `on_access_max` is the
/// text of every load that arrives on an access, with the imports it brings.
pub fn budget(view: &mut AgentView) {
    let mut by_label: Vec<(BlockLabel, u32)> = Vec::new();
    for block in &view.session {
        match by_label.iter_mut().find(|(label, _)| *label == block.label) {
            Some((_, tokens)) => *tokens += block.tokens,
            None => by_label.push((block.label, block.tokens)),
        }
    }
    let on_access_max = view
        .loads
        .iter()
        .filter(|load| is_on_access(&view.loads, load, 0))
        .filter_map(|load| load.delivered.as_ref())
        .map(|delivered| delivered.tokens)
        .sum();
    view.budget = Budget {
        session_start: view.session.iter().map(|block| block.tokens).sum(),
        by_label,
        on_access_max,
        method: view.budget.method,
    };
}

/// Every agent sends its built-in prompt and tool definitions before the first session block
/// (design section 3, `ContextBlock.preceded_by`). The gap goes at the front of the list, and
/// a gap that an adapter already named on that block stays behind it.
pub fn unsized_gaps(view: &mut AgentView) {
    if let Some(first) = view.session.first_mut()
        && !first.preceded_by.contains(&Unsized::BuiltIn)
    {
        first.preceded_by.insert(0, Unsized::BuiltIn);
    }
}

fn is_on_access(loads: &[Load], load: &Load, depth: usize) -> bool {
    match (&load.when, &load.via) {
        (Activation::OnAccess { .. }, _) => true,
        // An import is active exactly when its host is. The depth bound stops a cycle.
        (Activation::WithHost, Via::Import { host, .. }) if depth < 16 => loads
            .get(host.0 as usize)
            .is_some_and(|host| is_on_access(loads, host, depth + 1)),
        _ => false,
    }
}

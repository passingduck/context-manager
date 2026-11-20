//! What the model receives from each load of one file: the `load_texts` operation.
//!
//! A session-start load's text is its own piece of the block `render` returns, so the two cannot
//! disagree. Any other delivered load asks its adapter (`Agent::load_text`). A skipped load sends
//! nothing.

use std::collections::BTreeMap;

use crate::agents::{Agent, ScanCx};
use crate::model::*;

fn delivered(load: &Load) -> bool {
    matches!(
        load.status,
        LoadStatus::Loaded | LoadStatus::Truncated { .. }
    )
}

/// One entry per load of `file` in `view`, in load order.
pub fn load_texts(
    agent: &dyn Agent,
    cx: &ScanCx,
    view: &AgentView,
    file: &AbsPath,
) -> Vec<LoadText> {
    let mine: Vec<&Load> = view.loads.iter().filter(|l| &l.file == file).collect();
    if mine.is_empty() {
        // Nothing is read for a path the agent does not load.
        return Vec::new();
    }
    let mut pieces: BTreeMap<LoadId, String> = BTreeMap::new();
    if mine.iter().any(|l| delivered(l)) {
        for block in agent.render(cx, view) {
            for piece in block.pieces {
                if let Piece::Load { load, text } = piece
                    && mine.iter().any(|l| l.id == load)
                {
                    pieces.insert(load, text);
                }
            }
        }
    }
    let disk = cx
        .fs
        .read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    mine.into_iter()
        .map(|load| {
            let text = if delivered(load) {
                pieces
                    .remove(&load.id)
                    .or_else(|| agent.load_text(cx, view, load))
            } else {
                None
            };
            let whole_file = matches!((&text, &disk), (Some(t), Some(d)) if t.trim() == d.trim());
            LoadText {
                load: load.id.clone(),
                text,
                whole_file,
            }
        })
        .collect()
}

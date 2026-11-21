//! Checks every trace test makes on every record it reads (the brief's invariants): the pieces
//! of each block joined give the block's text, `shared` never passes the previous request,
//! `by_kind` adds up to the pieces, and no identity value reaches the JSON the client receives.

use std::collections::BTreeMap;

use cm_core::trace::OpenTrace;
use cm_core::{PiecePlace, PieceRef, PieceText, StepPieces, TracePiece};
use serde_json::Value;

/// What a request held, from a source other than the reader: one entry per message, one per
/// content block.
#[derive(Debug, Clone)]
pub struct Sent {
    pub at: String,
    pub system: Vec<String>,
    pub messages: Vec<Vec<Expect>>,
}

#[derive(Debug, Clone)]
pub enum Expect {
    /// The block's text: a text block, a thinking block, a tool result.
    Text(String),
    /// A tool call: its piece holds the input as JSON.
    Input(Value),
    /// A block without text, such as an image.
    Other,
}

/// The text of one piece of a step's request.
pub fn text(open: &OpenTrace, step: u32, piece: u32) -> String {
    match open.piece_text(step, PieceRef::Sent { piece }) {
        Some(PieceText::Current { text, cut, .. }) => {
            assert!(!cut, "a test record holds no piece over the cut");
            text
        }
        other => panic!("piece {piece} of step {step} has no text: {other:?}"),
    }
}

/// Runs every check. `sent` lists the requests the record holds, found by their time; a step
/// whose time is not in it is checked for everything but its texts. `secrets` are the values of
/// the identity fields the record holds. Returns how many steps had their texts checked.
pub fn check(open: &OpenTrace, sent: &[Sent], secrets: &[String]) -> usize {
    let trace = open.trace();
    let json = serde_json::to_string(trace).unwrap();
    for secret in secrets {
        assert!(
            !json.contains(secret.as_str()),
            "an identity value reached the trace"
        );
    }
    let mut previous: Option<u32> = None;
    let mut texts = 0;
    for (i, step) in trace.steps.iter().enumerate() {
        let i = i as u32;
        let pieces = open.step_pieces(i).expect("every step answers");
        let json = serde_json::to_string(&StepPieces::Current {
            pieces: pieces.clone(),
        })
        .unwrap();
        for secret in secrets {
            assert!(
                !json.contains(secret.as_str()),
                "an identity value reached a step"
            );
        }
        let Some(request) = &step.sent else {
            assert!(pieces.is_empty());
            previous = None;
            continue;
        };
        assert_eq!(request.pieces as usize, pieces.len(), "step {i}");
        match previous {
            Some(before) => assert!(
                request.shared <= before,
                "step {i}: shared {} > {before}",
                request.shared
            ),
            None => assert_eq!(request.shared, 0, "step {i}"),
        }
        previous = Some(request.pieces);
        let tokens: u64 = pieces.iter().map(|p| u64::from(p.tokens)).sum();
        let by_kind: u64 = request
            .by_kind
            .iter()
            .map(|k| u64::from(k.shared) + u64::from(k.fresh))
            .sum();
        assert_eq!(by_kind, tokens, "step {i}: by_kind adds up to the pieces");
        if let Some(expected) = sent.iter().find(|s| s.at == step.at) {
            check_texts(open, i, &pieces, expected);
            texts += 1;
        }
    }
    texts
}

fn check_texts(open: &OpenTrace, step: u32, pieces: &[TracePiece], expected: &Sent) {
    let mut system: BTreeMap<u32, String> = BTreeMap::new();
    let mut blocks: BTreeMap<(u32, u32), (String, Vec<TracePiece>)> = BTreeMap::new();
    for (n, piece) in pieces.iter().enumerate() {
        let text = text(open, step, n as u32);
        match piece.place {
            PiecePlace::System { block } => system.entry(block).or_default().push_str(&text),
            PiecePlace::Message { message, block, .. } => {
                let entry = blocks.entry((message, block)).or_default();
                entry.0.push_str(&text);
                entry.1.push(piece.clone());
            }
            PiecePlace::Tool { .. } => {}
        }
    }
    let system: Vec<String> = system.into_values().collect();
    assert_eq!(system, expected.system, "step {step}: system blocks");
    let count: usize = expected.messages.iter().map(Vec::len).sum();
    assert_eq!(blocks.len(), count, "step {step}: content blocks");
    for (m, message) in expected.messages.iter().enumerate() {
        for (b, want) in message.iter().enumerate() {
            let (joined, parts) = &blocks[&(m as u32, b as u32)];
            match want {
                Expect::Text(text) => {
                    assert_eq!(joined, text, "step {step}, message {m}, block {b}")
                }
                Expect::Input(input) => {
                    let got: Value = serde_json::from_str(joined).expect("a tool call holds JSON");
                    assert_eq!(&got, input, "step {step}, message {m}, block {b}");
                }
                Expect::Other => assert!(joined.is_empty(), "step {step}, message {m}, block {b}"),
            }
            assert!(!parts.is_empty());
        }
    }
}

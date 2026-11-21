//! Fitted piece sizes (design section 14, "Real and estimated numbers stay apart").
//!
//! The o200k count of a piece is far from what the model's tokenizer counts, and a thinking
//! block whose text the record leaves out counts almost nothing. The usage the API reported pins
//! most of it down. A step's prompt size is `input + cache_read + cache_write`. When its request
//! extends an earlier request whose sizes add up to that request's prompt size, the pieces it
//! adds cost the difference of the two prompt sizes, and the earlier reply's usage splits that
//! difference further: its thinking pieces cost its thinking tokens, its other pieces cost the
//! rest of its output, and the new pieces cost what is left. Otherwise one factor fits every
//! fresh piece to the prompt size minus what the shared pieces cost. When even that is
//! impossible, the fresh pieces keep their raw counts.

use std::collections::VecDeque;

use super::{At, same};
use crate::model::{PieceKind, PiecePlace, Role, Usage};
use crate::trace::TraceData;

/// How many earlier steps with a request a request is compared with to find the one it extends.
/// A main thread interleaves a few side requests (a title, a summary) with its own chain.
const WINDOW: usize = 8;

/// How a step's sizes were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The step has no request.
    NoRequest,
    /// The added pieces were split by the earlier reply's usage.
    Split,
    /// One factor for every fresh piece.
    Factor(Coarse),
    /// The fresh pieces keep their o200k counts.
    Raw(Unfitted),
}

/// Why a step that has usage was fitted by one factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coarse {
    /// The request extends no earlier fitted request in the window: the first request of a
    /// chain, a request after a compaction, or one that changed its own past.
    NoExtension,
    /// The added messages of the model do not hold a piece of each kind as often as the earlier
    /// reply does.
    ReplyNotWhole,
    /// The earlier reply has thinking pieces and its usage has no thinking count.
    ThinkingUnknown,
    /// A share came out negative, or had no piece to go to.
    DoesNotClose,
}

/// Why a step's fresh pieces keep their raw counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfitted {
    NoUsage,
    /// The shared pieces already cost more than the prompt size.
    SharedTooLarge,
    /// A remainder with no fresh piece of a nonzero count to take it.
    NothingToFit,
}

impl Fit {
    /// The step's sizes add up to its prompt size.
    pub fn fitted(self) -> bool {
        matches!(self, Fit::Split | Fit::Factor(_))
    }
}

/// The sizes of one step's request, piece by piece, and how they were found.
#[derive(Debug, Clone, PartialEq)]
pub struct StepFit {
    pub fit: Fit,
    pub sizes: Vec<u32>,
    /// When the request extends an earlier fitted one, what it added to it.
    pub added: Option<Added>,
}

/// The pieces a request added to the earlier request it extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Added {
    /// The step of the earlier request.
    pub to: u32,
    /// The pieces before the added ones: the earlier request's length.
    pub after: u32,
    /// The difference of the two prompt sizes: the real size of the added pieces.
    pub real: u64,
    /// The sum of their o200k counts.
    pub raw: u64,
}

/// The prompt size the API reported.
pub fn prompt(usage: &Usage) -> u64 {
    usage.input + usage.cache_read + usage.cache_write
}

/// Fits the steps in order. Each step needs the fits of the steps before it.
pub(super) struct Fitter<'a> {
    data: &'a TraceData,
    tokens: &'a [Vec<u32>],
    /// The last steps with a request, oldest first, with their pieces.
    recent: VecDeque<(usize, Vec<At>)>,
    pub fits: Vec<StepFit>,
}

impl<'a> Fitter<'a> {
    pub fn new(data: &'a TraceData, tokens: &'a [Vec<u32>]) -> Fitter<'a> {
        Fitter {
            data,
            tokens,
            recent: VecDeque::with_capacity(WINDOW),
            fits: Vec::with_capacity(data.steps.len()),
        }
    }

    /// Fits the next step, whose request has the pieces `list`, or none.
    pub fn push(&mut self, list: Option<Vec<At>>) {
        let index = self.fits.len();
        let Some(list) = list else {
            self.fits.push(StepFit {
                fit: Fit::NoRequest,
                sizes: Vec::new(),
                added: None,
            });
            return;
        };
        let fit = self.fit(index, &list);
        self.fits.push(fit);
        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back((index, list));
    }

    fn raw(&self, at: &At) -> u32 {
        self.tokens[at.block as usize][at.span as usize]
    }

    fn fit(&self, index: usize, list: &[At]) -> StepFit {
        let raw: Vec<u32> = list.iter().map(|at| self.raw(at)).collect();
        let (extends, reference, common) = self.reference(list);
        let mut sizes: Vec<u32> = Vec::with_capacity(list.len());
        if let Some(j) = reference {
            sizes.extend_from_slice(&self.fits[j].sizes[..common]);
        }
        let fresh = &list[common..];
        let fresh_raw = &raw[common..];
        let keep_raw = |mut sizes: Vec<u32>, why| {
            sizes.extend_from_slice(fresh_raw);
            StepFit {
                fit: Fit::Raw(why),
                sizes,
                added: None,
            }
        };
        let Some(usage) = &self.data.steps[index].usage else {
            return keep_raw(sizes, Unfitted::NoUsage);
        };
        let carried: u64 = sizes.iter().map(|&n| u64::from(n)).sum();
        let Some(remainder) = prompt(usage).checked_sub(carried) else {
            return keep_raw(sizes, Unfitted::SharedTooLarge);
        };
        let added = extends.map(|j| Added {
            to: j as u32,
            after: common as u32,
            real: remainder,
            raw: fresh_raw.iter().map(|&n| u64::from(n)).sum(),
        });
        let coarse = match extends {
            Some(j) => match self.split(j, fresh, fresh_raw, remainder) {
                Ok(split) => {
                    sizes.extend(split);
                    return StepFit {
                        fit: Fit::Split,
                        sizes,
                        added,
                    };
                }
                Err(why) => why,
            },
            None => Coarse::NoExtension,
        };
        let weights: Vec<u64> = fresh_raw.iter().map(|&n| u64::from(n)).collect();
        match share(remainder, &weights) {
            Some(factored) => {
                sizes.extend(factored);
                StepFit {
                    fit: Fit::Factor(coarse),
                    sizes,
                    added,
                }
            }
            None => keep_raw(sizes, Unfitted::NothingToFit),
        }
    }

    /// The step whose sizes this request starts from, and how many leading pieces it shares
    /// with it. The latest fitted step whose whole request this one extends comes first (it is
    /// also returned as the first value), else the step that shares the most leading pieces.
    fn reference(&self, list: &[At]) -> (Option<usize>, Option<usize>, usize) {
        let mut best: Option<(usize, usize)> = None;
        for (j, before) in self.recent.iter().rev() {
            let common = before
                .iter()
                .zip(list)
                .take_while(|(a, b)| same(self.data, a, b))
                .count();
            if common == before.len() && self.fits[*j].fit.fitted() {
                return (Some(*j), Some(*j), common);
            }
            if best.is_none_or(|(_, c)| common > c) {
                best = Some((*j, common));
            }
        }
        match best {
            Some((j, common)) => (None, Some(j), common),
            None => (None, None, 0),
        }
    }

    /// Splits `remainder`, the real size of the pieces this request added to step `j`'s
    /// request, by the usage of step `j`'s reply.
    fn split(
        &self,
        j: usize,
        fresh: &[At],
        fresh_raw: &[u32],
        remainder: u64,
    ) -> Result<Vec<u32>, Coarse> {
        let before = &self.data.steps[j];
        let usage = before.usage.as_ref().expect("a fitted step has usage");
        // The request sends the reply back as a message of the model. The agent may have
        // rewritten it on the way (a tool call's input), so it is matched by the kinds of its
        // pieces, not by their text.
        let kind = |at: &At| self.data.blocks[at.block as usize].spans[at.span as usize].kind;
        let mut reply: Vec<PieceKind> = before
            .reply
            .iter()
            .flat_map(|&block| {
                self.data.blocks[block as usize]
                    .spans
                    .iter()
                    .map(|s| s.kind)
            })
            .collect();
        #[derive(Clone, Copy, PartialEq)]
        enum Part {
            Thinking,
            Reply,
            New,
        }
        let mut sent: Vec<PieceKind> = Vec::new();
        let parts: Vec<Part> = fresh
            .iter()
            .map(|at| {
                let from_model = matches!(
                    at.place,
                    PiecePlace::Message {
                        role: Role::Model,
                        ..
                    }
                );
                if !from_model {
                    return Part::New;
                }
                sent.push(kind(at));
                match kind(at) {
                    PieceKind::Thinking => Part::Thinking,
                    _ => Part::Reply,
                }
            })
            .collect();
        reply.sort_unstable();
        sent.sort_unstable();
        if sent != reply {
            return Err(Coarse::ReplyNotWhole);
        }
        let has = |part: Part| parts.contains(&part);
        let thinking = match (has(Part::Thinking), usage.thinking) {
            (false, _) => 0,
            (true, Some(thinking)) => thinking,
            (true, None) => return Err(Coarse::ThinkingUnknown),
        };
        let other = if has(Part::Reply) {
            usage
                .output
                .checked_sub(usage.thinking.unwrap_or(0))
                .ok_or(Coarse::DoesNotClose)?
        } else {
            0
        };
        let new = remainder
            .checked_sub(thinking + other)
            .ok_or(Coarse::DoesNotClose)?;

        let weights = |part: Part| -> Vec<u64> {
            let chosen = fresh.iter().zip(fresh_raw).zip(&parts);
            let chosen = chosen.filter(|(_, p)| **p == part).map(|(pair, _)| pair);
            match part {
                // By what the record holds for each: the text, else the signature.
                Part::Thinking => chosen
                    .map(|(at, _)| {
                        let span = &self.data.blocks[at.block as usize].spans[at.span as usize];
                        match span.text.len() {
                            0 => span.opaque,
                            len => len as u64,
                        }
                    })
                    .collect(),
                Part::Reply | Part::New => chosen.map(|(_, &raw)| u64::from(raw)).collect(),
            }
        };
        let mut thinking_weights = weights(Part::Thinking);
        if thinking_weights.iter().all(|&w| w == 0) {
            thinking_weights.iter_mut().for_each(|w| *w = 1);
        }
        let mut shares = [
            share(thinking, &thinking_weights),
            share(other, &weights(Part::Reply)),
            share(new, &weights(Part::New)),
        ]
        .map(|sizes| sizes.map(Vec::into_iter));
        let [Some(t), Some(r), Some(n)] = &mut shares else {
            return Err(Coarse::DoesNotClose);
        };
        Ok(parts
            .iter()
            .map(|part| {
                let next = match part {
                    Part::Thinking => t.next(),
                    Part::Reply => r.next(),
                    Part::New => n.next(),
                };
                next.expect("one share per piece")
            })
            .collect())
    }
}

/// A reply's own pieces, fitted to the usage of that reply: its thinking pieces add up to the
/// thinking tokens, and its other pieces to the rest of the output. Each piece is its kind, the
/// weight of a thinking piece (its text, else its signature) and its o200k count. The o200k
/// counts stay when the usage is missing or a share does not close.
pub(super) fn reply(usage: Option<&Usage>, pieces: &[(PieceKind, u64, u32)]) -> Vec<u32> {
    let raw = || pieces.iter().map(|&(_, _, raw)| raw).collect::<Vec<u32>>();
    let Some(usage) = usage else {
        return raw();
    };
    let thinks = |kind: PieceKind| kind == PieceKind::Thinking;
    let has_thinking = pieces.iter().any(|&(kind, _, _)| thinks(kind));
    let thinking = match (has_thinking, usage.thinking) {
        (false, _) => 0,
        (true, Some(thinking)) => thinking,
        (true, None) => return raw(),
    };
    let Some(other) = usage.output.checked_sub(usage.thinking.unwrap_or(0)) else {
        return raw();
    };
    let mut thinking_weights: Vec<u64> = pieces
        .iter()
        .filter(|piece| thinks(piece.0))
        .map(|piece| piece.1)
        .collect();
    if thinking_weights.iter().all(|&w| w == 0) {
        thinking_weights.iter_mut().for_each(|w| *w = 1);
    }
    let other_weights: Vec<u64> = pieces
        .iter()
        .filter(|piece| !thinks(piece.0))
        .map(|piece| u64::from(piece.2))
        .collect();
    let (Some(thinking), Some(other)) = (
        share(thinking, &thinking_weights),
        share(other, &other_weights),
    ) else {
        return raw();
    };
    let (mut thinking, mut other) = (thinking.into_iter(), other.into_iter());
    pieces
        .iter()
        .map(|&(kind, _, _)| {
            let next = if thinks(kind) {
                thinking.next()
            } else {
                other.next()
            };
            next.expect("one share per piece")
        })
        .collect()
}

/// `total` shared by `weights`, in whole tokens that add up to `total` (largest remainder).
/// `None` when `total` is not zero and no weight is.
fn share(total: u64, weights: &[u64]) -> Option<Vec<u32>> {
    let sum: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    if sum == 0 {
        return (total == 0).then(|| vec![0; weights.len()]);
    }
    let total = u128::from(total);
    let mut sizes: Vec<u128> = weights
        .iter()
        .map(|&w| total * u128::from(w) / sum)
        .collect();
    let mut left = total - sizes.iter().sum::<u128>();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(total * u128::from(weights[i]) % sum));
    for i in order {
        if left == 0 {
            break;
        }
        sizes[i] += 1;
        left -= 1;
    }
    Some(
        sizes
            .into_iter()
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{reply, share};
    use crate::model::{PieceKind, Usage};

    fn usage(output: u64, thinking: Option<u64>) -> Usage {
        Usage {
            input: 1,
            cache_read: 0,
            cache_write: 0,
            output,
            thinking,
        }
    }

    /// The thinking pieces take the thinking tokens by signature size, and the rest of the
    /// output goes to the other pieces by their o200k counts.
    #[test]
    fn a_reply_is_fitted_to_its_own_usage() {
        let pieces = [
            (PieceKind::Thinking, 3_000, 0),
            (PieceKind::Thinking, 1_000, 0),
            (PieceKind::ModelText, 0, 10),
            (PieceKind::ToolCall, 0, 30),
        ];
        assert_eq!(
            reply(Some(&usage(500, Some(300))), &pieces),
            [225, 75, 50, 150]
        );
    }

    #[test]
    fn a_reply_keeps_its_o200k_counts_when_its_usage_cannot_place_them() {
        let pieces = [
            (PieceKind::Thinking, 3_000, 2),
            (PieceKind::ModelText, 0, 10),
        ];
        assert_eq!(reply(None, &pieces), [2, 10], "no usage");
        assert_eq!(
            reply(Some(&usage(500, None)), &pieces),
            [2, 10],
            "thinking pieces and no thinking count"
        );
        assert_eq!(
            reply(Some(&usage(100, Some(300))), &pieces),
            [2, 10],
            "more thinking than output"
        );
        assert_eq!(
            reply(Some(&usage(500, Some(300))), &[(PieceKind::Thinking, 9, 2)]),
            [2],
            "output left over and no piece to take it"
        );
    }

    #[test]
    fn a_share_adds_up_to_the_total_and_follows_the_weights() {
        assert_eq!(share(10, &[1, 1, 1]), Some(vec![4, 3, 3]));
        assert_eq!(share(7, &[0, 2, 5]), Some(vec![0, 2, 5]));
        assert_eq!(share(100, &[3, 1]), Some(vec![75, 25]));
        assert_eq!(share(0, &[0, 0]), Some(vec![0, 0]));
        assert_eq!(share(5, &[0, 0]), None);
        assert_eq!(share(5, &[]), None);
        assert_eq!(share(0, &[]), Some(vec![]));
    }
}

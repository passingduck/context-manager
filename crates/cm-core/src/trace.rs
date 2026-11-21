//! Past sessions (design section 14): what an adapter reads from a session record, and its
//! projections into [`Trace`], [`StepPieces`] and [`PieceText`].
//!
//! An adapter returns a [`TraceData`]: every distinct block once, as the spans it is made of, and
//! every step as references to blocks. [`OpenTrace`] counts tokens once per span, fits the counts
//! to the usage the API reported ([`fit`]), and computes `shared`, `by_kind`, `top`, and the
//! problems that follow from the steps. No adapter builds those.

mod fit;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

pub use fit::{Added, Coarse, Fit, StepFit, Unfitted, prompt};

use crate::model::*;
use crate::tokens;

/// Index into [`TraceData::blocks`].
pub type BlockId = u32;
/// Index into [`TraceData::messages`].
pub type MessageId = u32;

/// How many pieces `Trace.top` lists.
pub const TOP_PIECES: usize = 30;
/// `trace_piece` returns at most this many bytes of a piece's text.
pub const MAX_PIECE_TEXT: usize = 1024 * 1024;
/// `TracePiece.head` holds at most this many characters.
pub const HEAD_CHARS: usize = 160;

/// A span of a block's text with one kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub kind: PieceKind,
    pub label: Option<String>,
    pub file: Option<AbsPath>,
    pub text: String,
    /// Bytes the record holds for the span besides its text, such as the signature of a
    /// thinking block whose text the record leaves out.
    pub opaque: u64,
}

impl Span {
    pub fn new(kind: PieceKind, text: impl Into<String>) -> Span {
        Span {
            kind,
            label: None,
            file: None,
            text: text.into(),
            opaque: 0,
        }
    }

    pub fn with_opaque(mut self, bytes: u64) -> Span {
        self.opaque = bytes;
        self
    }

    pub fn labelled(mut self, label: Option<String>) -> Span {
        self.label = label;
        self
    }

    pub fn from_file(mut self, file: Option<AbsPath>) -> Span {
        self.file = file;
        self
    }
}

/// A system block, a tool definition, or one content block of a message, as the spans it is
/// made of. The spans joined give the block's text. A block has at least one span.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub spans: Vec<Span>,
}

impl Block {
    /// An empty list becomes one empty span of `kind`, so every block has a place in a request.
    pub fn new(spans: Vec<Span>, kind: PieceKind) -> Block {
        if spans.is_empty() {
            return Block {
                spans: vec![Span::new(kind, "")],
            };
        }
        Block { spans }
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// A message of a request: its role and its content blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<BlockId>,
}

/// Where a request carried a cache mark. A message mark names the block's position in the
/// request: the message index and the block index within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkAt {
    System { block: u32 },
    Tool { index: u32 },
    Message { message: u32, block: u32 },
}

/// One recorded request, rebuilt from the record.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The rebuilt request matches the digest the agent recorded.
    pub exact: bool,
    pub system: Vec<BlockId>,
    pub tools: Vec<BlockId>,
    /// In request order. `None` is a message the record names but does not hold.
    pub messages: Vec<Option<MessageId>>,
    pub marks: Vec<MarkAt>,
}

/// One call of the model.
#[derive(Debug, Clone, PartialEq)]
pub struct StepData {
    pub at: String,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub sent: Option<Request>,
    /// The blocks of the reply, in order.
    pub reply: Vec<BlockId>,
}

/// What an adapter reads from one session record.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceData {
    pub info: TraceInfo,
    /// Bytes of the record the data was built from.
    pub read: u64,
    pub blocks: Vec<Block>,
    pub messages: Vec<Message>,
    pub steps: Vec<StepData>,
    pub boundaries: Vec<Boundary>,
    pub parts: Vec<PartInfo>,
    /// What only the reader knows: unreadable lines, a record over the limit. The projection
    /// adds `NoRequests` and `Inexact` from the steps.
    pub problems: Vec<TraceProblem>,
}

/// A trace with its tokens counted and fitted and its overview computed. The host keeps the
/// last two.
#[derive(Debug)]
pub struct OpenTrace {
    data: TraceData,
    /// Per step, the sizes of its request's pieces.
    fits: Vec<StepFit>,
    trace: Trace,
}

/// One piece of a request: a span of a block, at a place.
#[derive(Debug, Clone, PartialEq)]
struct At {
    block: BlockId,
    span: u32,
    place: PiecePlace,
}

impl OpenTrace {
    pub fn new(data: TraceData) -> OpenTrace {
        let tokens = count_all(&data.blocks);
        let (trace, fits) = project(&data, &tokens);
        OpenTrace { data, fits, trace }
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    pub fn read(&self) -> u64 {
        self.data.read
    }

    /// How each step's sizes were found, in step order.
    pub fn fits(&self) -> &[StepFit] {
        &self.fits
    }

    /// The pieces of one step's request. Empty for a step without one. `None` for no such step.
    pub fn step_pieces(&self, step: u32) -> Option<Vec<TracePiece>> {
        let data = self.data.steps.get(step as usize)?;
        let Some(request) = &data.sent else {
            return Some(Vec::new());
        };
        let (list, _) = listing(&self.data, request);
        let sizes = &self.fits[step as usize].sizes;
        Some(
            list.into_iter()
                .zip(sizes)
                .map(|(at, &tokens)| {
                    let span = self.span(&at);
                    TracePiece {
                        kind: span.kind,
                        label: span.label.clone(),
                        file: span.file.clone(),
                        bytes: span.text.len() as u64,
                        tokens,
                        head: span.text.chars().take(HEAD_CHARS).collect(),
                        place: at.place,
                    }
                })
                .collect(),
        )
    }

    /// The text of one piece, cut at [`MAX_PIECE_TEXT`]. `None` for no such step or piece.
    pub fn piece_text(&self, step: u32, piece: PieceRef) -> Option<PieceText> {
        let data = self.data.steps.get(step as usize)?;
        let span = match piece {
            PieceRef::Sent { piece } => {
                let (list, _) = listing(&self.data, data.sent.as_ref()?);
                self.span(list.get(piece as usize)?)
            }
            PieceRef::Reply { piece } => data
                .reply
                .iter()
                .flat_map(|&block| &self.data.blocks[block as usize].spans)
                .nth(piece as usize)?,
        };
        let kept = cut(&span.text, MAX_PIECE_TEXT);
        Some(PieceText::Current {
            text: kept.to_owned(),
            bytes: span.text.len() as u64,
            cut: kept.len() < span.text.len(),
        })
    }

    fn span(&self, at: &At) -> &Span {
        &self.data.blocks[at.block as usize].spans[at.span as usize]
    }
}

/// The longest prefix of `text` of at most `max` bytes that ends at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The pieces of a request in the order the model received them, and the indexes of the pieces
/// that end at a cache mark.
fn listing(data: &TraceData, request: &Request) -> (Vec<At>, Vec<u32>) {
    let mut list = Vec::new();
    let mut ends: HashMap<MarkAt, u32> = HashMap::new();
    let mut push = |list: &mut Vec<At>, block: BlockId, place: PiecePlace, end: MarkAt| {
        let spans = data.blocks[block as usize].spans.len() as u32;
        for span in 0..spans {
            list.push(At {
                block,
                span,
                place: place.clone(),
            });
        }
        if spans > 0 {
            ends.insert(end, list.len() as u32 - 1);
        }
    };
    for (i, &block) in request.system.iter().enumerate() {
        let i = i as u32;
        push(
            &mut list,
            block,
            PiecePlace::System { block: i },
            MarkAt::System { block: i },
        );
    }
    for (i, &block) in request.tools.iter().enumerate() {
        let i = i as u32;
        push(
            &mut list,
            block,
            PiecePlace::Tool { index: i },
            MarkAt::Tool { index: i },
        );
    }
    for (n, message) in request.messages.iter().enumerate() {
        let Some(message) = message else { continue };
        let message = &data.messages[*message as usize];
        for (k, &block) in message.blocks.iter().enumerate() {
            let (n, k) = (n as u32, k as u32);
            push(
                &mut list,
                block,
                PiecePlace::Message {
                    message: n,
                    role: message.role,
                    block: k,
                },
                MarkAt::Message {
                    message: n,
                    block: k,
                },
            );
        }
    }
    let mut marks: Vec<u32> = request
        .marks
        .iter()
        .filter_map(|mark| ends.get(mark).copied())
        .collect();
    marks.sort_unstable();
    marks.dedup();
    (list, marks)
}

/// Two pieces are equal when they sit at the same place and hold the same span.
fn same(data: &TraceData, a: &At, b: &At) -> bool {
    if a.place != b.place {
        return false;
    }
    if a.block == b.block && a.span == b.span {
        return true;
    }
    let x = &data.blocks[a.block as usize].spans[a.span as usize];
    let y = &data.blocks[b.block as usize].spans[b.span as usize];
    x == y
}

fn project(data: &TraceData, tokens: &[Vec<u32>]) -> (Trace, Vec<StepFit>) {
    let mut fitter = fit::Fitter::new(data, tokens);
    let mut steps = Vec::with_capacity(data.steps.len());
    let mut previous: Option<Vec<At>> = None;
    // The step and the piece at which each span was first sent.
    let mut first: HashMap<(BlockId, u32), (u32, u32)> = HashMap::new();
    for (index, step) in data.steps.iter().enumerate() {
        let request = step.sent.as_ref().map(|request| listing(data, request));
        fitter.push(request.as_ref().map(|(list, _)| list.clone()));
        let fitted = &fitter.fits[index];
        let sent = request.map(|(list, marks)| {
            let shared = previous.as_ref().map_or(0, |before| {
                before
                    .iter()
                    .zip(&list)
                    .take_while(|(a, b)| same(data, a, b))
                    .count()
            });
            let mut kinds: BTreeMap<PieceKind, (u32, u32)> = BTreeMap::new();
            for (i, (at, &size)) in list.iter().zip(&fitted.sizes).enumerate() {
                let kind = data.blocks[at.block as usize].spans[at.span as usize].kind;
                let total = kinds.entry(kind).or_default();
                if i < shared {
                    total.0 = total.0.saturating_add(size);
                } else {
                    total.1 = total.1.saturating_add(size);
                }
                first
                    .entry((at.block, at.span))
                    .or_insert((index as u32, i as u32));
            }
            let sent = Sent {
                exact: step.sent.as_ref().is_some_and(|r| r.exact),
                pieces: list.len() as u32,
                shared: shared as u32,
                fitted: fitted.fit.fitted(),
                by_kind: kinds
                    .into_iter()
                    .map(|(kind, (shared, fresh))| KindTotal {
                        kind,
                        shared,
                        fresh,
                    })
                    .collect(),
                marks,
            };
            previous = Some(list);
            sent
        });
        if sent.is_none() {
            previous = None;
        }
        let reply = step
            .reply
            .iter()
            .flat_map(|&block| {
                data.blocks[block as usize]
                    .spans
                    .iter()
                    .zip(&tokens[block as usize])
            })
            .map(|(span, &tokens)| ReplyPiece {
                kind: span.kind,
                label: span.label.clone(),
                bytes: span.text.len() as u64,
                tokens,
            })
            .collect();
        steps.push(Step {
            at: step.at.clone(),
            model: step.model.clone(),
            usage: step.usage.clone(),
            sent,
            reply,
        });
    }
    let fits = fitter.fits;

    let mut top: Vec<TopPiece> = first
        .into_iter()
        .map(|((block, span), (step, piece))| {
            let s = &data.blocks[block as usize].spans[span as usize];
            TopPiece {
                step,
                piece,
                kind: s.kind,
                label: s.label.clone(),
                tokens: fits[step as usize].sizes[piece as usize],
            }
        })
        .collect();
    top.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then(a.step.cmp(&b.step))
            .then(a.piece.cmp(&b.piece))
    });
    top.truncate(TOP_PIECES);

    let mut problems = Vec::new();
    let unrecorded = steps.iter().filter(|s| s.sent.is_none()).count() as u32;
    if unrecorded > 0 {
        problems.push(TraceProblem::NoRequests { steps: unrecorded });
    }
    let inexact = steps
        .iter()
        .filter(|s| s.sent.as_ref().is_some_and(|sent| !sent.exact))
        .count() as u32;
    if inexact > 0 {
        problems.push(TraceProblem::Inexact { steps: inexact });
    }
    problems.extend(data.problems.iter().cloned());

    let trace = Trace {
        info: data.info.clone(),
        read: data.read,
        steps,
        boundaries: data.boundaries.clone(),
        parts: data.parts.clone(),
        top,
        problems,
    };
    (trace, fits)
}

/// The o200k count of every span, once each. Large records hold tens of megabytes of text, so
/// the blocks are counted on several threads.
fn count_all(blocks: &[Block]) -> Vec<Vec<u32>> {
    const CHUNK: usize = 64;
    let chunks: Vec<&[Block]> = blocks.chunks(CHUNK).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8)
        .min(chunks.len().max(1));
    let mut counted: Vec<(usize, Vec<Vec<u32>>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(chunk) = chunks.get(i) else { break };
                        let counts = chunk
                            .iter()
                            .map(|block| {
                                block
                                    .spans
                                    .iter()
                                    .map(|span| tokens::count(&span.text))
                                    .collect()
                            })
                            .collect();
                        out.push((i, counts));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a token counting thread"))
            .collect()
    });
    counted.sort_by_key(|(i, _)| *i);
    counted.into_iter().flat_map(|(_, counts)| counts).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> TraceInfo {
        TraceInfo {
            agent: AgentId::parse("claude").unwrap(),
            id: TraceId("s".into()),
            file: AbsPath::parse("/r/s.jsonl").unwrap(),
            bytes: 0,
            title: None,
            started: None,
            last: None,
            cwd: None,
        }
    }

    fn block(kind: PieceKind, text: &str) -> Block {
        Block::new(vec![Span::new(kind, text)], kind)
    }

    fn request(messages: Vec<Option<MessageId>>) -> Request {
        Request {
            exact: true,
            system: vec![0],
            tools: vec![1],
            messages,
            marks: vec![MarkAt::System { block: 0 }],
        }
    }

    fn step(sent: Option<Request>, reply: Vec<BlockId>) -> StepData {
        StepData {
            at: "2026-11-21T00:00:00Z".into(),
            model: None,
            usage: None,
            sent,
            reply,
        }
    }

    /// System 0, tool 1, a user message (2: one block of two spans), a reply (3), a second user
    /// message (4) whose text equals the first one's.
    fn data() -> TraceData {
        TraceData {
            info: info(),
            read: 10,
            blocks: vec![
                block(PieceKind::System, "You are a test."),
                block(PieceKind::ToolDefinition, "{\"name\":\"Bash\"}"),
                Block::new(
                    vec![
                        Span::new(PieceKind::Instructions, "be brief"),
                        Span::new(PieceKind::Frame, "\n\n"),
                    ],
                    PieceKind::Other,
                ),
                block(PieceKind::ModelText, "hello there"),
                Block::new(
                    vec![
                        Span::new(PieceKind::Instructions, "be brief"),
                        Span::new(PieceKind::Frame, "\n\n"),
                    ],
                    PieceKind::Other,
                ),
            ],
            messages: vec![
                Message {
                    role: Role::User,
                    blocks: vec![2],
                },
                Message {
                    role: Role::Model,
                    blocks: vec![3],
                },
                Message {
                    role: Role::User,
                    blocks: vec![4],
                },
            ],
            steps: vec![
                step(Some(request(vec![Some(0)])), vec![3]),
                step(Some(request(vec![Some(0), Some(1), None])), vec![]),
                step(None, vec![3]),
                step(Some(request(vec![Some(2)])), vec![]),
            ],
            boundaries: Vec::new(),
            parts: Vec::new(),
            problems: vec![TraceProblem::UnreadableLines { lines: 1 }],
        }
    }

    #[test]
    fn shared_counts_the_common_prefix_and_resets_after_a_step_without_a_request() {
        let open = OpenTrace::new(data());
        let trace = open.trace();
        let sent: Vec<_> = trace.steps.iter().map(|s| s.sent.as_ref()).collect();
        assert_eq!(sent[0].unwrap().pieces, 4);
        assert_eq!(sent[0].unwrap().shared, 0);
        // The second request repeats every piece of the first and adds the reply.
        assert_eq!(sent[1].unwrap().pieces, 5);
        assert_eq!(sent[1].unwrap().shared, 4);
        assert!(sent[2].is_none());
        // The step before it has no request, so nothing counts as shared.
        assert_eq!(sent[3].unwrap().shared, 0);
        assert_eq!(sent[0].unwrap().marks, vec![0]);
    }

    #[test]
    fn by_kind_adds_up_to_the_pieces_and_the_problems_follow_from_the_steps() {
        let open = OpenTrace::new(data());
        let trace = open.trace();
        for (i, step) in trace.steps.iter().enumerate() {
            let Some(sent) = &step.sent else { continue };
            let pieces = open.step_pieces(i as u32).unwrap();
            assert_eq!(pieces.len() as u32, sent.pieces);
            let total: u32 = pieces.iter().map(|p| p.tokens).sum();
            let by_kind: u32 = sent.by_kind.iter().map(|k| k.shared + k.fresh).sum();
            assert_eq!(total, by_kind);
            let shared: u32 = pieces[..sent.shared as usize]
                .iter()
                .map(|p| p.tokens)
                .sum();
            assert_eq!(shared, sent.by_kind.iter().map(|k| k.shared).sum::<u32>());
        }
        assert_eq!(
            trace.problems,
            vec![
                TraceProblem::NoRequests { steps: 1 },
                TraceProblem::UnreadableLines { lines: 1 },
            ]
        );
    }

    #[test]
    fn top_names_each_span_at_the_step_that_sent_it_first() {
        let open = OpenTrace::new(data());
        let top = &open.trace().top;
        let reply = top.iter().find(|t| t.kind == PieceKind::ModelText).unwrap();
        assert_eq!((reply.step, reply.piece), (1, 4));
        assert!(top.windows(2).all(|w| w[0].tokens >= w[1].tokens));
    }

    fn usage(
        input: u64,
        cache_read: u64,
        cache_write: u64,
        output: u64,
        thinking: Option<u64>,
    ) -> Usage {
        Usage {
            input,
            cache_read,
            cache_write,
            output,
            thinking,
        }
    }

    fn thinking(signature: u64) -> Block {
        Block::new(
            vec![Span::new(PieceKind::Thinking, "").with_opaque(signature)],
            PieceKind::Thinking,
        )
    }

    /// A main request (step 0) whose reply holds two thinking blocks, a text and a tool call; a
    /// side request with its own system block (step 1); the main request again with the reply
    /// and a tool result added (step 2); and the same with a prompt added, without usage
    /// (step 3). The prompt sizes are 100, 20 and 220, so step 2 added 120 real tokens.
    fn chain() -> TraceData {
        let mut data = data();
        data.blocks = vec![
            block(PieceKind::System, "You are a test."),
            block(PieceKind::ToolDefinition, "{\"name\":\"Bash\"}"),
            block(PieceKind::UserText, "hi"),
            thinking(100),
            thinking(300),
            block(PieceKind::ModelText, "hello there, I will run it"),
            block(PieceKind::ToolCall, "{\"command\":\"ls\"}"),
            block(PieceKind::ToolResult, "a.txt b.txt c.txt"),
            block(PieceKind::System, "Write a title."),
            block(PieceKind::UserText, "next"),
        ];
        let message = |role, blocks: Vec<BlockId>| Message { role, blocks };
        data.messages = vec![
            message(Role::User, vec![2]),
            message(Role::Model, vec![3, 4, 5, 6]),
            message(Role::User, vec![7]),
            message(Role::User, vec![9]),
        ];
        let main = |messages: Vec<Option<MessageId>>| request(messages);
        let side = Request {
            exact: true,
            system: vec![8],
            tools: vec![],
            messages: vec![Some(3)],
            marks: vec![],
        };
        let with = |sent, reply, usage| StepData {
            usage,
            ..step(Some(sent), reply)
        };
        data.steps = vec![
            with(
                main(vec![Some(0)]),
                vec![3, 4, 5, 6],
                Some(usage(10, 0, 90, 50, Some(40))),
            ),
            with(side, vec![], Some(usage(20, 0, 0, 3, None))),
            with(
                main(vec![Some(0), Some(1), Some(2)]),
                vec![],
                Some(usage(0, 100, 120, 5, None)),
            ),
            with(main(vec![Some(0), Some(1), Some(2), Some(3)]), vec![], None),
        ];
        data.problems = Vec::new();
        data
    }

    fn sizes(open: &OpenTrace, step: u32) -> Vec<u32> {
        open.step_pieces(step)
            .unwrap()
            .iter()
            .map(|p| p.tokens)
            .collect()
    }

    #[test]
    fn the_pieces_a_request_added_cost_the_difference_of_the_prompt_sizes() {
        let open = OpenTrace::new(chain());
        let steps = &open.trace().steps;
        let fitted: Vec<bool> = steps
            .iter()
            .map(|s| s.sent.as_ref().unwrap().fitted)
            .collect();
        assert_eq!(fitted, [true, true, true, false]);
        assert_eq!(sizes(&open, 0).iter().sum::<u32>(), 100);
        assert_eq!(sizes(&open, 1).iter().sum::<u32>(), 20);

        // Step 2 extends step 0, not the side request before it. Its pieces after step 0's three
        // are: two thinking blocks, the text, the tool call, the tool result.
        let step2 = sizes(&open, 2);
        assert_eq!(step2[..3], sizes(&open, 0)[..]);
        let added = &step2[3..];
        assert_eq!(added.iter().sum::<u32>(), 220 - 100);
        // The thinking tokens of step 0's reply, shared 1 to 3 by the signatures.
        assert_eq!(added[..2], [10, 30]);
        // The rest of its output, 50 - 40, goes to the text and the call.
        assert_eq!(added[2] + added[3], 10);
        // What is left is the tool result.
        assert_eq!(added[4], 120 - 40 - 10);
        assert_eq!(open.fits()[2].fit, Fit::Split);

        // Without usage the new prompt keeps its raw count, and the rest is carried.
        let step3 = sizes(&open, 3);
        assert_eq!(step3[..8], step2[..]);
        assert_eq!(open.fits()[3].fit, Fit::Raw(Unfitted::NoUsage));

        // A row of the largest pieces carries the fitted size of the step that sent it first.
        let result = open
            .trace()
            .top
            .iter()
            .find(|t| t.kind == PieceKind::ToolResult);
        assert_eq!(
            result.map(|t| (t.step, t.piece, t.tokens)),
            Some((2, 7, 70))
        );
    }

    #[test]
    fn a_step_whose_numbers_do_not_close_falls_back_to_one_factor_or_its_raw_counts() {
        // 45 added tokens cannot hold 40 thinking and 10 other output tokens.
        let mut data = chain();
        data.steps[2].usage = Some(usage(0, 100, 45, 5, None));
        let open = OpenTrace::new(data);
        assert_eq!(open.fits()[2].fit, Fit::Factor(Coarse::DoesNotClose));
        assert!(open.trace().steps[2].sent.as_ref().unwrap().fitted);
        assert_eq!(sizes(&open, 2)[3..].iter().sum::<u32>(), 45);

        // A reply whose thinking was not counted.
        let mut data = chain();
        data.steps[0].usage = Some(usage(10, 0, 90, 50, None));
        let open = OpenTrace::new(data);
        assert_eq!(open.fits()[2].fit, Fit::Factor(Coarse::ThinkingUnknown));
        assert_eq!(sizes(&open, 2).iter().sum::<u32>(), 220);

        // A request that dropped one thinking block of the reply.
        let mut data = chain();
        data.messages[1].blocks = vec![3, 5, 6];
        let open = OpenTrace::new(data);
        assert_eq!(open.fits()[2].fit, Fit::Factor(Coarse::ReplyNotWhole));

        // A prompt smaller than what the shared pieces already cost.
        let mut data = chain();
        data.steps[2].usage = Some(usage(0, 90, 0, 5, None));
        let open = OpenTrace::new(data);
        assert_eq!(open.fits()[2].fit, Fit::Raw(Unfitted::SharedTooLarge));
        assert!(!open.trace().steps[2].sent.as_ref().unwrap().fitted);
    }

    #[test]
    fn a_piece_text_is_cut_at_a_character_boundary() {
        let mut data = data();
        let long = "é".repeat(MAX_PIECE_TEXT);
        data.blocks[3] = block(PieceKind::ModelText, &long);
        let open = OpenTrace::new(data);
        let Some(PieceText::Current { text, bytes, cut }) =
            open.piece_text(0, PieceRef::Reply { piece: 0 })
        else {
            panic!("the reply piece exists");
        };
        assert!(cut);
        assert_eq!(bytes, long.len() as u64);
        assert_eq!(text.len(), MAX_PIECE_TEXT);
        assert_eq!(
            open.piece_text(1, PieceRef::Sent { piece: 0 }),
            Some(PieceText::Current {
                text: "You are a test.".into(),
                bytes: 15,
                cut: false
            })
        );
        assert_eq!(open.piece_text(9, PieceRef::Sent { piece: 0 }), None);
        assert_eq!(open.piece_text(2, PieceRef::Sent { piece: 0 }), None);
    }
}

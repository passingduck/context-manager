import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { MergeView } from "@codemirror/merge";
import { EditorState, RangeSetBuilder, type Extension } from "@codemirror/state";
import { Decoration, EditorView, GutterMarker, gutter, lineNumbers } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { useEffect, useRef } from "react";
import type { Format } from "../gen/Format";

const highlight = HighlightStyle.define([
  { tag: [tags.heading, tags.strong], fontWeight: "700" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: [tags.keyword, tags.processingInstruction], color: "var(--c3)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--ok)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--c4)" },
  { tag: [tags.propertyName, tags.attributeName], color: "var(--info)" },
  { tag: [tags.comment, tags.quote], color: "var(--fg-faint)", fontStyle: "italic" },
  { tag: [tags.link, tags.url], color: "var(--accent)", textDecoration: "underline" },
  { tag: [tags.monospace, tags.contentSeparator], color: "var(--c0)" },
  { tag: tags.meta, color: "var(--fg-faint)" },
]);

function language(format: Format): Extension[] {
  switch (format) {
    case "markdown":
      return [markdown()];
    case "json":
    case "jsonc":
      return [json()];
    case "toml":
      return [StreamLanguage.define(toml)];
    default:
      return [];
  }
}

class CutMarker extends GutterMarker {
  constructor(private readonly title: string) {
    super();
  }
  override toDOM(): HTMLElement {
    const el = document.createElement("span");
    el.className = "cm-cut-marker";
    el.title = this.title;
    return el;
  }
}

/** Dims the lines after a truncation point and marks the cut in the gutter. */
function cutExtension(cutLine: number, title: string): Extension {
  const marker = new CutMarker(title);
  const dim = Decoration.line({ class: "cm-cut-after" });
  return [
    gutter({
      class: "cm-cut-gutter",
      lineMarker: (view, line) => (view.state.doc.lineAt(line.from).number === cutLine ? marker : null),
      initialSpacer: () => marker,
    }),
    EditorView.decorations.of((view) => {
      const builder = new RangeSetBuilder<Decoration>();
      for (let n = cutLine + 1; n <= view.state.doc.lines; n++) builder.add(view.state.doc.line(n).from, view.state.doc.line(n).from, dim);
      return builder.finish();
    }),
  ];
}

function hitExtension(line: number): Extension {
  const mark = Decoration.line({ class: "cm-hit-line" });
  return EditorView.decorations.of((view) => {
    if (line < 1 || line > view.state.doc.lines) return Decoration.none;
    const from = view.state.doc.line(line).from;
    return Decoration.set([mark.range(from)]);
  });
}

const base: Extension[] = [
  lineNumbers(),
  syntaxHighlighting(highlight),
  EditorView.lineWrapping,
  EditorState.readOnly.of(true),
];

interface EditorProps {
  text: string;
  format: Format;
  /** Line after which the agent no longer receives the text. */
  cutLine?: number | null;
  cutTitle?: string;
  /** Line to scroll to and highlight. */
  revealLine?: number;
}

/** Read-only CodeMirror 6. Remount it (with a `key`) to show another document. */
export function CodeEditor({ text, format, cutLine, cutTitle = "", revealLine = 0 }: EditorProps) {
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!host.current) return;
    const extensions = [...base, ...language(format)];
    if (cutLine) extensions.push(cutExtension(cutLine, cutTitle));
    if (revealLine) extensions.push(hitExtension(revealLine));
    const view = new EditorView({ parent: host.current, state: EditorState.create({ doc: text, extensions }) });
    if (revealLine > 0 && revealLine <= view.state.doc.lines) {
      view.dispatch({ effects: EditorView.scrollIntoView(view.state.doc.line(revealLine).from, { y: "center" }) });
    }
    return () => view.destroy();
  }, [text, format, cutLine, cutTitle, revealLine]);
  return <div ref={host} className="h-full min-h-0 overflow-hidden" />;
}

/** Side by side diff of two texts, read-only. */
export function DiffView({ before, after, format }: { before: string; after: string; format: Format }) {
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!host.current) return;
    const extensions = [...base, ...language(format)];
    const view = new MergeView({
      a: { doc: before, extensions },
      b: { doc: after, extensions },
      parent: host.current,
      collapseUnchanged: { margin: 2, minSize: 4 },
      highlightChanges: true,
      gutter: true,
    });
    return () => view.destroy();
  }, [before, after, format]);
  return <div ref={host} className="max-h-[60vh] overflow-auto rounded border border-line" />;
}

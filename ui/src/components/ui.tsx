import type { ReactNode } from "react";
import type { ApiError } from "../gen/ApiError";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";

export type Tone = "ok" | "warn" | "bad" | "info" | "neutral";

const tones: Record<Tone, string> = {
  ok: "bg-ok-bg text-ok",
  warn: "bg-warn-bg text-warn",
  bad: "bg-bad-bg text-bad",
  info: "bg-info-bg text-info",
  neutral: "bg-sunken text-muted",
};

export function Badge({ tone, children, title }: { tone: Tone; children: ReactNode; title?: string }) {
  return (
    <span title={title} className={`inline-flex shrink-0 items-center whitespace-nowrap rounded px-1.5 py-px text-[11px] font-semibold leading-4 ${tones[tone]}`}>
      {children}
    </span>
  );
}

export function Chip({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span title={title} className="inline-flex shrink-0 items-center whitespace-nowrap rounded border border-line px-1.5 py-px text-[11px] leading-4 text-muted">
      {children}
    </span>
  );
}

export function Mono({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <span className={`font-mono text-[12px] ${className}`}>{children}</span>;
}

export function Button({ children, ...rest }: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      {...rest}
      className={`rounded border border-line bg-bg px-2 py-0.5 text-[12px] hover:bg-sunken disabled:cursor-not-allowed disabled:opacity-50 ${rest.className ?? ""}`}
    >
      {children}
    </button>
  );
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="mx-auto mt-16 max-w-sm px-4 text-center">
      <p className="text-[13px] font-semibold">{title}</p>
      {children && <p className="mt-1 text-muted">{children}</p>}
    </div>
  );
}

export function PaneTitle({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="flex h-10 shrink-0 items-center justify-between gap-2 border-b border-line px-3">
      <h2 className="text-[13px] font-semibold">{children}</h2>
      <div className="flex min-w-0 items-center gap-2">{right}</div>
    </div>
  );
}

function isApiError(error: unknown): error is ApiError {
  return typeof error === "object" && error !== null && "kind" in error;
}

/** A failed query. Operations the host has not implemented yet answer with an `io` error. */
export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const { t, d } = useI18n();
  const text = isApiError(error) ? say(t.apiError, error, d) : String(error);
  return (
    <div role="alert" className="m-3 rounded border border-line bg-subtle p-3">
      <p className="font-semibold">{t.errors.notReady}</p>
      <p className="mt-1 break-words font-mono text-[12px] text-muted">{text}</p>
      {onRetry && (
        <Button className="mt-2" onClick={onRetry}>
          {t.retry}
        </Button>
      )}
    </div>
  );
}

export function Loading() {
  const { t } = useI18n();
  return <p className="p-3 text-muted">{t.loading}</p>;
}

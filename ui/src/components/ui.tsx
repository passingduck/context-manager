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

/** Class names shared by every text input in the write dialogs. */
export const inputClass = "w-full rounded border border-line bg-bg px-2 py-1 text-[13px] placeholder:text-faint disabled:opacity-60";

export function FormRow({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-[12px] font-semibold">{label}</span>
      {children}
      {hint && <span className="mt-0.5 block text-[11px] text-muted">{hint}</span>}
    </label>
  );
}

/** A dialog over the page. `layer` raises it above another dialog that stays open underneath. */
export function Modal({ title, label, onClose, footer, children, layer = 0, wide = false }: { title: ReactNode; label: string; onClose: () => void; footer?: ReactNode; children: ReactNode; layer?: number; wide?: boolean }) {
  return (
    <div
      className="fixed inset-0 grid place-items-center bg-black/45 p-4"
      style={{ zIndex: 40 + layer }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        ref={(node) => node?.focus({ preventScroll: true })}
        className={`flex max-h-[90vh] w-full flex-col rounded-lg border border-line bg-bg shadow-2xl outline-none ${wide ? "max-w-5xl" : "max-w-2xl"}`}
      >
        <header className="flex shrink-0 items-center gap-2 border-b border-line px-4 py-2.5">
          <h2 className="min-w-0 flex-1 text-[14px] font-semibold">{title}</h2>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">{children}</div>
        {footer && <footer className="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-line bg-subtle px-4 py-2.5">{footer}</footer>}
      </div>
    </div>
  );
}

export function PrimaryButton({ children, ...rest }: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <Button {...rest} className={`border-accent bg-accent text-bg hover:bg-accent ${rest.className ?? ""}`}>
      {children}
    </Button>
  );
}

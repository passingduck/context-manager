import { useEffect, useRef, useState } from "react";
import { useProjects } from "../api/queries";
import { Button, Mono } from "../components/ui";
import type { AbsPath } from "../gen/AbsPath";
import { useI18n, type Lang } from "../i18n";
import type { Route } from "../lib/route";
import type { Theme } from "./theme";

type Go = (patch: Partial<Route>, replace?: boolean) => void;

/** A text box that writes to the route after a short pause, so each keystroke is not a history entry. */
function Debounced({ value, onCommit, ...rest }: { value: string; onCommit: (next: string) => void } & Omit<React.InputHTMLAttributes<HTMLInputElement>, "value" | "onChange">) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  useEffect(() => {
    if (draft === value) return;
    const timer = setTimeout(() => onCommit(draft), 250);
    return () => clearTimeout(timer);
  }, [draft, value, onCommit]);
  return <input {...rest} value={draft} onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => e.key === "Escape" && setDraft("")} />;
}

const inputClass = "h-7 w-full rounded border border-line bg-bg px-2 text-[12px] placeholder:text-faint";

export function TopBar({ cwd, home, route, go, theme, toggleTheme }: { cwd: AbsPath; home: AbsPath; route: Route; go: Go; theme: Theme; toggleTheme: () => void }) {
  const { t, lang, setLang } = useI18n();
  return (
    <header className="flex h-12 shrink-0 items-center gap-3 border-b border-line bg-bg px-3">
      <div className="flex shrink-0 items-center gap-2 font-semibold">
        <span className="size-4 rounded bg-accent" aria-hidden />
        {t.title}
      </div>
      <ProjectSwitcher cwd={cwd} home={home} go={go} />
      <div className="w-60 shrink-0">
        <Debounced type="search" aria-label={t.topbar.search} placeholder={t.topbar.searchPlaceholder} value={route.q} onCommit={(q) => go({ q }, true)} className={inputClass} />
      </div>
      <div className="min-w-40 max-w-sm flex-1">
        <Debounced type="text" aria-label={t.topbar.simulate} placeholder={t.topbar.simulatePlaceholder} value={route.sim} onCommit={(sim) => go({ sim, view: "context" }, true)} className={`${inputClass} font-mono`} />
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-2">
        <div role="group" aria-label={t.language} className="flex overflow-hidden rounded border border-line text-[12px]">
          {(["ko", "en"] as const satisfies readonly Lang[]).map((code) => (
            <button key={code} type="button" data-testid={`lang-${code}`} aria-pressed={lang === code} onClick={() => setLang(code)} className="px-2 py-1 hover:bg-sunken aria-pressed:bg-accent aria-pressed:font-semibold aria-pressed:text-bg">
              {code === "ko" ? "한국어" : "EN"}
            </button>
          ))}
        </div>
        <Button onClick={toggleTheme} title={theme === "dark" ? t.theme.switchToLight : t.theme.switchToDark} className="h-7">
          {theme === "dark" ? t.theme.light : t.theme.dark}
        </Button>
      </div>
    </header>
  );
}

function ProjectSwitcher({ cwd, home, go }: { cwd: AbsPath; home: AbsPath; go: Go }) {
  const { t } = useI18n();
  const d = (path: string) => (home && path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path);
  const projects = useProjects();
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent ? event.key === "Escape" : !box.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);
  const choose = (path: string) => {
    go({ p: path, sel: "", ag: "", ln: 0 });
    setOpen(false);
    setDraft("");
  };
  return (
    <div ref={box} className="relative shrink-0">
      <button type="button" aria-haspopup="listbox" aria-expanded={open} aria-label={t.topbar.project} onClick={() => setOpen(!open)} className="flex h-7 max-w-64 items-center gap-1.5 rounded border border-line bg-bg px-2 hover:bg-sunken">
        <Mono className="truncate">{d(cwd)}</Mono>
        <span aria-hidden className="text-faint">▾</span>
      </button>
      {open && (
        <div className="absolute left-0 top-8 z-20 w-80 rounded border border-line bg-bg p-2 shadow-lg">
          <p className="px-1 pb-1 text-[11px] font-semibold uppercase tracking-wide text-faint">{t.topbar.projects}</p>
          <ul role="listbox" className="max-h-56 overflow-y-auto">
            {(projects.data ?? []).map((project) => (
              <li key={project.path} role="option" aria-selected={project.path === cwd}>
                <button type="button" disabled={!project.exists} onClick={() => choose(project.path)} className="flex w-full items-center gap-2 rounded px-1.5 py-1 text-left hover:bg-sunken disabled:opacity-50 aria-selected:font-semibold">
                  <Mono className="min-w-0 flex-1 truncate">{d(project.path)}</Mono>
                </button>
              </li>
            ))}
            {projects.isError || (projects.data ?? []).length === 0 ? <li className="px-1.5 py-1 text-muted">{t.topbar.noProjects}</li> : null}
          </ul>
          <form
            className="mt-2 border-t border-line pt-2"
            onSubmit={(event) => {
              event.preventDefault();
              if (draft.trim()) choose(draft.trim());
            }}
          >
            <label className="block px-1 pb-1 text-[11px] font-semibold uppercase tracking-wide text-faint" htmlFor="project-path">{t.topbar.openPath}</label>
            <input id="project-path" value={draft} onChange={(e) => setDraft(e.target.value)} placeholder="/path/to/project" title={t.topbar.openPathHint} className={`${inputClass} font-mono`} />
          </form>
        </div>
      )}
    </div>
  );
}

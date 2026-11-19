// Deleting a file from a row's menu or from the detail pane. Both go through the apply flow.
import { useState } from "react";
import { useProject } from "../app/project";
import type { AbsPath } from "../gen/AbsPath";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import { useWrite } from "./flow";

export interface Trash {
  can: boolean;
  /** Why the file cannot be deleted, when it cannot. */
  reason: string | undefined;
  run: () => void;
}

export function useTrash(path: AbsPath): Trash {
  const { t, d } = useI18n();
  const project = useProject();
  const write = useWrite();
  const entry = project.snapshot.files[path];
  const state = entry?.state;
  const skill = project.snapshot.agents.flatMap((view) => view.extensions).find((ext) => ext.file === path && ext.extKind === "skill");
  const reason = write.blocked ?? (entry?.policy.kind === "readOnly" ? say(t.readOnlyWhy, entry.policy.why, d) : state?.kind !== "present" ? t.write.notPresent : undefined);
  return {
    can: reason === undefined,
    reason,
    run: () => {
      if (!entry || state?.kind !== "present") return;
      write.propose({
        edit: { kind: "trash", path, base: state.rev },
        title: t.write.trashTitle(entry.display),
        notice: skill ? t.write.skillDirGoes(skill.name, d(path.slice(0, path.lastIndexOf("/")))) : undefined,
      });
    },
  };
}

/** The "..." menu at the end of a tree row. */
export function RowMenu({ path }: { path: AbsPath }) {
  const { t } = useI18n();
  const trash = useTrash(path);
  const [open, setOpen] = useState(false);
  return (
    <span className="relative shrink-0" onClick={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}>
      <button type="button" tabIndex={-1} aria-haspopup="menu" aria-expanded={open} aria-label={t.write.rowMenu} onClick={() => setOpen(!open)} className="rounded px-1 text-muted hover:bg-sunken">
        ...
      </button>
      {open && (
        <>
          <button type="button" tabIndex={-1} aria-label={t.write.close} className="fixed inset-0 z-30 cursor-default" onClick={() => setOpen(false)} />
          <span role="menu" className="absolute right-0 top-full z-40 block min-w-40 rounded border border-line bg-bg py-1 shadow-lg">
            <button
              type="button"
              role="menuitem"
              disabled={!trash.can}
              title={trash.reason}
              onClick={() => {
                setOpen(false);
                trash.run();
              }}
              className="block w-full px-3 py-1 text-left hover:bg-subtle disabled:cursor-not-allowed disabled:opacity-50"
            >
              {t.detail.trash}
            </button>
          </span>
        </>
      )}
    </span>
  );
}

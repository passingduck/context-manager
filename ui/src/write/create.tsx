// Creating things: the "new" menu built from `slots`, and the add-memory form.
import { useState } from "react";
import { useProject } from "../app/project";
import { Button, Chip, FormRow, inputClass, Modal, Mono, PrimaryButton } from "../components/ui";
import type { AgentId } from "../gen/AgentId";
import type { MemoryStore } from "../gen/MemoryStore";
import type { Slot } from "../gen/Slot";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import type { Route } from "../lib/route";
import { useWrite } from "./flow";

function target(slot: Slot, name: string): string {
  switch (slot.place.kind) {
    case "file":
      return slot.place.path;
    case "namedFile":
      return `${slot.place.dir}/${name || "<name>"}.${slot.place.ext}`;
    case "namedDir":
      return `${slot.place.dir}/${name || "<name>"}/${slot.place.file}`;
  }
}

/** One entry for every place where a new item would load, grouped by agent. */
export function NewMenu({ go }: { go: (patch: Partial<Route>) => void }) {
  const { t } = useI18n();
  const project = useProject();
  const { blocked } = useWrite();
  const [open, setOpen] = useState(false);
  const [chosen, setChosen] = useState<{ agent: AgentId; slot: Slot } | null>(null);
  const withSlots = project.snapshot.agents.filter((view) => view.slots.length > 0);
  return (
    <div className="relative">
      <Button aria-haspopup="menu" aria-expanded={open} disabled={blocked !== null || withSlots.length === 0} title={blocked ?? undefined} onClick={() => setOpen(!open)}>
        {t.write.newItem}
      </Button>
      {open && (
        <>
          <button type="button" aria-label={t.write.close} className="fixed inset-0 z-30 cursor-default" onClick={() => setOpen(false)} />
          <div role="menu" aria-label={t.write.newItem} className="absolute right-0 top-full z-40 mt-1 w-[22rem] max-w-[80vw] rounded border border-line bg-bg shadow-lg" onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
            {withSlots.map((view) => (
              <div key={view.agent} role="group" aria-label={project.name(view.agent)}>
                <p className="flex items-center gap-2 bg-subtle px-3 py-1 font-semibold">
                  <span className="size-2 rounded-full" style={{ background: project.color(view.agent) }} aria-hidden />
                  {project.name(view.agent)}
                </p>
                {view.slots.map((slot) => {
                  const exists = slot.place.kind === "file" && project.snapshot.files[slot.place.path]?.state.kind === "present";
                  return (
                    <button
                      key={slot.id}
                      type="button"
                      role="menuitem"
                      disabled={exists}
                      onClick={() => {
                        setChosen({ agent: view.agent, slot });
                        setOpen(false);
                      }}
                      className="block w-full px-3 py-1.5 text-left hover:bg-subtle disabled:cursor-not-allowed disabled:opacity-50"
                    >
                      <span className="flex items-center gap-2">
                        <b>{t.newKind[slot.newKind]}</b>
                        <Chip>{say(t.scope, slot.scope, project.display)}</Chip>
                        {exists && <Chip>{t.write.exists}</Chip>}
                      </span>
                      <Mono className="block truncate text-[11px] text-muted">{project.display(target(slot, ""))}</Mono>
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
        </>
      )}
      {chosen && <NewItemForm agent={chosen.agent} slot={chosen.slot} go={go} onClose={() => setChosen(null)} />}
    </div>
  );
}

function NewItemForm({ agent, slot, go, onClose }: { agent: AgentId; slot: Slot; go: (patch: Partial<Route>) => void; onClose: () => void }) {
  const { t } = useI18n();
  const project = useProject();
  const { propose } = useWrite();
  const named = slot.place.kind !== "file";
  const [name, setName] = useState("");
  const bad = named && (name.trim() === "" || /[/\\]/.test(name) || name.startsWith("."));
  const kind = t.newKind[slot.newKind];
  return (
    <Modal
      title={t.write.newTitle(kind, project.name(agent))}
      label={t.write.newTitle(kind, project.name(agent))}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t.write.cancel}</Button>
          <PrimaryButton
            disabled={bad}
            onClick={() =>
              propose({
                edit: { kind: "newItem", agent, slot: slot.id, name: named ? name.trim() : null },
                title: t.write.newTitle(kind, project.name(agent)),
                onApplied: (result) => {
                  const first = result.files[0]?.[0];
                  if (first) go({ view: "context", sel: `file:${first}`, ag: agent, ln: 0 });
                  onClose();
                },
              })
            }
          >
            {t.write.review}
          </PrimaryButton>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-muted">{project.rule(slot.rule)}</p>
        {named && (
          <FormRow label={t.write.name} hint={t.write.nameHint}>
            <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          </FormRow>
        )}
        <p>
          {t.write.willCreateAt} <Mono className="break-all font-semibold">{project.display(target(slot, name.trim()))}</Mono>
        </p>
      </div>
    </Modal>
  );
}

export function AddMemory({ agent, store }: { agent: AgentId; store: MemoryStore }) {
  const { t, d } = useI18n();
  const project = useProject();
  const { blocked } = useWrite();
  const [open, setOpen] = useState(false);
  const readOnly = store.write.kind === "readOnly" ? project.rule(store.write.why) : null;
  const reason = blocked ?? readOnly;
  return (
    <>
      <Button disabled={reason !== null} title={reason ?? say(t.memoryWrite, store.write, d)} onClick={() => setOpen(true)}>
        {t.write.addMemory}
      </Button>
      {open && <MemoryForm agent={agent} store={store} onClose={() => setOpen(false)} />}
    </>
  );
}

function MemoryForm({ agent, store, onClose }: { agent: AgentId; store: MemoryStore; onClose: () => void }) {
  const { t, d } = useI18n();
  const project = useProject();
  const { propose } = useWrite();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [body, setBody] = useState("");
  return (
    <Modal
      title={t.write.addMemoryIn(store.id)}
      label={t.write.addMemoryIn(store.id)}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t.write.cancel}</Button>
          <PrimaryButton
            disabled={title.trim() === ""}
            onClick={() =>
              propose({
                edit: { kind: "addMemory", agent, store: store.id, title: title.trim(), description: description.trim(), body },
                title: t.write.addMemoryIn(store.id),
                onApplied: onClose,
              })
            }
          >
            {t.write.review}
          </PrimaryButton>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-muted">{say(t.memoryWrite, store.write, d)}</p>
        <p className="text-muted">{store.index ? t.write.memoryIndexLine(project.display(store.index)) : t.write.memoryNoIndex}</p>
        <FormRow label={t.write.memTitle}>
          <input className={inputClass} value={title} onChange={(e) => setTitle(e.target.value)} autoFocus />
        </FormRow>
        <FormRow label={t.write.memDescription} hint={t.write.memDescriptionHint}>
          <input className={inputClass} value={description} onChange={(e) => setDescription(e.target.value)} />
        </FormRow>
        <FormRow label={t.write.memBody}>
          <textarea className={`${inputClass} font-mono`} rows={8} value={body} onChange={(e) => setBody(e.target.value)} />
        </FormRow>
      </div>
    </Modal>
  );
}

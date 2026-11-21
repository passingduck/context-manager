import { useI18n } from "../i18n";
import { exact } from "../lib/prefix";
import type { CacheLoss } from "../lib/trace";

/** What a lost cache means, said from the facts of the two requests: one sentence for each fact. `first` names the first piece that differs, when the caller has the pieces. */
export function LossNote({ loss, first, testid }: { loss: CacheLoss; first?: string | null; testid?: string }) {
  const { t } = useI18n();
  const s = t.sessions.lost;
  const why = loss.why;
  return (
    <ul data-testid={testid} className="space-y-0.5">
      <li data-testid="loss-why" className="font-semibold">
        {why.kind === "changed" ? s.whyChanged : why.kind === "expired" ? s.whyExpired : s.whyUnknown}
      </li>
      <li>{s.summary(exact(loss.read), exact(loss.expected))}</li>
      {why.kind === "changed" && (
        <>
          <li>{s.changed(why.shared, why.before)}</li>
          <li>{first ? s.firstNamed(why.at + 1, first) : s.first(why.at + 1)}</li>
        </>
      )}
      {why.kind === "expired" && <li>{s.expired}</li>}
      {why.kind === "expired" && why.gapMs !== null && <li>{s.gap(t.sessions.duration(Math.round(why.gapMs / 1000)))}</li>}
      {why.kind === "unknown" && <li>{s.unknown}</li>}
      {loss.afterCompaction && <li>{s.afterCompaction}</li>}
    </ul>
  );
}

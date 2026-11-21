---
name: write-pipeline
description: Check a change to context-manager's write pipeline against its TLA+ model. The pipeline is apply, its undo, recovery, revert and the data-directory lock in crates/cm-host. TLC checks every order of steps of two instances, crashes, failed file operations and outside edits. Use before you commit a change to apply.rs, recovery.rs, history.rs or fsio.rs in crates/cm-host, to Host::apply, Host::history, Host::change or Host::with_parts, or to Planner::revert in crates/cm-core/src/edit.rs, and when scripts/check.sh prints "write pipeline model: ... changed since".
---

# Write pipeline model

The write pipeline writes the operator's real agent configuration. Its promise: the tool never writes or deletes an agent file at a time the operator did not approve, a change that failed or never finished is never applied later, and whatever the tool wrote over can be restored from its history.

`formal/write-pipeline/WritePipeline.tla` models that pipeline as a state machine. TLC, the TLA+ model checker, explores every order of the steps of two instances on one data directory, with a crash at any step, a file operation that fails, and the operator editing the same files. Unit tests inject one failure into one process at a time. The model covers interleavings that the tests cannot reach, such as a recovery in one instance between two steps of an apply in another.

## The rules

Each rule is an invariant in the spec. The harm is what the operator loses when it breaks.

| Invariant | Rule | Harm |
|---|---|---|
| `RecoveryWritesNoAgentFile` | Recovery and a history read never change an agent file. | The configuration changes when nobody asked. |
| `NoLateWrite` | The tool writes a change's text only from that change's own apply, while it runs. | An edit that failed, or died with its process, appears later, possibly over newer work. |
| `ReplacedTextKept` | When a file holds text the tool wrote over other text, the replaced text is in the history, in a record that names the file. | The operator's text is gone, and no revert can bring it back. |
| `SealedMeansDone` | A record marked sealed: every one of its files held its after text at some time. | The history says a change is in place when part of it never reached the disk. |
| `AbortedMeansNothing` | A record marked aborted: no file holds text its own apply wrote. | The history says nothing of a change is on disk while part of it is. |
| `UndisturbedApplySeals` | An apply that no crash, no failed operation and no other write of its own files interrupts returns Applied, and its record stays sealed. | A correct edit fails or is misrecorded because a second window of the tool was open. |
| `RevertRestoresAndCloses` | A revert of a record whose files are each at before or at after, with nothing else writing them, returns every file to its before text and leaves the record closed. | The operator cannot undo a change, or it stays marked unfinished. |

`TypeOK` is a sanity check of the model itself. A failure of `TypeOK` is a modelling error.

## Does your change touch it

Each action's comment in the spec has a `code:` line naming the function it models.

| Code | Model actions |
|---|---|
| `host.rs` `Host::apply`, `apply.rs` `Ctx::apply` (`lock.lock()` on `Ctx::lock_file`, the plan, AlreadyApplied, a revert that plans to nothing) | `StartSave`, `StartRevert`, `TakeLock`, `Plan`, `TargetWrite`, `TargetRemove` |
| `edit.rs` `Planner::revert`, `Planner::plan` | `RevertOps`, `Conflicts`, `SaveOps` in `Plan` |
| `apply.rs` `Ctx::journal`, `history.rs` `History::put_blob` | `StoreBlobs` |
| `apply.rs` `Ctx::run` (the pending record), `history.rs` `History::write_pending` | `WritePending` |
| `apply.rs` `Effects::expect` | `CheckHash` |
| `apply.rs` `Effects::write`, `fsio.rs` `atomic_write` | `WriteTemp`, `Rename` |
| `apply.rs` `Effects::write` with `has_other_names`, `fsio.rs` `write_in_place` and its `Failed { changed }` | `Truncate`, `WriteInPlace` |
| `apply.rs` `Effects::trash` | `TrashFile` |
| `apply.rs` `Ctx::run` (after the loop: `unfinished_target`, the seal), `history.rs` `History::seal`, `History::abort`, `History::close` | `AfterOp`, `TargetWrite`, `TargetRemove`, `SealWrite`, `SealRemove` |
| `apply.rs` `Ctx::run` (`failure.changed`), `Ctx::undo`, `Ctx::restore` | `StartUndo`, `UndoClassify`, `UndoTemp`, `UndoRename`, `UndoRemove`, `UndoTruncate`, `UndoInPlace`, `UndoAbortWrite`, `UndoAbortRemove`, `UndoAttention` |
| `recovery.rs` `recover` (`file.try_lock()`), `classify`, `inspect`, `remove_temp_files` | `StartRecovery`, `RecoveryLock`, `RecoveryList`, `RecoveryInspect`, `RecoveryTemps`, `RecoveryDecide`, `RecoverySealWrite`, `RecoverySealRemove`, `RecoveryAbortWrite`, `RecoveryAbortRemove`, `RecoveryAttention` |
| `history.rs` `History::collect_garbage`, `BLOB_MAX_AGE` | `RecoveryGcRead`, `RecoveryGcRemove`, `DayPasses` |
| `history.rs` `History::all`, `History::pending`, `History::detail_of` | `Vis`, `Unfinished`, `RecoveryList`, `BlobsKept` |
| `host.rs` `Host::with_parts`, `Host::recover_now` | `Crash` (the restart runs recovery) |
| `host.rs` `Host::history`, `Host::change` | `StartRecovery` |

If your change touches a row, run the check. If it changes what that code does, mirror the change in the model first (below).

The model does not cover these, so a change to them needs no model change: where a write lands (`fsio.rs` `write_target`, symlinks, and the refusal of a symlink to nothing, which the model sees as a failed write), a read-only host (its recovery writes nothing), directory trash (`Action::TrashDir`, `NotRevertible`), mode bits, parked hooks, the digest comparison that returns Stale, and what an agent's planner puts in a plan.

## Run the check

From the repository root:

```sh
formal/write-pipeline/run.sh
```

A run in which everything passes takes two to four minutes and uses about 230 MB of disk in a temp directory, which it removes when it exits. A configuration stops at its first failure, so a broken model usually shows within a minute. The first run downloads a pinned Java runtime and TLC into `.cache/formal`, which git ignores, and checks their SHA-256. It installs nothing elsewhere. A run is killed when it passes 400 MB or 300 seconds; set `LIMIT_MB` or `LIMIT_S` to change that.

Each line is one configuration:

```text
ok    model                          passes                             1667801 states  104s   224 MB
ok    model in-place                 passes                             1993784 states   76s   229 MB
ok    model in-place-no-crash        passes                               87800 states    5s     1 MB
ok    variant in-place-torn          breaks AbortedMeansNothing           13915 states    6s     0 MB
ok    finding edit-in-window         breaks ReplacedTextKept                587 states    2s     0 MB
```

- A `model` (`WritePipeline.cfg`, `models/*.cfg`) is the pipeline as the code at the sync commit has it. It must pass. It also fails when an action never fires, because a pass then says nothing about that action. A model's `\* not exercised:` line names the actions it leaves to another model. `models/in-place.cfg` gives file `b` a second hard link, so the tool writes `b` in place. `models/in-place-no-crash.cfg` does the same without a crash, so that it can check `AbortedMeansNothing` for a failed in-place write.
- A `variant` (`variants/*.cfg`) mirrors one concrete break of the code, such as the undo restoring nothing. It must break the invariant on its `\* expect:` line. A variant that passes means the model can no longer see that break.
- A `finding` (`findings/*.cfg`) is a case where the code at the sync commit breaks a rule. It must keep failing until the code is fixed. Then move it to `variants/`.

The script exits 0 only when every line says `ok`. `formal/write-pipeline/run.sh model` runs one configuration. `TRACE=1` also prints the trace of each expected failure. `OUT=<dir>` keeps TLC's output of each run in `<dir>`.

## Read a failure

On a `FAIL` line, the script prints the shortest trace as numbered steps. Each step gives the instance (`p1`, `p2`, `operator`, `clock`), the action, its comment, the code it models, and what changed on disk (`disk`, `temps`) and in the history (`pend` for `changes/<id>.pending.json`, `closed` for `changes/<id>.json`, `blobs`). Retell it in the code's words, for example: "p1 renames the temp file over `a`; p2's history read runs `remove_temp_files` on p1's record; p1's next rename fails with ENOENT."

Read every result with the `/statechart` skill as well. Give it `formal/write-pipeline/WritePipeline.tla`, the configuration that failed, and the view `formal/write-pipeline/chart/view.mjs`. Point its output folder at a new temp directory. It reports the shortest path to each rule that breaks, in plain words.

## A real defect or a modelling error

Replay the trace against the code, one step for each action:

1. Open the function on the step's `code:` line. Check that the code can take this step in this state. A step the code cannot take, or two steps that the code does as one, is a modelling error.
2. Write the trace as a test in `crates/cm-host/src/pipeline_tests.rs`. Setting `host.fail_after` to a `Step` stops a run where a crash would (see `Two::crash_after`). A second `Host` over the same fixture data directory is a second instance. Run it with `cargo test -p cm-host`.
3. If the test reproduces the harm, it is a defect. Leave the invariant as it is and add `findings/<name>.cfg` with an `\* expect:` line, so that the check records it. Report it with the trace. Change product code only when the user asks.
4. If the step cannot happen in the code, fix the action and say in the commit what the model got wrong.

Never weaken an invariant or a bound to make the model pass.

## Mirror an intended change

1. Find the rows of the table above for the code you change.
2. Change those actions in `WritePipeline.tla`. One action is one step of the code: one rename, one hash check, one record write. Writing the sealed record and removing the pending one are two steps. Give each new action a comment and a `code:` line, and add it to `Next`.
3. A new way the code could break gets a constant in the spec and a `variants/<name>.cfg` that must fail on the invariant it threatens. A new rule gets an invariant with its sentence and its harm, and a variant that breaks it.
4. Run the check. If an action never fires, the model cannot reach it; fix the model or the bounds. Add an action that only a variant can take to `variant_only` in `run.sh`. An action that one model leaves to another goes on its `\* not exercised:` line.
5. Update the table above and the sync record below.

**When your intended change breaks a rule.** If a `model` line fails after you mirrored the change, the change is unsafe as it is designed. Do not commit it. Report the trace, in the code's words, to whoever asked for the change. A rule changes only when the owner decides that the pipeline's promise changes. Then change the invariant, its sentence and its harm in this file, add a variant that breaks the new rule, and say so in the commit.

## The sync record

`scripts/check.sh` reads the lines below. It prints `write pipeline model: ... changed since <commit>` when a watched file, or a watched function (`fn`), differs from the sync commit, in the working tree too. That line is a notice, not a failure.

```text
sync d87635d
watch crates/cm-host/src/apply.rs
watch crates/cm-host/src/recovery.rs
watch crates/cm-host/src/history.rs
watch crates/cm-host/src/fsio.rs
watch crates/cm-host/src/host.rs fn with_parts
watch crates/cm-host/src/host.rs fn recover_now
watch crates/cm-host/src/host.rs fn apply
watch crates/cm-host/src/host.rs fn history
watch crates/cm-host/src/host.rs fn change
watch crates/cm-core/src/edit.rs fn revert
```

Move `sync` forward only after you have confirmed that the model describes the code at the new commit. Read `git diff <sync> -- <path>` for each watched path, and match each change to an action. Mirror any change in behavior, or confirm that there is none. Run the check, then set `sync` to the commit you read. Commit the model and this file together.

## Bounds and assumptions

A pass covers the model under these bounds, not the whole program:

- Two instances, two files (`a` starts absent, `b` holds `b0`), two change ids, one crash, one failed file operation, and two outside edits. An outside edit writes `x` or checks out the file's starting text.
- Every save writes its own text to both files. A trash comes from reverting the save that created `a`.
- The lock is `flock` on `apply.lock` (`apply.rs` `LOCK_FILE`). The OS releases it when its process dies. Waiting for it and a recovery round that skips it are left out, because neither changes anything.
- Steps the model takes as one: the plan's read of the files, the store of all blobs of a change, the read of the pending records, and, with the lock, the garbage collection.
- No run takes a day between storing its blobs and writing its pending record.
- A failed sync after a complete in-place write is left out. It leaves the new text, which the undo finds at after and restores.
- The models assume that the operator never writes a file between the tool's hash check and its rename. For a file written in place, the window runs to the end of the write, and after a write that failed halfway, to the end of its restore, which `Ctx::undo` runs right after the failure. `findings/edit-in-window.cfg` shows what that window costs.
- `models/in-place.cfg` leaves out `AbortedMeansNothing`, because `findings/in-place-crash-revert.cfg` shows that a crash during an in-place write, then a revert of the unfinished record, breaks it.

The chart for the owner, in Korean, is in `formal/write-pipeline/chart/`. When the model is wrong about the code, the `/maintain-formal-verification-skill` skill brings it back in line.

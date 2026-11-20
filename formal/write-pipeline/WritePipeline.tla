---------------------------- MODULE WritePipeline ----------------------------
(***************************************************************************)
(* The write pipeline of crates/cm-host as a state machine: apply (plan    *)
(* again, journal, execute, seal), the undo of a failed apply, recovery,   *)
(* revert, and the lock on the data directory. Two instances share one     *)
(* data directory, either can crash at any step, a file operation can      *)
(* fail, and the operator edits the same files outside the tool.           *)
(*                                                                         *)
(* Each action names the code it models on its `code:` line. One action is *)
(* one step in the code: one rename, one hash check, one record write.     *)
(* The skill .claude/skills/write-pipeline/SKILL.md says how to run it.    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Procs,            \* instances of the tool on one data directory
    MaxIds,           \* how many change ids `new_change_id` hands out in a run
    MaxCrashes,       \* how many times an instance may die
    MaxFailures,      \* how many file operations may return an error
    MaxOutside,       \* how many times the operator may edit a file
    Lock,             \* TRUE: the data-directory lock of design section 4
    InPlace,          \* files with more than one hard link, written in place
    EditInWindow,     \* TRUE: the operator may edit a file between a hash check and the write
    ChartScript,      \* TRUE: one save, then reverts of it while a change id is left
    \* Broken variants. Each mirrors one concrete break of the code (see variants/).
    RollForward,          \* recovery writes the after text, as before commit 75fa028 (F4)
    ExecuteBeforeJournal, \* step 4 runs before the pending record is written
    SkipUndo,             \* a failed apply aborts its record without restoring any file
    SealBeforeLastOp,     \* the sealed record is written before the last operation
    RevertKeepsTarget     \* a revert never resolves the record it reverts

-----------------------------------------------------------------------------
(* Texts and files *)

Absent == "absent"     \* no file at the path
Torn == "torn"         \* a file an in-place write left half written
FileSeq == <<"a", "b">>
Files == {FileSeq[n] : n \in 1..Len(FileSeq)}
Init == [f \in Files |-> IF f = "b" THEN "b0" ELSE Absent]
OutsideText == "x"
ChangeTexts == <<"c1", "c2", "c3", "c4", "c5">>
Ids == 1..MaxIds
\* The text a save of change c writes. Each change writes its own text.
TextOf(c) == ChangeTexts[c]
BlobTexts == ({Init[f] : f \in Files} \cup {OutsideText, Torn}
                \cup {TextOf(c) : c \in Ids}) \ {Absent}

\* One journal operation (cm_core JournalOp): a file, its text before and its text after.
\* Absent stands for a blob id of None.
Op(f, b, a) == [file |-> f, before |-> b, after |-> a]

-----------------------------------------------------------------------------
VARIABLES
    \* The disk.
    disk,      \* [Files -> text]: the agent files
    temps,     \* set of <<id, file>>: `.cm-<id>-<tag>.tmp` beside a target (apply.rs temp_name)
    blobs,     \* [BlobTexts -> "none" | "young" | "old"]: history/blobs/<sha256>
    pend,      \* [Ids -> "none" | "Pending" | "NeedsAttention"]: changes/<id>.pending.json
    closed,    \* [Ids -> "none" | "Sealed" | "Aborted"]: changes/<id>.json
    journal,   \* [Ids -> seq of Op]: the operations a record of the change holds
    lock,      \* the instance that holds the data-directory lock, or "free"
    nextId,    \* the id `new_change_id` returns next
    \* Each instance.
    pc,        \* [Procs -> step label]
    act,       \* [Procs -> the apply in progress]
    rec,       \* [Procs -> the recovery in progress]
    \* Bounds.
    crashes, failures, outside,
    \* Ghosts: what the properties read. None of them steers a step.
    writer,         \* [Files -> the change whose apply wrote the text now on disk, or 0]
    replaced,       \* [Files -> the text the tool's last write replaced, or "none"]
    planned,        \* [Ids -> set of Op]: the plan of each change
    reached,        \* [Ids -> files that held their planned after text at some time]
    fate,           \* [Ids -> "none" | "running" | "ok" | "err" | "died"]
    dist,           \* [Procs -> an outside write or an injected error hit this apply]
    cleanRun,       \* [Ids -> the apply ran with nothing in `dist`]
    revertBroken,   \* a revert that should have restored and closed did not
    recoveryWrote,  \* recovery wrote an agent file
    lateWrote       \* the tool wrote a change's text outside that change's running apply

diskVars == <<disk, temps>>
histVars == <<blobs, pend, closed, journal>>
procVars == <<pc, act, rec, lock, nextId>>
boundVars == <<crashes, failures, outside>>
writeGhosts == <<writer, replaced, reached, recoveryWrote, lateWrote>>
runGhosts == <<planned, fate, dist, cleanRun, revertBroken>>
vars == <<diskVars, histVars, procVars, boundVars, writeGhosts, runGhosts>>

NoAct == [kind |-> "none", target |-> 0, ops |-> <<>>, id |-> 0,
          i |-> 0, targets |-> {}, rclean |-> FALSE, journaled |-> FALSE,
          early |-> FALSE, done |-> 0, j |-> 0, clean |-> TRUE]
NoRec == [list |-> <<>>, k |-> 0, i |-> 0, states |-> <<>>, refs |-> {}]

\* What `History::all` shows for a change: a closed record hides a pending one.
Vis(c) == IF closed[c] # "none" THEN closed[c] ELSE pend[c]
Unfinished(c) == Vis(c) \in {"Pending", "NeedsAttention"}

\* recovery.rs classify: by the text on disk, at after first, then at before.
Classify(op) == IF disk[op.file] = op.after THEN "after"
                ELSE IF disk[op.file] = op.before THEN "before"
                ELSE "other"

ApplyPcs == {"a_plan", "a_blobs", "a_pending", "a_check", "a_temp",
             "a_rename", "a_trunc", "a_inplace", "a_trash", "a_target_write",
             "a_target_remove", "a_seal_write", "a_seal_remove", "a_return",
             "u_classify", "u_temp", "u_rename", "u_trunc", "u_inplace",
             "u_remove", "u_abort_write", "u_abort_remove", "u_attention"}
RecoveryPcs == {"r_lock", "r_list", "r_inspect", "r_temps", "r_decide",
                "r_seal_write", "r_seal_remove", "r_abort_write",
                "r_abort_remove", "r_attention", "r_roll", "r_gc_read",
                "r_gc_remove"}

CurOp(p) == act[p].ops[act[p].i]
UndoOp(p) == act[p].ops[act[p].j]

\* The operator's edit of f would land between the tool's hash check of f and its
\* write of f, or between the undo's classification of f and its restore.
InWindow(f) ==
    \E p \in Procs :
        \/ /\ pc[p] \in {"a_temp", "a_rename", "a_trunc", "a_inplace", "a_trash"}
           /\ CurOp(p).file = f
        \/ /\ pc[p] \in {"u_temp", "u_rename", "u_trunc", "u_inplace", "u_remove"}
           /\ UndoOp(p).file = f

-----------------------------------------------------------------------------
(* Helpers shared by the actions *)

\* Each file f whose planned after text is t has reached it.
Observe(f, t) ==
    [c \in Ids |-> IF \E op \in planned[c] : op.file = f /\ op.after = t
                   THEN reached[c] \cup {f} ELSE reached[c]]

\* Every change the tool makes to an agent file goes through ToolWrite.
\* why: "do" writes the change's own text, "undo" puts a before text back.
\* cont: the second half of an in-place write, which continues the first.
ToolWrite(p, f, t, c, why, cont) ==
    /\ disk' = [disk EXCEPT ![f] = t]
    /\ writer' = [writer EXCEPT ![f] = IF why = "do" THEN c ELSE 0]
    /\ replaced' = [replaced EXCEPT ![f] = IF cont THEN @ ELSE disk[f]]
    /\ reached' = Observe(f, t)
    /\ recoveryWrote' = (recoveryWrote \/ pc[p] \in RecoveryPcs)
    /\ lateWrote' = (lateWrote \/ (why = "do" /\ ~(pc[p] \in ApplyPcs
                                                  /\ act[p].id = c
                                                  /\ fate[c] = "running")))
    /\ dist' = [q \in Procs |-> dist[q] \/ (q # p /\ f \in act[q].targets)]

Goto(p, label) == pc' = [pc EXCEPT ![p] = label]
SetAct(p, a) == act' = [act EXCEPT ![p] = a]

\* An injected error in this step of p's apply.
Fail(p) ==
    /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ dist' = [dist EXCEPT ![p] = TRUE]

\* A revert that returns now should have restored every file of its record and left
\* the record closed: every file was at before or at after when it planned (rclean),
\* and no other write and no injected error hit it. The return step changes neither
\* the disk nor the record it reverts, so the current state is the state it leaves.
RevertFellShort(p, failedNow, rclean) ==
    LET r == act[p].target
    IN /\ act[p].kind = "revert"
       /\ rclean /\ ~dist[p] /\ ~failedNow
       /\ ~(/\ \A n \in 1..Len(journal[r]) :
                 disk[journal[r][n].file] = journal[r][n].before
            /\ Vis(r) \in {"Sealed", "Aborted"})

\* The apply returns to its caller: `result` is "ok", "err" or "none" (a plan that
\* writes nothing: AlreadyApplied, Stale, or an error of the plan).
ReturnAt(p, result, failedNow, rclean) ==
    LET c == act[p].id
    IN /\ fate' = IF c # 0 THEN [fate EXCEPT ![c] = result] ELSE fate
       /\ cleanRun' = IF c # 0 THEN [cleanRun EXCEPT ![c] = ~dist[p] /\ ~failedNow]
                      ELSE cleanRun
       /\ revertBroken' = (revertBroken \/ RevertFellShort(p, failedNow, rclean))
       /\ dist' = [dist EXCEPT ![p] = FALSE]
       /\ lock' = IF lock = p THEN "free" ELSE lock
       /\ Goto(p, "idle")
       /\ SetAct(p, NoAct)

Return(p, result, failedNow) == ReturnAt(p, result, failedNow, act[p].rclean)

ReturnFailed(p) ==
    /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ Return(p, "err", TRUE)

\* Where an apply goes once operation act[p].i is done.
\*   code: crates/cm-host/src/apply.rs Ctx::run, the loop over the operations, then
\*   `unfinished_target` and the seal
AfterOp(p) ==
    LET a == act[p]
        n == Len(a.ops)
    IN IF a.i < n
       THEN IF SealBeforeLastOp /\ a.i + 1 = n
            THEN /\ Goto(p, "a_seal_write")
                 /\ SetAct(p, [a EXCEPT !.i = a.i + 1, !.early = TRUE])
            ELSE /\ Goto(p, "a_check")
                 /\ SetAct(p, [a EXCEPT !.i = a.i + 1])
       ELSE IF ~a.journaled
       THEN /\ Goto(p, "a_blobs")
            /\ SetAct(p, [a EXCEPT !.i = n + 1])
       ELSE IF a.early
       THEN /\ Goto(p, "a_return")
            /\ SetAct(p, [a EXCEPT !.i = n + 1])
       ELSE /\ Goto(p, IF a.kind = "revert" /\ Unfinished(a.target) /\ ~RevertKeepsTarget
                       THEN "a_target_write" ELSE "a_seal_write")
            /\ SetAct(p, [a EXCEPT !.i = n + 1])

\* The first step of the undo after operation act[p].i failed.
\*   code: crates/cm-host/src/apply.rs Ctx::run, `self.undo(&mut record, done, ends[index])`
StartUndo(p) ==
    /\ Goto(p, "u_classify")
    /\ SetAct(p, [act[p] EXCEPT !.done = act[p].i - 1, !.j = act[p].i, !.clean = TRUE])

\* The undo moves to the operation before act[p].j, or closes the record.
\*   code: crates/cm-host/src/apply.rs Ctx::undo
UndoNext(p, ok) ==
    LET a == act[p]
        clean == a.clean /\ ok
    IN IF a.j > 1
       THEN /\ Goto(p, "u_classify")
            /\ SetAct(p, [a EXCEPT !.j = a.j - 1, !.clean = clean])
       ELSE /\ Goto(p, IF clean THEN "u_abort_write" ELSE "u_attention")
            /\ SetAct(p, [a EXCEPT !.j = 0, !.clean = clean])

-----------------------------------------------------------------------------
(* The operator and the clock *)

\* The operator writes a file outside the tool: a new text, or a checkout of the text
\* the file held at the start (Absent deletes it).
OutsideEdit(f, t) ==
    /\ outside < MaxOutside
    /\ t # disk[f]
    /\ EditInWindow \/ ~InWindow(f)
    /\ outside' = outside + 1
    /\ disk' = [disk EXCEPT ![f] = t]
    /\ writer' = [writer EXCEPT ![f] = 0]
    /\ replaced' = [replaced EXCEPT ![f] = "none"]
    /\ reached' = Observe(f, t)
    /\ dist' = [q \in Procs |-> dist[q] \/ f \in act[q].targets]
    /\ UNCHANGED <<temps, histVars, procVars, crashes, failures,
                   recoveryWrote, lateWrote, planned, fate, cleanRun, revertBroken>>

\* A day passes: every blob becomes old enough for the collector.
\* Assumption: no run stalls a day between storing its blobs and writing its record.
\*   code: crates/cm-host/src/history.rs BLOB_MAX_AGE
DayPasses ==
    /\ \E t \in BlobTexts : blobs[t] = "young"
    /\ \A p \in Procs : pc[p] # "a_pending"
    /\ blobs' = [t \in BlobTexts |-> IF blobs[t] = "young" THEN "old" ELSE blobs[t]]
    /\ UNCHANGED <<diskVars, pend, closed, journal, procVars, boundVars,
                   writeGhosts, runGhosts>>

-----------------------------------------------------------------------------
(* An instance dies and starts again *)

\* The process dies at any step. Its memory is gone, its files stay, and the OS
\* releases its lock. The new process runs recovery before it serves anything.
\*   code: crates/cm-host/src/host.rs Host::with_parts, `host.recover_now()`
Crash(p) ==
    /\ crashes < MaxCrashes
    /\ pc[p] # "idle"
    /\ crashes' = crashes + 1
    /\ fate' = IF act[p].id # 0 /\ fate[act[p].id] = "running"
               THEN [fate EXCEPT ![act[p].id] = "died"] ELSE fate
    /\ lock' = IF lock = p THEN "free" ELSE lock
    /\ Goto(p, IF Lock THEN "r_lock" ELSE "r_list")
    /\ SetAct(p, NoAct)
    /\ rec' = [rec EXCEPT ![p] = NoRec]
    /\ dist' = [dist EXCEPT ![p] = FALSE]
    /\ UNCHANGED <<diskVars, histVars, nextId, failures, outside, writeGhosts,
                   planned, cleanRun, revertBroken>>

-----------------------------------------------------------------------------
(* Apply: design section 4, steps 2 to 5 *)

\* apply holds the lock from step 2 to step 5, and waits while another instance holds
\* it. An instance that waits does nothing else, so the model starts the apply at the
\* moment it gets the lock.
\*   code: design section 4, "The apply pipeline" (the lock on a file in the data directory)
TakeLock(p) ==
    IF Lock THEN lock = "free" /\ lock' = p ELSE UNCHANGED lock

\* The operator applies a save that writes the change's own text to both files. File
\* a starts absent, so the first save creates it, and a revert of that save trashes it.
\*   code: crates/cm-host/src/host.rs Host::apply, then apply.rs Ctx::apply
StartSave(p) ==
    /\ pc[p] = "idle"
    /\ ChartScript => nextId = 1
    /\ TakeLock(p)
    /\ SetAct(p, [NoAct EXCEPT !.kind = "save"])
    /\ Goto(p, "a_plan")
    /\ UNCHANGED <<diskVars, histVars, rec, nextId, boundVars, writeGhosts, runGhosts>>

\* The operator reverts a change the history lists.
\*   code: crates/cm-host/src/host.rs Host::apply with Edit::Revert
StartRevert(p) ==
    /\ pc[p] = "idle"
    /\ \E r \in Ids :
         /\ Vis(r) # "none"
         /\ ChartScript => r = 1 /\ nextId <= MaxIds
         /\ SetAct(p, [NoAct EXCEPT !.kind = "revert", !.target = r])
    /\ TakeLock(p)
    /\ Goto(p, "a_plan")
    /\ UNCHANGED <<diskVars, histVars, rec, nextId, boundVars, writeGhosts, runGhosts>>

\* The operations of a save, in file order: each file not already at its new text.
\*   code: crates/cm-core/src/edit.rs Planner::plan
SaveOps ==
    LET fs == SelectSeq(FileSeq, LAMBDA f : disk[f] # TextOf(nextId))
    IN [n \in 1..Len(fs) |-> Op(fs[n], disk[fs[n]], TextOf(nextId))]

\* The operations of a revert of r: each file at its after text goes back to its
\* before text. A file at before is skipped. A file at neither is a conflict.
\*   code: crates/cm-core/src/edit.rs Planner::revert
RevertOps(r) ==
    LET sel == SelectSeq(journal[r], LAMBDA op : disk[op.file] = op.after)
    IN [n \in 1..Len(sel) |-> Op(sel[n].file, sel[n].after, sel[n].before)]
Conflicts(r) ==
    {n \in 1..Len(journal[r]) : Classify(journal[r][n]) = "other"}
\* History::detail_of fails when a blob of the record is missing.
BlobsKept(r) ==
    \A n \in 1..Len(journal[r]) :
        /\ journal[r][n].before = Absent \/ blobs[journal[r][n].before] # "none"
        /\ journal[r][n].after = Absent \/ blobs[journal[r][n].after] # "none"

\* A plan with operations gets a change id and starts the journal.
Begin(p, ops, targets, rclean) ==
    /\ nextId' = nextId + 1
    /\ planned' = [planned EXCEPT ![nextId] = {ops[n] : n \in 1..Len(ops)}]
    /\ fate' = [fate EXCEPT ![nextId] = "running"]
    /\ SetAct(p, [act[p] EXCEPT !.ops = ops, !.id = nextId, !.i = 1,
                               !.targets = targets, !.rclean = rclean])
    /\ Goto(p, IF ExecuteBeforeJournal THEN "a_check" ELSE "a_blobs")
    /\ UNCHANGED <<cleanRun, revertBroken, dist, lock>>

\* Step 2: plan again against the disk as it is now. An empty plan is
\* AlreadyApplied. (The digest check, which returns Stale, is not modelled: a
\* Stale result writes nothing, like an apply that never started.)
\*   code: crates/cm-host/src/apply.rs Ctx::apply
Plan(p) ==
    /\ pc[p] = "a_plan"
    /\ LET a == act[p] IN
       IF a.kind = "save"
       THEN LET ops == SaveOps IN
            IF Len(ops) = 0 \/ nextId > MaxIds
            THEN Return(p, "none", FALSE) /\ UNCHANGED <<nextId, planned>>
            ELSE Begin(p, ops, {ops[n].file : n \in 1..Len(ops)}, FALSE)
       ELSE LET r == a.target
                ops == RevertOps(r)
                files == {journal[r][n].file : n \in 1..Len(journal[r])}
                clean == Conflicts(r) = {}
            IN IF ~BlobsKept(r)
               \* code: History::detail_of returns an error, and so does the plan
               THEN ReturnAt(p, "none", FALSE, clean) /\ UNCHANGED <<nextId, planned>>
               ELSE IF ~Unfinished(r) /\ ~clean
               \* code: a revert of a closed change with a conflict fails as a Conflict
               THEN ReturnAt(p, "none", FALSE, clean) /\ UNCHANGED <<nextId, planned>>
               ELSE IF Len(ops) = 0
               THEN IF Unfinished(r) /\ ~RevertKeepsTarget
                    \* code: "Such a revert resolves the record also when it plans to nothing."
                    THEN /\ Goto(p, "a_target_write")
                         /\ SetAct(p, [a EXCEPT !.targets = files, !.rclean = clean])
                         /\ UNCHANGED <<nextId, planned, fate, cleanRun, revertBroken, dist, lock>>
                    ELSE ReturnAt(p, "none", FALSE, clean) /\ UNCHANGED <<nextId, planned>>
               \* The model ran out of change ids: the apply ends with nothing written.
               ELSE IF nextId > MaxIds
               THEN ReturnAt(p, "none", FALSE, FALSE) /\ UNCHANGED <<nextId, planned>>
               ELSE Begin(p, ops, files, clean)
    /\ UNCHANGED <<diskVars, histVars, rec, boundVars, writeGhosts>>

\* Step 3a: store the before and after text of each operation as blobs. A blob that
\* exists is left as it is, also its age.
\*   code: crates/cm-host/src/apply.rs Ctx::journal, history.rs History::put_blob
StoreBlobs(p) ==
    /\ pc[p] = "a_blobs"
    /\ LET ops == act[p].ops
           ts == {ops[n].before : n \in 1..Len(ops)} \cup {ops[n].after : n \in 1..Len(ops)}
       IN blobs' = [t \in BlobTexts |-> IF t \in ts /\ blobs[t] = "none" THEN "young"
                                        ELSE blobs[t]]
    /\ Goto(p, "a_pending")
    /\ UNCHANGED <<diskVars, pend, closed, journal, act, rec, lock, nextId, boundVars,
                   writeGhosts, runGhosts>>

\* Step 3b: write changes/<id>.pending.json.
\*   code: crates/cm-host/src/apply.rs Ctx::run, history.rs History::write_pending
WritePending(p) ==
    /\ pc[p] = "a_pending"
    /\ LET a == act[p]
           c == a.id
       IN \/ /\ pend' = [pend EXCEPT ![c] = "Pending"]
             /\ journal' = [journal EXCEPT ![c] = a.ops]
             /\ IF a.i > Len(a.ops)   \* ExecuteBeforeJournal: every operation already ran
                THEN /\ Goto(p, "a_seal_write")
                     /\ SetAct(p, [a EXCEPT !.journaled = TRUE])
                ELSE /\ Goto(p, "a_check")
                     /\ SetAct(p, [a EXCEPT !.journaled = TRUE])
             /\ UNCHANGED <<failures, fate, cleanRun, revertBroken, dist, lock>>
          \/ /\ ReturnFailed(p)
             /\ UNCHANGED <<pend, journal>>
    /\ UNCHANGED <<diskVars, blobs, closed, rec, nextId, crashes, outside, writeGhosts,
                   planned>>

\* Step 4: hash the target again right before the write. A mismatch is a Conflict,
\* and the run undoes what it did.
\*   code: crates/cm-host/src/apply.rs Effects::expect
CheckHash(p) ==
    /\ pc[p] = "a_check"
    /\ LET op == CurOp(p) IN
       IF disk[op.file] = op.before
       THEN /\ Goto(p, IF op.after = Absent THEN "a_trash"
                       ELSE IF op.file \in InPlace /\ disk[op.file] # Absent THEN "a_trunc"
                       ELSE "a_temp")
            /\ UNCHANGED act
       ELSE StartUndo(p)
    /\ UNCHANGED <<diskVars, histVars, rec, lock, nextId, boundVars, writeGhosts, runGhosts>>

\* Step 4, Write: create, write and sync the temp file beside the target.
\*   code: crates/cm-host/src/apply.rs Effects::write, fsio.rs atomic_write up to the rename
WriteTemp(p) ==
    /\ pc[p] = "a_temp"
    /\ \/ /\ temps' = temps \cup {<<act[p].id, CurOp(p).file>>}
          /\ Goto(p, "a_rename")
          /\ UNCHANGED <<act, failures, dist>>
       \/ /\ Fail(p)           \* atomic_write removes the temp file on failure
          /\ StartUndo(p)
          /\ UNCHANGED temps
    /\ UNCHANGED <<disk, histVars, rec, lock, nextId, crashes, outside, writeGhosts,
                   planned, fate, cleanRun, revertBroken>>

\* Step 4, Write: rename the temp file over the target. It fails when the temp file
\* is gone, and the run undoes what it did.
\*   code: crates/cm-host/src/fsio.rs atomic_write, `fs::rename(tmp, target)`
Rename(p) ==
    /\ pc[p] = "a_rename"
    /\ LET op == CurOp(p)
           tmp == <<act[p].id, op.file>>
       IN IF tmp \in temps
          THEN \/ /\ ToolWrite(p, op.file, op.after, act[p].id, "do", FALSE)
                  /\ temps' = temps \ {tmp}
                  /\ AfterOp(p)
                  /\ UNCHANGED failures
               \/ /\ Fail(p)
                  /\ temps' = temps \ {tmp}
                  /\ StartUndo(p)
                  /\ UNCHANGED <<disk, writeGhosts>>
          ELSE /\ StartUndo(p)      \* ENOENT: someone removed the temp file
               /\ UNCHANGED <<diskVars, writeGhosts, failures, dist>>
    /\ UNCHANGED <<histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* Step 4, Write of a file with more than one hard link: open it truncated ...
\*   code: design section 4, step 4 ("written in place instead")
Truncate(p) ==
    /\ pc[p] = "a_trunc"
    /\ ToolWrite(p, CurOp(p).file, Torn, act[p].id, "do", FALSE)
    /\ Goto(p, "a_inplace")
    /\ UNCHANGED <<temps, histVars, act, rec, lock, nextId, boundVars, planned, fate,
                   cleanRun, revertBroken>>

\* ... then write the new text into it. A failure leaves the file half written.
\*   code: design section 4, step 4 ("written in place instead")
WriteInPlace(p) ==
    /\ pc[p] = "a_inplace"
    /\ \/ /\ ToolWrite(p, CurOp(p).file, CurOp(p).after, act[p].id, "do", TRUE)
          /\ AfterOp(p)
          /\ UNCHANGED failures
       \/ /\ Fail(p)
          /\ StartUndo(p)
          /\ UNCHANGED <<disk, writeGhosts>>
    /\ UNCHANGED <<temps, histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* Step 4, Trash: move the file to the OS trash (or remove it when the history holds it).
\*   code: crates/cm-host/src/apply.rs Effects::trash
TrashFile(p) ==
    /\ pc[p] = "a_trash"
    /\ \/ /\ ToolWrite(p, CurOp(p).file, Absent, act[p].id, "do", FALSE)
          /\ AfterOp(p)
          /\ UNCHANGED failures
       \/ /\ Fail(p)
          /\ StartUndo(p)
          /\ UNCHANGED <<disk, writeGhosts>>
    /\ UNCHANGED <<temps, histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* A revert of an unfinished record resolves it: write changes/<target>.json as Aborted ...
\*   code: crates/cm-host/src/apply.rs Ctx::apply and Ctx::run, History::abort,
\*   history.rs History::close, `self.write_record(record, false)`
TargetWrite(p) ==
    /\ pc[p] = "a_target_write"
    /\ \/ /\ closed' = [closed EXCEPT ![act[p].target] = "Aborted"]
          /\ Goto(p, "a_target_remove")
          /\ UNCHANGED <<failures, fate, cleanRun, revertBroken, dist, lock, act>>
       \/ /\ ReturnFailed(p)
          /\ UNCHANGED closed
    /\ UNCHANGED <<diskVars, blobs, pend, journal, rec, nextId, crashes, outside,
                   writeGhosts, planned>>

\* ... and remove changes/<target>.pending.json.
\*   code: crates/cm-host/src/history.rs History::close, `fs::remove_file`
TargetRemove(p) ==
    /\ pc[p] = "a_target_remove"
    /\ pend' = [pend EXCEPT ![act[p].target] = "none"]
    /\ IF act[p].id = 0     \* the revert planned to nothing: AlreadyApplied
       THEN Return(p, "none", FALSE)
       ELSE /\ Goto(p, "a_seal_write")
            /\ UNCHANGED <<act, fate, cleanRun, revertBroken, dist, lock>>
    /\ UNCHANGED <<diskVars, blobs, closed, journal, rec, nextId, boundVars, writeGhosts,
                   planned>>

\* Step 5: write changes/<id>.json as Sealed ...
\*   code: crates/cm-host/src/apply.rs Ctx::run, history.rs History::seal and History::close
SealWrite(p) ==
    /\ pc[p] = "a_seal_write"
    /\ \/ /\ closed' = [closed EXCEPT ![act[p].id] = "Sealed"]
          /\ Goto(p, "a_seal_remove")
          /\ UNCHANGED <<failures, fate, cleanRun, revertBroken, dist, lock, act>>
       \/ /\ ReturnFailed(p)      \* `self.history.seal(..).map_err(io_detail)?`
          /\ UNCHANGED closed
    /\ UNCHANGED <<diskVars, blobs, pend, journal, rec, nextId, crashes, outside,
                   writeGhosts, planned>>

\* ... and remove changes/<id>.pending.json. The apply returns Applied.
\*   code: crates/cm-host/src/history.rs History::close, `fs::remove_file`
SealRemove(p) ==
    /\ pc[p] = "a_seal_remove"
    /\ pend' = [pend EXCEPT ![act[p].id] = "none"]
    /\ IF act[p].early     \* SealBeforeLastOp: the last operation is still to run
       THEN /\ Goto(p, "a_check")
            /\ UNCHANGED <<act, fate, cleanRun, revertBroken, dist, lock>>
       ELSE Return(p, "ok", FALSE)
    /\ UNCHANGED <<diskVars, blobs, closed, journal, rec, nextId, boundVars, writeGhosts,
                   planned>>

\* Only in the variant SealBeforeLastOp: the apply returns after its last operation.
ApplyReturn(p) ==
    /\ pc[p] = "a_return"
    /\ Return(p, "ok", FALSE)
    /\ UNCHANGED <<diskVars, histVars, rec, nextId, boundVars, writeGhosts, planned>>

-----------------------------------------------------------------------------
(* The undo of a failed apply: design section 4, "A failed apply is not a crash" *)

\* Classify the file of operation j, last first. At after: restore it. At before:
\* nothing to do. At neither: someone else wrote it, unless it is the operation that
\* failed, which did not write it.
\*   code: crates/cm-host/src/apply.rs Ctx::undo, recovery.rs classify
UndoClassify(p) ==
    /\ pc[p] = "u_classify"
    /\ LET op == UndoOp(p)
           s == Classify(op)
       IN IF s = "after" /\ ~SkipUndo
          THEN /\ Goto(p, IF op.before = Absent THEN "u_remove"
                          ELSE IF op.file \in InPlace /\ disk[op.file] # Absent THEN "u_trunc"
                          ELSE "u_temp")
               /\ UNCHANGED act
          ELSE UndoNext(p, s # "other" \/ act[p].j > act[p].done)
    /\ UNCHANGED <<diskVars, histVars, rec, lock, nextId, boundVars, writeGhosts, runGhosts>>

\* Restore a before text: the temp file ...
\*   code: crates/cm-host/src/apply.rs Ctx::restore, Effects::write, fsio.rs atomic_write
UndoTemp(p) ==
    /\ pc[p] = "u_temp"
    /\ \/ /\ temps' = temps \cup {<<act[p].id, UndoOp(p).file>>}
          /\ Goto(p, "u_rename")
          /\ UNCHANGED <<act, failures, dist>>
       \/ /\ Fail(p)
          /\ UndoNext(p, FALSE)
          /\ UNCHANGED temps
    /\ UNCHANGED <<disk, histVars, rec, lock, nextId, crashes, outside, writeGhosts,
                   planned, fate, cleanRun, revertBroken>>

\* ... and the rename.
\*   code: crates/cm-host/src/fsio.rs atomic_write, `fs::rename(tmp, target)`
UndoRename(p) ==
    /\ pc[p] = "u_rename"
    /\ LET op == UndoOp(p)
           tmp == <<act[p].id, op.file>>
       IN IF tmp \in temps
          THEN \/ /\ ToolWrite(p, op.file, op.before, act[p].id, "undo", FALSE)
                  /\ temps' = temps \ {tmp}
                  /\ UndoNext(p, TRUE)
                  /\ UNCHANGED failures
               \/ /\ Fail(p)
                  /\ temps' = temps \ {tmp}
                  /\ UndoNext(p, FALSE)
                  /\ UNCHANGED <<disk, writeGhosts>>
          ELSE /\ UndoNext(p, FALSE)
               /\ UNCHANGED <<diskVars, writeGhosts, failures, dist>>
    /\ UNCHANGED <<histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* Restore a file with more than one hard link in place: truncate ...
\*   code: design section 4, step 4 ("written in place instead")
UndoTruncate(p) ==
    /\ pc[p] = "u_trunc"
    /\ ToolWrite(p, UndoOp(p).file, Torn, act[p].id, "undo", FALSE)
    /\ Goto(p, "u_inplace")
    /\ UNCHANGED <<temps, histVars, act, rec, lock, nextId, boundVars, planned, fate,
                   cleanRun, revertBroken>>

\* ... and write.
UndoInPlace(p) ==
    /\ pc[p] = "u_inplace"
    /\ \/ /\ ToolWrite(p, UndoOp(p).file, UndoOp(p).before, act[p].id, "undo", TRUE)
          /\ UndoNext(p, TRUE)
          /\ UNCHANGED failures
       \/ /\ Fail(p)
          /\ UndoNext(p, FALSE)
          /\ UNCHANGED <<disk, writeGhosts>>
    /\ UNCHANGED <<temps, histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* Restore a file the change created: remove it.
\*   code: crates/cm-host/src/apply.rs Ctx::restore, `fs::remove_file(write_target(..))`
UndoRemove(p) ==
    /\ pc[p] = "u_remove"
    /\ \/ /\ ToolWrite(p, UndoOp(p).file, Absent, act[p].id, "undo", FALSE)
          /\ UndoNext(p, TRUE)
          /\ UNCHANGED failures
       \/ /\ Fail(p)
          /\ UndoNext(p, FALSE)
          /\ UNCHANGED <<disk, writeGhosts>>
    /\ UNCHANGED <<temps, histVars, rec, lock, nextId, crashes, outside, planned, fate,
                   cleanRun, revertBroken>>

\* Every file is back: write changes/<id>.json as Aborted ...
\*   code: crates/cm-host/src/apply.rs Ctx::undo, `self.history.abort(record)`
UndoAbortWrite(p) ==
    /\ pc[p] = "u_abort_write"
    /\ \/ /\ closed' = [closed EXCEPT ![act[p].id] = "Aborted"]
          \* The record holds the operations (the same ones, unless ExecuteBeforeJournal).
          /\ journal' = [journal EXCEPT ![act[p].id] = act[p].ops]
          /\ Goto(p, "u_abort_remove")
          /\ UNCHANGED <<failures, dist>>
       \/ /\ Fail(p)
          /\ Goto(p, "u_attention")
          /\ UNCHANGED <<closed, journal>>
    /\ UNCHANGED <<diskVars, blobs, pend, act, rec, lock, nextId, crashes,
                   outside, writeGhosts, planned, fate, cleanRun, revertBroken>>

\* ... remove the pending record, and return the error.
\*   code: crates/cm-host/src/history.rs History::close, `fs::remove_file`
UndoAbortRemove(p) ==
    /\ pc[p] = "u_abort_remove"
    /\ pend' = [pend EXCEPT ![act[p].id] = "none"]
    /\ Return(p, "err", FALSE)
    /\ UNCHANGED <<diskVars, blobs, closed, journal, rec, nextId, boundVars, writeGhosts,
                   planned>>

\* A file could not be restored: the record needs attention, and the error returns.
\*   code: crates/cm-host/src/apply.rs Ctx::undo, `self.history.write_pending(record)`
UndoAttention(p) ==
    /\ pc[p] = "u_attention"
    /\ pend' = [pend EXCEPT ![act[p].id] = "NeedsAttention"]
    /\ journal' = [journal EXCEPT ![act[p].id] = act[p].ops]
    /\ Return(p, "err", FALSE)
    /\ UNCHANGED <<diskVars, blobs, closed, rec, nextId, boundVars, writeGhosts, planned>>

-----------------------------------------------------------------------------
(* Recovery: runs when an instance starts and before each history read *)

\* An idle instance reads the history (or the change detail), which runs recovery
\* first. Recovery takes the lock without waiting; a round that finds it held does
\* nothing, so the model leaves that round out.
\*   code: crates/cm-host/src/host.rs Host::history and Host::change
StartRecovery(p) ==
    /\ pc[p] = "idle"
    /\ TakeLock(p)
    /\ Goto(p, "r_list")
    /\ UNCHANGED <<diskVars, histVars, act, rec, nextId, boundVars, writeGhosts,
                   runGhosts>>

\* After a restart, recovery takes the lock without waiting. When another instance
\* holds it, that instance is applying a change, and recovery skips this round.
\*   code: design section 4, "The apply pipeline" ("Recovery takes the same lock without waiting")
RecoveryLock(p) ==
    /\ pc[p] = "r_lock"
    /\ IF lock = "free"
       THEN lock' = p /\ Goto(p, "r_list")
       ELSE UNCHANGED lock /\ Goto(p, "idle")
    /\ UNCHANGED <<diskVars, histVars, act, rec, nextId, boundVars, writeGhosts, runGhosts>>

\* The end of a recovery round.
RecoveryDone(p) ==
    /\ Goto(p, "idle")
    /\ rec' = [rec EXCEPT ![p] = NoRec]
    /\ lock' = IF lock = p THEN "free" ELSE lock

\* List the records not yet closed, oldest first, with the state each file says.
\*   code: crates/cm-host/src/recovery.rs recover, history.rs History::pending
RecoveryList(p) ==
    /\ pc[p] = "r_list"
    /\ LET ids == SelectSeq([n \in 1..MaxIds |-> n], LAMBDA c : Unfinished(c))
           list == [n \in 1..Len(ids) |-> [id |-> ids[n], state |-> Vis(ids[n])]]
       IN IF Len(list) = 0
          THEN Goto(p, "r_gc_read") /\ UNCHANGED rec
          ELSE /\ Goto(p, "r_inspect")
               /\ rec' = [rec EXCEPT ![p] = [NoRec EXCEPT !.list = list, !.k = 1, !.i = 1]]
    /\ UNCHANGED <<diskVars, histVars, act, lock, nextId, boundVars, writeGhosts, runGhosts>>

RecId(p) == rec[p].list[rec[p].k].id
RecOps(p) == journal[RecId(p)]

\* Classify one operation of the record by the hash on disk.
\*   code: crates/cm-host/src/recovery.rs inspect, classify
RecoveryInspect(p) ==
    /\ pc[p] = "r_inspect"
    /\ LET r == rec[p]
           s == Classify(RecOps(p)[r.i])
       IN /\ rec' = [rec EXCEPT ![p] = [r EXCEPT !.states = Append(r.states, s),
                                               !.i = r.i + 1]]
          /\ Goto(p, IF r.i < Len(RecOps(p)) THEN "r_inspect" ELSE "r_temps")
    /\ UNCHANGED <<diskVars, histVars, act, lock, nextId, boundVars, writeGhosts, runGhosts>>

\* Remove the temp files of this change beside its targets.
\*   code: crates/cm-host/src/recovery.rs remove_temp_files
RecoveryTemps(p) ==
    /\ pc[p] = "r_temps"
    /\ temps' = {t \in temps : t[1] # RecId(p)}
    /\ Goto(p, "r_decide")
    /\ UNCHANGED <<disk, histVars, act, rec, lock, nextId, boundVars, writeGhosts, runGhosts>>

\* The next record, or the garbage collection.
RecoveryNext(p) ==
    IF rec[p].k < Len(rec[p].list)
    THEN /\ Goto(p, "r_inspect")
         /\ rec' = [rec EXCEPT ![p] = [rec[p] EXCEPT !.k = @ + 1, !.i = 1, !.states = <<>>]]
    ELSE /\ Goto(p, "r_gc_read")
         /\ rec' = [rec EXCEPT ![p] = [NoRec EXCEPT !.list = rec[p].list]]

\* All at after: seal. All at before: abort. Anything else: needs attention.
\*   code: crates/cm-host/src/recovery.rs recover
RecoveryDecide(p) ==
    /\ pc[p] = "r_decide"
    /\ LET st == rec[p].states
           all(s) == \A n \in 1..Len(st) : st[n] = s
           others == \E n \in 1..Len(st) : st[n] = "other"
       IN IF RollForward /\ ~others
          THEN /\ Goto(p, "r_roll")
               /\ rec' = [rec EXCEPT ![p] = [rec[p] EXCEPT !.i = 1]]
          ELSE IF all("after")
          THEN Goto(p, "r_seal_write") /\ UNCHANGED rec
          ELSE IF all("before")
          THEN Goto(p, "r_abort_write") /\ UNCHANGED rec
          ELSE IF rec[p].list[rec[p].k].state # "NeedsAttention"
          THEN Goto(p, "r_attention") /\ UNCHANGED rec
          ELSE RecoveryNext(p)
    /\ UNCHANGED <<diskVars, histVars, act, lock, nextId, boundVars, writeGhosts, runGhosts>>

\* Only in the variant RollForward: write the after text of each operation still at
\* before, from its blob, as the code did before commit 75fa028.
\*   code: crates/cm-host/src/recovery.rs roll_record_forward (removed in 75fa028)
RecoveryRoll(p) ==
    /\ pc[p] = "r_roll"
    /\ LET op == RecOps(p)[rec[p].i]
       IN IF Classify(op) = "before"
          THEN ToolWrite(p, op.file, op.after, RecId(p), "do", FALSE)
          ELSE UNCHANGED <<disk, writeGhosts, dist>>
    /\ IF rec[p].i < Len(RecOps(p))
       THEN /\ rec' = [rec EXCEPT ![p] = [rec[p] EXCEPT !.i = @ + 1]]
            /\ Goto(p, "r_roll")
       ELSE Goto(p, "r_seal_write") /\ UNCHANGED rec
    /\ UNCHANGED <<temps, histVars, act, lock, nextId, boundVars, planned, fate,
                   cleanRun, revertBroken>>

\* Write changes/<id>.json as `state`, and go to `then`.
RecClose(p, state, then) ==
    /\ closed' = [closed EXCEPT ![RecId(p)] = state]
    /\ Goto(p, then)
    /\ UNCHANGED <<diskVars, blobs, pend, journal, act, rec, lock, nextId, boundVars,
                   writeGhosts, runGhosts>>

\* Remove changes/<id>.pending.json, and go to the next record.
RecRemovePending(p) ==
    /\ pend' = [pend EXCEPT ![RecId(p)] = "none"]
    /\ RecoveryNext(p)
    /\ UNCHANGED <<diskVars, blobs, closed, journal, act, lock, nextId, boundVars,
                   writeGhosts, runGhosts>>

\* Seal: write changes/<id>.json as Sealed ...
\*   code: crates/cm-host/src/recovery.rs recover, history.rs History::seal and History::close
RecoverySealWrite(p) ==
    /\ pc[p] = "r_seal_write"
    /\ RecClose(p, "Sealed", "r_seal_remove")

\* ... then remove the pending record.
\*   code: crates/cm-host/src/history.rs History::close, `fs::remove_file`
RecoverySealRemove(p) ==
    /\ pc[p] = "r_seal_remove"
    /\ RecRemovePending(p)

\* Abort: write changes/<id>.json as Aborted ...
\*   code: crates/cm-host/src/recovery.rs recover, history.rs History::abort and History::close
RecoveryAbortWrite(p) ==
    /\ pc[p] = "r_abort_write"
    /\ RecClose(p, "Aborted", "r_abort_remove")

\* ... then remove the pending record.
\*   code: crates/cm-host/src/history.rs History::close, `fs::remove_file`
RecoveryAbortRemove(p) ==
    /\ pc[p] = "r_abort_remove"
    /\ RecRemovePending(p)

\* Needs attention: write the pending record again with that state.
\*   code: crates/cm-host/src/recovery.rs recover, `ctx.history.write_pending(&record)`
RecoveryAttention(p) ==
    /\ pc[p] = "r_attention"
    /\ pend' = [pend EXCEPT ![RecId(p)] = "NeedsAttention"]
    /\ RecoveryNext(p)
    /\ UNCHANGED <<diskVars, blobs, closed, journal, act, lock, nextId, boundVars,
                   writeGhosts, runGhosts>>

\* The blobs the records name.
Refs == UNION {{journal[c][n].before : n \in 1..Len(journal[c])}
               \cup {journal[c][n].after : n \in 1..Len(journal[c])}
               : c \in {c2 \in Ids : Vis(c2) # "none"}}

\* Every old blob that `refs` does not name is removed.
Collect(refs) ==
    blobs' = [t \in BlobTexts |-> IF blobs[t] = "old" /\ t \notin refs
                                  THEN "none" ELSE blobs[t]]

\* Collect garbage: read which blobs the records name, then remove each old blob that
\* none names. With the lock, no other writer of the history runs between the read and
\* the removes, so the model takes them as one step; without it, as two.
\*   code: crates/cm-host/src/history.rs History::collect_garbage, `self.all()`
RecoveryGcRead(p) ==
    /\ pc[p] = "r_gc_read"
    /\ IF Lock
       THEN /\ Collect(Refs)
            /\ RecoveryDone(p)
       ELSE /\ rec' = [rec EXCEPT ![p] = [NoRec EXCEPT !.refs = Refs]]
            /\ Goto(p, "r_gc_remove")
            /\ UNCHANGED <<blobs, lock>>
    /\ UNCHANGED <<diskVars, pend, closed, journal, act, nextId, boundVars, writeGhosts,
                   runGhosts>>

\* Without the lock: remove each old blob the read did not find named.
\*   code: crates/cm-host/src/history.rs History::collect_garbage, `fs::remove_file`
RecoveryGcRemove(p) ==
    /\ pc[p] = "r_gc_remove"
    /\ Collect(rec[p].refs)
    /\ RecoveryDone(p)
    /\ UNCHANGED <<diskVars, pend, closed, journal, act, nextId, boundVars, writeGhosts,
                   runGhosts>>

-----------------------------------------------------------------------------
Init_ ==
    /\ disk = Init
    /\ temps = {}
    /\ blobs = [t \in BlobTexts |-> "none"]
    /\ pend = [c \in Ids |-> "none"]
    /\ closed = [c \in Ids |-> "none"]
    /\ journal = [c \in Ids |-> <<>>]
    /\ lock = "free"
    /\ nextId = 1
    /\ pc = [p \in Procs |-> "idle"]
    /\ act = [p \in Procs |-> NoAct]
    /\ rec = [p \in Procs |-> NoRec]
    /\ crashes = 0 /\ failures = 0 /\ outside = 0
    /\ writer = [f \in Files |-> 0]
    /\ replaced = [f \in Files |-> "none"]
    /\ planned = [c \in Ids |-> {}]
    /\ reached = [c \in Ids |-> {}]
    /\ fate = [c \in Ids |-> "none"]
    /\ dist = [p \in Procs |-> FALSE]
    /\ cleanRun = [c \in Ids |-> FALSE]
    /\ revertBroken = FALSE
    /\ recoveryWrote = FALSE
    /\ lateWrote = FALSE

Next ==
    \/ \E p \in Procs :
        \/ StartSave(p) \/ StartRevert(p) \/ Plan(p)
        \/ StoreBlobs(p) \/ WritePending(p) \/ CheckHash(p) \/ WriteTemp(p)
        \/ Rename(p) \/ Truncate(p) \/ WriteInPlace(p) \/ TrashFile(p)
        \/ TargetWrite(p) \/ TargetRemove(p) \/ SealWrite(p) \/ SealRemove(p)
        \/ ApplyReturn(p)
        \/ UndoClassify(p) \/ UndoTemp(p) \/ UndoRename(p) \/ UndoTruncate(p)
        \/ UndoInPlace(p) \/ UndoRemove(p) \/ UndoAbortWrite(p)
        \/ UndoAbortRemove(p) \/ UndoAttention(p)
        \/ StartRecovery(p) \/ RecoveryLock(p) \/ RecoveryList(p)
        \/ RecoveryInspect(p) \/ RecoveryTemps(p) \/ RecoveryDecide(p)
        \/ RecoveryRoll(p) \/ RecoverySealWrite(p) \/ RecoverySealRemove(p)
        \/ RecoveryAbortWrite(p) \/ RecoveryAbortRemove(p) \/ RecoveryAttention(p)
        \/ RecoveryGcRead(p) \/ RecoveryGcRemove(p)
        \/ Crash(p)
    \/ \E f \in Files : \E t \in {OutsideText, Init[f]} : OutsideEdit(f, t)
    \/ DayPasses

Spec == Init_ /\ [][Next]_vars

\* The two instances are alike: a state and its mirror image are checked once.
Symmetry == Permutations(Procs)

-----------------------------------------------------------------------------
(* The properties. Each is one sentence; the harm is the next one.         *)

\* Recovery and a history read never change an agent file.
\* Harm: the operator's configuration changes without anyone asking for it.
RecoveryWritesNoAgentFile == ~recoveryWrote

\* The tool writes a change's text only from that change's own apply, while it runs.
\* Harm: an edit the operator saw fail, or that died with its process, shows up in
\* their configuration later, possibly over their newer work.
NoLateWrite == ~lateWrote

\* Whenever a file holds text the tool wrote over other text, the replaced text is in
\* the history, in a record that names the file.
\* Harm: the operator's text is gone, and no revert can bring it back.
ReplacedTextKept ==
    \A f \in Files :
        replaced[f] \notin {"none", Absent} =>
            /\ blobs[replaced[f]] # "none"
            /\ \E c \in Ids :
                 /\ Vis(c) # "none"
                 /\ \E n \in 1..Len(journal[c]) :
                      /\ journal[c][n].file = f
                      /\ replaced[f] \in {journal[c][n].before, journal[c][n].after}

\* A record marked sealed: every one of its files held its after text at some time.
\* Harm: the history says a change is in place when part of it never reached the disk.
SealedMeansDone ==
    \A c \in Ids : Vis(c) = "Sealed" => \A op \in planned[c] : op.file \in reached[c]

\* A record marked aborted: no file holds text its own apply wrote.
\* Harm: the history says nothing of a change is on disk while part of it is.
AbortedMeansNothing ==
    \A c \in Ids : Vis(c) = "Aborted" =>
        \A f \in Files : writer[f] # c

\* An apply that no crash, no injected error and no other write of its own files
\* interrupts returns Applied and its record stays sealed, also while the other
\* instance reads the history or applies another change.
\* Harm: a correct edit fails, or is recorded as something it is not, because a second
\* window of the tool was open.
UndisturbedApplySeals ==
    \A c \in Ids :
        fate[c] \in {"ok", "err"} /\ cleanRun[c] => fate[c] = "ok" /\ Vis(c) = "Sealed"

\* A revert of a record whose files are each at before or at after, with no other
\* write of them and no injected error while it runs, returns every file to its text
\* from before and leaves the record closed.
\* Harm: the operator cannot undo a change, or the change stays marked unfinished.
RevertRestoresAndCloses == ~revertBroken


\* Every variable stays in its type. A failure here is a modelling error.
TypeOK ==
    /\ \A f \in Files : disk[f] \in BlobTexts \cup {Absent}
    /\ lock \in Procs \cup {"free"}
    /\ nextId \in 1..(MaxIds + 1)
    /\ \A c \in Ids : pend[c] \in {"none", "Pending", "NeedsAttention"}
    /\ \A c \in Ids : closed[c] \in {"none", "Sealed", "Aborted"}
=============================================================================

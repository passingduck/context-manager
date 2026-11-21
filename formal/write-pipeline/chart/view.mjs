// The view of WritePipeline.tla for the /statechart skill: what the owner sees and cares
// about, in Korean. Each state is what the history says about the change and how many of
// the two files differ from where they started. Each arrow is what happens, as the owner
// would see it.

export const title = "도구가 설정 파일을 저장하고 되돌리는 과정";

const START = { a: "absent", b: "b0" };

const vis = (v, c) => (v.closed[c] !== "none" ? v.closed[c] : v.pend[c]);
const changed = (v) => ["a", "b"].filter((f) => v.disk[f] !== START[f]).length;
const files = (v) =>
  ["a", "b"].some((f) => v.disk[f] === "torn")
    ? `파일 ${["a", "b"].find((f) => v.disk[f] === "torn")}가 반쯤 쓰인 채 남아 있다`
    : ["두 파일 모두 처음 그대로다", "파일 하나가 바뀌어 있다", "두 파일이 모두 바뀌어 있다"][changed(v)];
const unfinished = (s) => s === "Pending" || s === "NeedsAttention";
const anyUnfinished = (v) => unfinished(vis(v, 1)) || unfinished(vis(v, 2));

// What the history says, with the instance idle.
const resting = (v) => {
  const first = vis(v, 1);
  const second = vis(v, 2);
  if (second !== "none") {
    if (second === "Sealed") return `되돌리기가 끝났다. ${files(v)}.`;
    if (second === "Aborted") return `되돌리기가 취소로 기록되었다. ${files(v)}.`;
    if (second === "Pending") return `되돌리기가 끝나지 않은 채 기록에 남아 있다. ${files(v)}.`;
    return `되돌리기가 확인이 필요하다고 기록되었다. ${files(v)}.`;
  }
  if (first === "none") return "아무것도 바뀌지 않았다.";
  if (first === "Sealed") return `저장이 끝나 완료로 기록되었다. ${files(v)}.`;
  if (first === "Aborted") return `저장이 취소로 기록되었다. ${files(v)}.`;
  if (first === "Pending") return `저장이 끝나지 않은 채 기록에 남아 있다. ${files(v)}.`;
  return `저장이 확인이 필요하다고 기록되었다. ${files(v)}.`;
};

export const state = (v) => {
  const pc = v.pc.p1;
  if (pc.startsWith("a_") || pc.startsWith("u_")) {
    const what = v.act.p1.kind === "revert" ? "되돌리기를" : "저장을";
    return `${what} 적용하고 있다. ${files(v)}.`;
  }
  if (pc.startsWith("r_") && anyUnfinished(v)) {
    return `복구가 끝나지 않은 기록을 읽고 정리하고 있다. ${files(v)}.`;
  }
  return resting(v);
};

export const group = (v) => {
  const pc = v.pc.p1;
  if (pc.startsWith("a_") || pc.startsWith("u_")) return WORKING;
  if (pc.startsWith("r_") && anyUnfinished(v)) return RECOVERING;
  if (anyUnfinished(v)) return ATTENTION;
  return RESTING;
};

const RESTING = "도구가 쉬고 있다";
const WORKING = "도구가 파일을 쓰고 있다";
const RECOVERING = "복구가 기록을 정리하고 있다";
const ATTENTION = "끝나지 않은 기록이 남아 있다";

export const colors = {
  [RESTING]: "teal",
  [WORKING]: "blue",
  [RECOVERING]: "yellow",
  [ATTENTION]: "orange",
};

const failed = (from, to) => to.failures > from.failures;
const ends = (to) => to.pc.p1 === "idle";
const undoes = (to) => to.pc.p1.startsWith("u_");

// A revert that finds a file of its target at neither text stops with a Conflict.
const conflict = (from) => {
  const a = from.act.p1;
  if (a.kind !== "revert") return false;
  return Object.values(from.journal[a.target] ?? {}).some(
    (op) => from.disk[op.file] !== op.before && from.disk[op.file] !== op.after,
  );
};

// Each step in the words of what the owner would see happen.
const STEPS = {
  StartSave: () => "운영자가 저장한다",
  StartRevert: () => "운영자가 되돌리기를 누른다",
  StartRecovery: () => "운영자가 기록을 연다",
  Crash: () => "프로그램이 죽고 다시 시작한다",
  OutsideEdit: () => "운영자가 도구 밖에서 파일을 고친다",
  DayPasses: () => "하루가 지난다",
  Plan: (from, to) =>
    !ends(to)
      ? "도구가 파일을 다시 읽고 계획을 세운다"
      : conflict(from)
        ? "도구가 파일이 바뀐 것을 찾고 충돌을 알린다"
        : "도구가 다시 계획해 보니 쓸 것이 없다",
  StoreBlobs: () => "도구가 이전 내용과 새 내용을 기록 폴더에 저장한다",
  WritePending: (from, to) =>
    failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 끝나지 않은 기록을 쓴다",
  CheckHash: (from, to) =>
    undoes(to) ? "도구가 파일이 바뀐 것을 찾고 되돌리기 시작한다" : "도구가 파일이 계획 때와 같은지 확인한다",
  WriteTemp: (from, to) =>
    failed(from, to) ? "도구가 임시 파일을 쓰다가 실패한다" : "도구가 임시 파일을 쓴다",
  Rename: (from, to) =>
    failed(from, to) ? "도구가 파일을 바꾸다가 실패한다" : "도구가 파일 하나를 바꾼다",
  Truncate: (from, to) =>
    failed(from, to) ? "도구가 파일을 열다가 실패한다" : "도구가 파일을 비우고 다시 쓰기 시작한다",
  WriteInPlace: (from, to) =>
    failed(from, to) ? "도구가 파일을 다시 쓰다가 실패한다" : "도구가 파일 하나를 바꾼다",
  TrashFile: (from, to) =>
    failed(from, to) ? "도구가 파일을 휴지통으로 옮기다가 실패한다" : "도구가 파일을 휴지통으로 옮긴다",
  TargetWrite: (from, to) =>
    failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 원래 변경의 기록을 취소로 쓴다",
  TargetRemove: () => "도구가 원래 변경의 끝나지 않은 기록을 지운다",
  SealWrite: (from, to) =>
    failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 완료 기록을 쓴다",
  SealRemove: () => "도구가 끝나지 않은 기록을 지우고 완료를 알린다",
  UndoClassify: () => "도구가 파일 하나의 상태를 읽는다",
  UndoTemp: (from, to) =>
    failed(from, to) ? "도구가 임시 파일을 쓰다가 실패한다" : "도구가 원래 내용을 임시 파일에 쓴다",
  UndoRename: (from, to) =>
    failed(from, to) ? "도구가 파일을 원래대로 돌리다가 실패한다" : "도구가 바꾼 파일을 원래대로 돌린다",
  UndoRemove: (from, to) =>
    failed(from, to) ? "도구가 파일을 원래대로 돌리다가 실패한다" : "도구가 새로 만든 파일을 지운다",
  UndoTruncate: () => "도구가 파일을 비우고 원래 내용을 쓰기 시작한다",
  UndoInPlace: (from, to) =>
    failed(from, to) ? "도구가 파일을 원래대로 돌리다가 실패한다" : "도구가 바꾼 파일을 원래대로 돌린다",
  UndoAbortWrite: (from, to) =>
    failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 취소 기록을 쓴다",
  UndoAbortRemove: () => "도구가 끝나지 않은 기록을 지우고 실패를 알린다",
  UndoAttention: () => "도구가 기록을 확인 필요로 남기고 실패를 알린다",
  RecoveryLock: () => "복구가 시작된다",
  RecoveryList: () => "복구가 끝나지 않은 기록의 목록을 읽는다",
  RecoveryInspect: () => "복구가 파일 하나의 상태를 읽는다",
  RecoveryTemps: () => "복구가 남은 임시 파일을 지운다",
  RecoveryDecide: (from, to) => (ends(to) ? "복구가 끝난다" : "복구가 기록을 어떻게 고칠지 정한다"),
  RecoverySealWrite: () => "복구가 기록을 완료로 고친다",
  RecoverySealRemove: () => "복구가 기록을 완료로 고친다",
  RecoveryAbortWrite: () => "복구가 기록을 취소로 고친다",
  RecoveryAbortRemove: () => "복구가 기록을 취소로 고친다",
  RecoveryAttention: () => "복구가 기록을 확인 필요로 고친다",
  RecoveryGcRead: () => "복구가 쓰지 않는 오래된 내용을 지우고 끝난다",
};

export const step = (action, from, to) => (STEPS[action] ?? (() => action))(from, to);

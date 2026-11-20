// The view of WritePipeline.tla for the /statechart skill: what the owner sees and cares
// about, in Korean. Each state is what the history says about the change and how many of
// the two files differ from where they started. Each arrow is what happens, as the owner
// would see it.

export const title = "도구가 설정 파일을 저장하고 되돌리는 과정";

const START = { a: "absent", b: "b0" };

const vis = (v, c) => (v.closed[c] !== "none" ? v.closed[c] : v.pend[c]);
const changed = (v) => ["a", "b"].filter((f) => v.disk[f] !== START[f]).length;
const files = (v) =>
  ["두 파일 모두 처음 그대로다", "파일 하나가 바뀌어 있다", "두 파일이 모두 바뀌어 있다"][changed(v)];
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
    const what = v.act.p1.kind === "revert" ? "되돌리기" : "저장";
    return `${what}를 적용하고 있다. ${files(v)}.`.replace("저장를", "저장을");
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

export const step = (action, from, to) => {
  switch (action) {
    case "StartSave":
      return "운영자가 저장한다";
    case "StartRevert":
      return "운영자가 되돌리기를 누른다";
    case "StartRecovery":
      return "운영자가 기록을 연다";
    case "Plan":
      return "도구가 다시 계획해 보니 쓸 것이 없다";
    case "Crash":
      return "프로그램이 죽고 다시 시작한다";
    case "WriteTemp":
    case "Rename":
      return failed(from, to) ? "도구가 파일을 쓰다가 실패한다" : "도구가 파일 하나를 바꾼다";
    case "TrashFile":
      return failed(from, to) ? "도구가 파일을 휴지통으로 옮기다가 실패한다" : "도구가 파일을 휴지통으로 옮긴다";
    case "WritePending":
    case "SealWrite":
    case "TargetWrite":
      return failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 기록을 쓴다";
    case "SealRemove":
      return "도구가 완료를 기록한다";
    case "TargetRemove":
      return "도구가 저장 기록을 취소로 정리한다";
    case "UndoTemp":
    case "UndoRename":
    case "UndoRemove":
      return failed(from, to) ? "도구가 파일을 원래대로 돌리다가 실패한다" : "도구가 바꾼 파일을 원래대로 돌린다";
    case "UndoAbortWrite":
      return failed(from, to) ? "도구가 기록을 쓰다가 실패한다" : "도구가 기록을 쓴다";
    case "UndoAbortRemove":
      return "도구가 기록을 취소로 남기고 실패를 알린다";
    case "UndoAttention":
      return "도구가 기록을 확인 필요로 남기고 실패를 알린다";
    case "RecoverySealWrite":
    case "RecoverySealRemove":
      return "복구가 기록을 완료로 고친다";
    case "RecoveryAbortWrite":
    case "RecoveryAbortRemove":
      return "복구가 기록을 취소로 고친다";
    case "RecoveryAttention":
      return "복구가 기록을 확인 필요로 고친다";
    case "RecoveryGcRead":
    case "RecoveryDecide":
      return "복구가 기록을 확인 필요로 남기고 끝난다";
    default:
      return action;
  }
};

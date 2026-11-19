import type { en } from "./en";

export const ko: typeof en = {
  title: "context-manager",
  language: "언어",
  loading: "불러오는 중입니다",
  failed: "요청이 실패했습니다",
  session: {
    heading: "세션",
    version: "버전",
    shell: "실행 방식",
    readOnly: "읽기 전용",
    launchCwd: "프로젝트 디렉터리",
    home: "홈 디렉터리",
    os: "운영체제",
    yes: "예",
    no: "아니요",
  },
  agents: {
    heading: "에이전트",
    home: "홈",
    missing: "없음",
  },
  shell: { desktop: "데스크톱 창", serve: "웹 서버" },
  os: { macos: "macOS", linux: "Linux", windows: "Windows", other: "기타" },
  presence: {
    installed: "설치됨",
    configOnly: "설정은 있으나 프로그램을 찾지 못함",
    absent: "찾지 못함",
  },
};

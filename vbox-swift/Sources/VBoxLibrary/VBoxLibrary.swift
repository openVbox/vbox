import AppKit
import Combine
import SwiftUI

enum VBoxSwiftVersion {
    static let current = "0.1.1"
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - L10n — 4언어 (en/ko/zh/ja) 다국어화. 시스템 Locale 자동 감지, default en.
// ───────────────────────────────────────────────────────────────────────────

enum AppLanguage: String {
    case en, ko, zh, ja

    static let current: AppLanguage = {
        let primary = Locale.preferredLanguages.first ?? "en"
        if primary.hasPrefix("ko") { return .ko }
        if primary.hasPrefix("zh") { return .zh }
        if primary.hasPrefix("ja") { return .ja }
        return .en
    }()
}

/// 짧은 헬퍼: `L("Machines")` → 현 언어에 맞는 표시 문자열. 누락 키는 en 폴백, 그것도 없으면 키 그대로.
func L(_ key: String) -> String {
    let table: [String: String]?
    switch AppLanguage.current {
    case .ko: table = L10n.ko
    case .zh: table = L10n.zh
    case .ja: table = L10n.ja
    case .en: table = nil  // en 은 dictionary 우선 그 다음 key
    }
    if let table, let v = table[key] { return v }
    return L10n.en[key] ?? key
}

/// 1개 인자 보간: `L("Active label", arg: name)` — 사전 값에 "{0}" placeholder 치환.
func L(_ key: String, arg: String) -> String {
    L(key).replacingOccurrences(of: "{0}", with: arg)
}

func L(_ key: String, _ a: String, _ b: String) -> String {
    L(key).replacingOccurrences(of: "{0}", with: a).replacingOccurrences(of: "{1}", with: b)
}

func L(_ key: String, _ a: String, _ b: String, _ c: String) -> String {
    L(key)
        .replacingOccurrences(of: "{0}", with: a)
        .replacingOccurrences(of: "{1}", with: b)
        .replacingOccurrences(of: "{2}", with: c)
}

enum L10n {
    // 영어: default + 폴백 사전. 모든 키 정의. 다른 언어는 누락 시 영어로.
    static let en: [String: String] = [
        // Machines sheet
        "Machines": "Machines",
        "Guest Machines": "Guest Machines",
        "Machines subtitle": "Parallels VMs + remote SSH hosts",
        "Refresh": "Refresh",
        "Close": "Close",
        "Add": "Add",
        "Cancel": "Cancel",
        "Save": "Save",
        "Saving": "Saving…",
        "Loading": "Loading…",
        "Add remote host": "Add remote host",
        "Remove remote host": "Remove remote host",
        "Machine settings": "Machine settings",
        "No machines": "No machines",
        "No machines hint": "Add a Parallels VM or remote host to get started.",
        "Add a remote host": "Add a remote host",
        "machines found {0}": "{0} machine(s) found",
        // Status badges
        "Status running": "Running",
        "Status stopped": "Stopped",
        "Status suspended": "Suspended",
        "Status paused": "Paused",
        "Status invalid": "Invalid",
        "Status remote": "Remote",
        "Status unknown": "Unknown",
        // Row chrome
        "Active": "Active",
        "Tap to activate guest": "Tap to set as the active guest",
        "Cannot activate": "Cannot activate (needs Linux + running + IP)",
        "Activate guest": "Set this machine as the active guest",
        "Cannot select": "Selection unavailable (needs Linux + running)",
        "Power off VM": "Stop VM",
        "Power on VM": "Start VM",
        "Open settings": "Open machine settings",
        "Bundle": "App bundle",
        "Run in viewer": "In viewer",
        "Bundle help installed": "Run as a standalone macOS app (shown in Launchpad/Dock)",
        "Bundle help missing": "Toggle Launchpad to install the bundle first",
        "Viewer help": "Attach to the open viewer, or start a new one",
        // Empty library
        "Library empty title": "This machine has no apps yet",
        "Library empty subtitle": "Once {0}'s desktop entries are fetched, they show up here.",
        "Fetch this machine's library": "Fetch this machine's library",
        // Toolbar
        "Refresh icons": "Refresh icons",
        "Reload library + Launchpad": "Reload library + rebuild Launchpad bundles",
        "Running": "Running",
        "Running with count": "Running ({0})",
        // Detail
        "Launchpad": "Launchpad",
        "Command": "Command",
        "Category": "Categories",
        // Running processes sheet
        "Running processes": "Running processes",
        "Processes subtitle": "Processes attached to the guest Wayland session ({0})",
        "Refresh processes": "Refresh processes",
        "No running apps": "No running apps",
        "No running apps subtitle": "Apps you launch from the library will appear here.",
        "Auto refresh hint": "Auto-refreshes every 5s",
        "End": "End",
        "PID {0}": "PID {0}",
        "started seconds ago {0}": "started {0}s ago",
        "started minutes ago {0}": "started {0}m ago",
        "started hours ago {0}": "started {0}h ago",
        "started hours minutes ago {0} {1}": "started {0}h {1}m ago",
        // Add remote sheet
        "Add remote host title": "Add remote host",
        "Add remote subtitle": "Register an SSH-reachable machine alongside Parallels VMs.",
        "Required info": "Required",
        "Optional fields": "Optional (you can change these later)",
        "Display name": "Display name",
        "Display name placeholder": "e.g. Office desktop",
        "Display name hint": "Free-form label shown in the machine list.",
        "SSH target": "SSH target",
        "SSH target placeholder": "e.g. ubuntu@192.168.1.10",
        "SSH target hint": "user@host. Non-default ports go in the machine settings after adding.",
        "Workdir": "Workspace directory",
        "Workdir placeholder": "e.g. /home/ubuntu/vbox",
        "Workdir hint placeholder": "If empty, defaults to vbox/ under the user home.",
        "OS label": "OS label",
        "OS label placeholder": "e.g. Ubuntu 24.04",
        "OS label hint": "Distro name in the list. Recognized names auto-pick a logo.",
        // Config sheet
        "Config title": "Machine settings",
        "SSH section": "SSH",
        "SSH footer": "All vbox commands to this machine go through this SSH target.",
        "Field user": "User",
        "Field user placeholder": "e.g. ubuntu",
        "Field user hint": "SSH account name used to log into the guest.",
        "Field host": "Host",
        "Field host placeholder": "e.g. 192.168.1.10",
        "Field host hint": "IP address or domain.",
        "Field ssh port": "SSH port",
        "Field ssh port placeholder": "e.g. 22",
        "Field ssh port hint": "Only set this if using a non-standard SSH port.",
        "Field identity": "Private key file",
        "Field identity placeholder": "e.g. ~/.ssh/vbox_ed25519",
        "Field identity hint": "Force a specific SSH private key file when needed.",
        "Field identity browse": "Choose…",
        "Field password": "Password",
        "Field password placeholder": "Optional — stored in macOS Keychain",
        "Field password hint": "Password is kept in your local macOS Keychain only; we never write it to disk in plain text.",
        "Field password current": "Saved in Keychain",
        "Field password clear": "Clear",
        "Field password change placeholder": "Leave blank to keep existing",
        "Machine info": "Machine info",
        "Identity section": "Identity",
        "Custom overrides section": "Custom overrides",
        "Failed to load info": "Couldn't load machine info",
        "Probe section": "Connection probe",
        "Run probe": "Run probe",
        "Re-probe": "Re-probe",
        "Probing": "Probing…",
        "Reachable": "Reachable",
        "Unreachable": "Unreachable",
        "Guest path section": "Guest path",
        "Field guestdir": "Workspace directory",
        "Field guestdir placeholder": "e.g. /home/ubuntu/vbox",
        "Field guestdir hint": "Where vbox source and binaries live on the guest.",
        "Viewer section": "Connection & viewer",
        "Viewer footer": "Leave blank to let vbox pick sensible defaults.",
        "Field port": "Server port",
        "Field port placeholder": "e.g. 5710",
        "Field port hint": "TCP port used by the vbox data channel.",
        "Field socket": "Wayland socket",
        "Field socket placeholder": "e.g. vbox-0",
        "Field socket hint": "Wayland session name on the guest.",
        "Field width": "Window width (px)",
        "Field width placeholder": "e.g. 1280",
        "Field width hint": "Initial macOS viewer window width. Blank = auto.",
        "Field height": "Window height (px)",
        "Field height placeholder": "e.g. 800",
        "Field height hint": "Initial macOS viewer window height. Blank = auto.",
        "Flags section": "Flags",
        "Flag debug": "Debug logs",
        "Flag debug hint": "Record detailed wire messages for connections, input and window management.",
        "Flag tls": "mTLS direct connect",
        "Flag tls hint": "Skip the SSH tunnel and use certs to reach the vbox control channel.",
        "Memo section": "Memo",
        "Memo placeholder": "e.g. Office desktop — Rust build only",
        "Memo footer": "Personal note. Doesn't affect vbox behaviour.",
        "Current label": "Current",
        // Library status text
        "Library fresh": "Up to date",
        "Refresh failed": "Refresh failed",
        "Library reloaded {0}": "Library + Launchpad updated ({0})",
        "Reload library": "Reload library",
        "Launchpad bundles refresh": "Updating Launchpad bundles",
        "Bundle started {0}": "Launching {0} bundle…",
        "Viewer started {0}": "Launching {0} in viewer…",
        "Run failed": "Launch failed",
        "Bundle run failed {0}": "Bundle launch failed: {0}",
        "Bundle missing": "Bundle not installed — fetch the library or toggle Launchpad first",
        "Active machine no count {0}": "{0} active",
        "Active machine with count {0} {1}": "{0} active — {1} apps",
        "Active machine empty cache {0}": "{0} active — library not loaded yet",
        "Default guest active": "Default guest active",
        "Saved settings {0}": "Saved settings for {0}",
        "Save failed": "Some keys failed to save",
        "Add remote failed": "Failed to add remote host",
        "Add remote ok {0}": "Remote host added: {0}",
        "Remove remote ok {0}": "Remote host removed: {0}",
        "Remove remote failed": "Failed to remove remote host",
        // Search
        "Search": "Search",
        // Bundle progress
        "Bundle progress phase {0} {1}": "Build bundle ({0}/{1})",
        "Bundle progress phase with name {0} {1} {2}": "Building bundle ({0}/{1}) · {2}",
        // Misc
        "Apps count {0}": "{0} apps",
        "Toolbar refresh icons help": "Refresh icons",
        "Toolbar reload help": "Reload app library + rebuild Launchpad bundles",
        "Machines toolbar help": "Guest machines / pick active",
        "View running help": "Show processes running on the guest",
        "Global settings": "Global settings",
        "Show macOS titlebar": "Show macOS titlebar",
        "Titlebar hint": "Off → viewer window is borderless. Apps with their own header bar (Firefox/Chrome) may feel more natural with this off.",
        "Settings reapply hint": "Changes apply when the next viewer launches — windows already open keep their previous setting.",
    ]

    static let ko: [String: String] = [
        "Machines": "머신",
        "Guest Machines": "게스트 머신",
        "Machines subtitle": "Parallels VM + 원격 SSH 호스트",
        "Refresh": "새로 고침",
        "Close": "닫기",
        "Add": "추가",
        "Cancel": "취소",
        "Save": "저장",
        "Saving": "저장 중…",
        "Loading": "로드 중…",
        "Add remote host": "원격 호스트 추가",
        "Remove remote host": "원격 호스트 등록 삭제",
        "Machine settings": "머신 설정",
        "No machines": "머신 없음",
        "No machines hint": "Parallels VM 또는 원격 호스트를 추가해 보세요.",
        "Add a remote host": "원격 호스트 추가",
        "machines found {0}": "{0}대 발견",
        "Status running": "실행 중",
        "Status stopped": "중지됨",
        "Status suspended": "일시 정지",
        "Status paused": "정지",
        "Status invalid": "없음/오류",
        "Status remote": "원격",
        "Status unknown": "알 수 없음",
        "Active": "활성",
        "Tap to activate guest": "탭하면 활성 guest 로 선택",
        "Cannot activate": "선택 불가 (Linux + 실행 중 + IP 필요)",
        "Activate guest": "이 머신을 활성 guest 로 선택",
        "Cannot select": "선택 불가 (Linux + 실행 중 필요)",
        "Power off VM": "VM 종료",
        "Power on VM": "VM 부팅",
        "Open settings": "머신 설정",
        "Bundle": "앱 번들",
        "Run in viewer": "뷰에서",
        "Bundle help installed": "독립 macOS 앱처럼 실행 (Launchpad/Dock 표시)",
        "Bundle help missing": "Launchpad 토글로 번들 설치 후 사용 가능",
        "Viewer help": "열려있는 뷰어에 추가, 없으면 새 뷰어를 띄움",
        "Library empty title": "이 머신의 앱 라이브러리가 비어 있어요",
        "Library empty subtitle": "{0} 의 .desktop 앱 목록을 받아오면 여기에 표시됩니다.",
        "Fetch this machine's library": "이 머신 라이브러리 가져오기",
        "Refresh icons": "아이콘 새로고침",
        "Reload library + Launchpad": "앱 목록 다시 로드 + Launchpad 번들 갱신",
        "Running": "실행 중",
        "Running with count": "실행 중 ({0})",
        "Launchpad": "Launchpad",
        "Command": "명령",
        "Category": "분류",
        "Running processes": "실행 중인 프로세스",
        "Processes subtitle": "guest 의 Wayland 세션({0})에 연결된 프로세스",
        "Refresh processes": "프로세스 새로고침",
        "No running apps": "실행 중인 앱이 없습니다",
        "No running apps subtitle": "앱 라이브러리에서 실행 버튼을 누르면 이 목록에 표시됩니다.",
        "Auto refresh hint": "5초마다 자동 새로고침",
        "End": "종료",
        "PID {0}": "PID {0}",
        "started seconds ago {0}": "{0}초 전 시작",
        "started minutes ago {0}": "{0}분 전 시작",
        "started hours ago {0}": "{0}시간 전 시작",
        "started hours minutes ago {0} {1}": "{0}시간 {1}분 전 시작",
        "Add remote host title": "원격 호스트 추가",
        "Add remote subtitle": "Parallels 외 SSH 로 접속 가능한 머신을 머신 리스트에 등록합니다.",
        "Required info": "필수 정보",
        "Optional fields": "선택 (나중에 머신 설정에서 변경 가능)",
        "Display name": "표시 이름",
        "Display name placeholder": "예: 사무실 데스크탑",
        "Display name hint": "머신 리스트에 표시될 별칭. 자유롭게.",
        "SSH target": "SSH 타겟",
        "SSH target placeholder": "예: ubuntu@192.168.1.10",
        "SSH target hint": "user@host 형식. 비표준 포트는 추가 후 머신 설정에서 따로 지정.",
        "Workdir": "작업 디렉터리",
        "Workdir placeholder": "예: /home/ubuntu/vbox",
        "Workdir hint placeholder": "비우면 사용자 홈 아래 vbox/ 로 자동 설정.",
        "OS label": "OS 라벨",
        "OS label placeholder": "예: Ubuntu 24.04",
        "OS label hint": "리스트에 표시될 OS 이름. 배포판 이름이 들어가면 적절한 아이콘 자동 선택.",
        "Config title": "머신 설정",
        "SSH section": "SSH 연결",
        "SSH footer": "이 머신으로의 모든 게스트 통신이 위 SSH 정보로 이루어집니다.",
        "Field user": "사용자",
        "Field user placeholder": "예: ubuntu",
        "Field user hint": "게스트에 SSH 로 접속할 계정 이름.",
        "Field host": "호스트",
        "Field host placeholder": "예: 192.168.1.10",
        "Field host hint": "IP 주소 또는 도메인.",
        "Field ssh port": "SSH 포트",
        "Field ssh port placeholder": "예: 22",
        "Field ssh port hint": "비표준 SSH 포트를 쓰는 경우만 입력.",
        "Field identity": "개인키 파일",
        "Field identity placeholder": "예: ~/.ssh/vbox_ed25519",
        "Field identity hint": "특정 SSH 개인키 파일을 강제할 때만 지정.",
        "Field identity browse": "파일 선택…",
        "Field password": "비밀번호",
        "Field password placeholder": "선택 — macOS 키체인에 저장",
        "Field password hint": "비밀번호는 로컬 macOS 키체인에만 저장되며, 평문으로 디스크에 기록되지 않습니다.",
        "Field password current": "키체인에 저장됨",
        "Field password clear": "삭제",
        "Field password change placeholder": "비워두면 기존 값 유지",
        "Machine info": "머신 정보",
        "Identity section": "식별",
        "Custom overrides section": "사용자 설정",
        "Failed to load info": "정보를 불러오지 못했습니다",
        "Probe section": "연결 점검",
        "Run probe": "점검 실행",
        "Re-probe": "다시 점검",
        "Probing": "점검 중…",
        "Reachable": "도달 가능",
        "Unreachable": "도달 불가",
        "Guest path section": "게스트 경로",
        "Field guestdir": "작업 디렉터리",
        "Field guestdir placeholder": "예: /home/ubuntu/vbox",
        "Field guestdir hint": "게스트에 vbox 소스와 바이너리가 들어갈 폴더.",
        "Viewer section": "연결 & 뷰어",
        "Viewer footer": "비워두면 vbox 가 알아서 적절한 값을 고릅니다.",
        "Field port": "서버 포트",
        "Field port placeholder": "예: 5710",
        "Field port hint": "vbox 데이터 채널이 사용할 포트.",
        "Field socket": "Wayland 소켓",
        "Field socket placeholder": "예: vbox-0",
        "Field socket hint": "게스트 Wayland 세션 이름.",
        "Field width": "창 너비 (px)",
        "Field width placeholder": "예: 1280",
        "Field width hint": "macOS 뷰어 창 초기 너비. 비우면 화면 크기 자동.",
        "Field height": "창 높이 (px)",
        "Field height placeholder": "예: 800",
        "Field height hint": "macOS 뷰어 창 초기 높이. 비우면 화면 크기 자동.",
        "Flags section": "플래그",
        "Flag debug": "디버그 로그",
        "Flag debug hint": "연결/입력/창 관리 와이어 메시지를 자세히 기록.",
        "Flag tls": "mTLS 직접 연결",
        "Flag tls hint": "SSH 터널 없이 인증서로 vbox 제어 채널에 직접 접속.",
        "Memo section": "메모",
        "Memo placeholder": "예: 사무실 데스크탑 — Rust 빌드 전용",
        "Memo footer": "vbox 동작에는 영향이 없는 개인용 메모.",
        "Current label": "현재",
        "Library fresh": "최신 상태",
        "Refresh failed": "새로고침 실패",
        "Library reloaded {0}": "라이브러리 + Launchpad 갱신 완료 ({0}개)",
        "Reload library": "앱 목록 다시 로드",
        "Launchpad bundles refresh": "Launchpad 번들 갱신",
        "Bundle started {0}": "{0} 앱 번들 실행",
        "Viewer started {0}": "{0} 뷰에서 실행",
        "Run failed": "실행 실패",
        "Bundle run failed {0}": "번들 실행 실패: {0}",
        "Bundle missing": "번들 미설치 — 라이브러리 가져오기/Launchpad 토글 후 다시 시도",
        "Active machine no count {0}": "{0} 활성",
        "Active machine with count {0} {1}": "{0} 활성 — {1}개 앱",
        "Active machine empty cache {0}": "{0} 활성 — 아직 라이브러리 미로드",
        "Default guest active": "기본 guest 활성",
        "Saved settings {0}": "{0} 설정 저장됨",
        "Save failed": "일부 키 저장 실패",
        "Add remote failed": "원격 호스트 추가 실패",
        "Add remote ok {0}": "원격 호스트 추가: {0}",
        "Remove remote ok {0}": "원격 호스트 삭제: {0}",
        "Remove remote failed": "원격 호스트 삭제 실패",
        "Search": "검색",
        "Bundle progress phase {0} {1}": "번들 생성 ({0}/{1})",
        "Bundle progress phase with name {0} {1} {2}": "번들 생성 ({0}/{1}) · {2}",
        "Apps count {0}": "{0}개 앱",
        "Toolbar refresh icons help": "아이콘 새로고침",
        "Toolbar reload help": "앱 목록 다시 로드 + Launchpad 번들 갱신",
        "Machines toolbar help": "게스트 머신 리스트 / 활성 머신 선택",
        "View running help": "guest에서 실행 중인 프로세스 보기",
        "Global settings": "전역 설정",
        "Show macOS titlebar": "macOS 타이틀바 표시",
        "Titlebar hint": "끄면 viewer 창이 borderless로 표시됩니다. Firefox·Chrome 처럼 자체 헤더바가 있는 앱은 끄는 게 더 자연스러울 수 있습니다.",
        "Settings reapply hint": "변경한 설정은 다음에 새 viewer를 띄울 때 적용됩니다 — 이미 떠 있는 창은 그대로입니다.",
    ]

    static let zh: [String: String] = [
        "Machines": "虚拟机",
        "Guest Machines": "虚拟机",
        "Machines subtitle": "Parallels 虚拟机 + 远程 SSH 主机",
        "Refresh": "刷新",
        "Close": "关闭",
        "Add": "添加",
        "Cancel": "取消",
        "Save": "保存",
        "Saving": "正在保存…",
        "Loading": "正在加载…",
        "Add remote host": "添加远程主机",
        "Remove remote host": "移除远程主机",
        "Machine settings": "虚拟机设置",
        "No machines": "没有虚拟机",
        "No machines hint": "添加一个 Parallels 虚拟机或远程主机以开始。",
        "Add a remote host": "添加远程主机",
        "machines found {0}": "找到 {0} 台",
        "Status running": "运行中",
        "Status stopped": "已停止",
        "Status suspended": "已挂起",
        "Status paused": "已暂停",
        "Status invalid": "无效/错误",
        "Status remote": "远程",
        "Status unknown": "未知",
        "Active": "活动",
        "Tap to activate guest": "点击设为活动客户机",
        "Cannot activate": "无法选中 (需要 Linux + 运行中 + IP)",
        "Activate guest": "将此机器设为活动客户机",
        "Cannot select": "无法选中 (需要 Linux + 运行中)",
        "Power off VM": "关机",
        "Power on VM": "开机",
        "Open settings": "打开虚拟机设置",
        "Bundle": "应用包",
        "Run in viewer": "在视图中",
        "Bundle help installed": "以独立 macOS 应用方式运行 (显示在 Launchpad/Dock)",
        "Bundle help missing": "先通过 Launchpad 开关安装应用包",
        "Viewer help": "附加到已开的查看器,没有则新建",
        "Library empty title": "此虚拟机暂无应用",
        "Library empty subtitle": "获取 {0} 的应用列表后将显示在这里。",
        "Fetch this machine's library": "获取此机器的应用库",
        "Refresh icons": "刷新图标",
        "Reload library + Launchpad": "重新加载并重建 Launchpad 包",
        "Running": "运行中",
        "Running with count": "运行中 ({0})",
        "Launchpad": "Launchpad",
        "Command": "命令",
        "Category": "类别",
        "Running processes": "运行中的进程",
        "Processes subtitle": "连接到客户机 Wayland 会话 ({0}) 的进程",
        "Refresh processes": "刷新进程",
        "No running apps": "无运行中的应用",
        "No running apps subtitle": "从应用库启动的应用会显示在此处。",
        "Auto refresh hint": "每 5 秒自动刷新",
        "End": "结束",
        "PID {0}": "PID {0}",
        "started seconds ago {0}": "{0} 秒前启动",
        "started minutes ago {0}": "{0} 分钟前启动",
        "started hours ago {0}": "{0} 小时前启动",
        "started hours minutes ago {0} {1}": "{0} 小时 {1} 分前启动",
        "Add remote host title": "添加远程主机",
        "Add remote subtitle": "在 Parallels 之外,将可通过 SSH 访问的机器加入列表。",
        "Required info": "必填",
        "Optional fields": "可选 (稍后可在虚拟机设置中修改)",
        "Display name": "显示名称",
        "Display name placeholder": "例如:办公桌面",
        "Display name hint": "在虚拟机列表中显示的别名。",
        "SSH target": "SSH 目标",
        "SSH target placeholder": "例如: ubuntu@192.168.1.10",
        "SSH target hint": "user@host 格式。非默认端口请在添加后于虚拟机设置中指定。",
        "Workdir": "工作目录",
        "Workdir placeholder": "例如: /home/ubuntu/vbox",
        "Workdir hint placeholder": "留空则默认在用户主目录下的 vbox/。",
        "OS label": "操作系统",
        "OS label placeholder": "例如: Ubuntu 24.04",
        "OS label hint": "列表显示的发行版名称,可自动选图标。",
        "Config title": "虚拟机设置",
        "SSH section": "SSH 连接",
        "SSH footer": "对此机器的所有 vbox 命令均通过上述 SSH 信息。",
        "Field user": "用户",
        "Field user placeholder": "例如: ubuntu",
        "Field user hint": "登录客户机的 SSH 账号。",
        "Field host": "主机",
        "Field host placeholder": "例如: 192.168.1.10",
        "Field host hint": "IP 地址或域名。",
        "Field ssh port": "SSH 端口",
        "Field ssh port placeholder": "例如: 22",
        "Field ssh port hint": "仅在非标准 SSH 端口时填写。",
        "Field identity": "私钥文件",
        "Field identity placeholder": "例如: ~/.ssh/vbox_ed25519",
        "Field identity hint": "需要强制指定 SSH 私钥时填写。",
        "Field identity browse": "选择…",
        "Field password": "密码",
        "Field password placeholder": "可选 — 保存在 macOS 钥匙串中",
        "Field password hint": "密码仅保存在本地 macOS 钥匙串中，绝不会以明文形式写入磁盘。",
        "Field password current": "已保存在钥匙串",
        "Field password clear": "清除",
        "Field password change placeholder": "留空保留现有值",
        "Machine info": "机器信息",
        "Identity section": "标识",
        "Custom overrides section": "自定义覆盖",
        "Failed to load info": "无法加载机器信息",
        "Probe section": "连接探测",
        "Run probe": "运行探测",
        "Re-probe": "重新探测",
        "Probing": "探测中…",
        "Reachable": "可达",
        "Unreachable": "不可达",
        "Guest path section": "客户机路径",
        "Field guestdir": "工作目录",
        "Field guestdir placeholder": "例如: /home/ubuntu/vbox",
        "Field guestdir hint": "客户机上存放 vbox 源代码和二进制文件的目录。",
        "Viewer section": "连接 & 查看器",
        "Viewer footer": "留空则由 vbox 自动选择合适的值。",
        "Field port": "服务端口",
        "Field port placeholder": "例如: 5710",
        "Field port hint": "vbox 数据通道使用的端口。",
        "Field socket": "Wayland socket",
        "Field socket placeholder": "例如: vbox-0",
        "Field socket hint": "客户机的 Wayland 会话名称。",
        "Field width": "窗口宽度 (px)",
        "Field width placeholder": "例如: 1280",
        "Field width hint": "macOS 查看器初始宽度。留空表示自动。",
        "Field height": "窗口高度 (px)",
        "Field height placeholder": "例如: 800",
        "Field height hint": "macOS 查看器初始高度。留空表示自动。",
        "Flags section": "开关",
        "Flag debug": "调试日志",
        "Flag debug hint": "详细记录连接/输入/窗口管理的协议消息。",
        "Flag tls": "mTLS 直连",
        "Flag tls hint": "跳过 SSH 隧道,使用证书直接访问 vbox 控制通道。",
        "Memo section": "备注",
        "Memo placeholder": "例如: 办公桌面 — 仅用于 Rust 构建",
        "Memo footer": "个人备注,不影响 vbox 行为。",
        "Current label": "当前",
        "Library fresh": "已是最新",
        "Refresh failed": "刷新失败",
        "Library reloaded {0}": "应用库 + Launchpad 已更新 ({0})",
        "Reload library": "重新加载应用列表",
        "Launchpad bundles refresh": "更新 Launchpad 包",
        "Bundle started {0}": "正在启动 {0} 应用包…",
        "Viewer started {0}": "正在视图中启动 {0}…",
        "Run failed": "启动失败",
        "Bundle run failed {0}": "应用包启动失败: {0}",
        "Bundle missing": "未安装应用包 — 请先获取应用库或开启 Launchpad",
        "Active machine no count {0}": "{0} 活动",
        "Active machine with count {0} {1}": "{0} 活动 — {1} 个应用",
        "Active machine empty cache {0}": "{0} 活动 — 尚未加载应用库",
        "Default guest active": "默认客户机活动",
        "Saved settings {0}": "已保存 {0} 的设置",
        "Save failed": "部分设置保存失败",
        "Add remote failed": "添加远程主机失败",
        "Add remote ok {0}": "已添加远程主机: {0}",
        "Remove remote ok {0}": "已移除远程主机: {0}",
        "Remove remote failed": "移除远程主机失败",
        "Search": "搜索",
        "Bundle progress phase {0} {1}": "构建应用包 ({0}/{1})",
        "Bundle progress phase with name {0} {1} {2}": "构建应用包 ({0}/{1}) · {2}",
        "Apps count {0}": "{0} 个应用",
        "Toolbar refresh icons help": "刷新图标",
        "Toolbar reload help": "重新加载应用库并重建 Launchpad 包",
        "Machines toolbar help": "客户机列表 / 选择活动机器",
        "View running help": "查看客户机上运行中的进程",
        "Global settings": "全局设置",
        "Show macOS titlebar": "显示 macOS 标题栏",
        "Titlebar hint": "关闭后查看器窗口为无边框。Firefox/Chrome 等自带标题栏的应用关闭可能更自然。",
        "Settings reapply hint": "改动会在下次启动查看器时生效 — 已打开的窗口保持原状。",
    ]

    static let ja: [String: String] = [
        "Machines": "マシン",
        "Guest Machines": "ゲストマシン",
        "Machines subtitle": "Parallels VM + リモート SSH ホスト",
        "Refresh": "更新",
        "Close": "閉じる",
        "Add": "追加",
        "Cancel": "キャンセル",
        "Save": "保存",
        "Saving": "保存中…",
        "Loading": "読み込み中…",
        "Add remote host": "リモートホストを追加",
        "Remove remote host": "リモートホストを削除",
        "Machine settings": "マシン設定",
        "No machines": "マシンなし",
        "No machines hint": "Parallels VM またはリモートホストを追加してください。",
        "Add a remote host": "リモートホストを追加",
        "machines found {0}": "{0} 台見つかりました",
        "Status running": "実行中",
        "Status stopped": "停止",
        "Status suspended": "サスペンド",
        "Status paused": "一時停止",
        "Status invalid": "無効/エラー",
        "Status remote": "リモート",
        "Status unknown": "不明",
        "Active": "アクティブ",
        "Tap to activate guest": "タップでアクティブなゲストに設定",
        "Cannot activate": "選択不可 (Linux + 実行中 + IP が必要)",
        "Activate guest": "このマシンをアクティブに設定",
        "Cannot select": "選択不可 (Linux + 実行中が必要)",
        "Power off VM": "VM を停止",
        "Power on VM": "VM を起動",
        "Open settings": "マシン設定を開く",
        "Bundle": "アプリバンドル",
        "Run in viewer": "ビューで実行",
        "Bundle help installed": "独立した macOS アプリとして実行 (Launchpad/Dock 表示)",
        "Bundle help missing": "Launchpad トグルでバンドルをインストールしてください",
        "Viewer help": "開いているビューアに追加、なければ新規起動",
        "Library empty title": "このマシンのアプリは未取得です",
        "Library empty subtitle": "{0} の .desktop アプリ一覧を取得するとここに表示されます。",
        "Fetch this machine's library": "このマシンのアプリ一覧を取得",
        "Refresh icons": "アイコンを更新",
        "Reload library + Launchpad": "アプリ一覧の再読込 + Launchpad バンドル更新",
        "Running": "実行中",
        "Running with count": "実行中 ({0})",
        "Launchpad": "Launchpad",
        "Command": "コマンド",
        "Category": "カテゴリ",
        "Running processes": "実行中のプロセス",
        "Processes subtitle": "ゲストの Wayland セッション ({0}) に接続中のプロセス",
        "Refresh processes": "プロセスを更新",
        "No running apps": "実行中のアプリはありません",
        "No running apps subtitle": "アプリ一覧から起動するとここに表示されます。",
        "Auto refresh hint": "5 秒ごとに自動更新",
        "End": "終了",
        "PID {0}": "PID {0}",
        "started seconds ago {0}": "{0} 秒前に開始",
        "started minutes ago {0}": "{0} 分前に開始",
        "started hours ago {0}": "{0} 時間前に開始",
        "started hours minutes ago {0} {1}": "{0} 時間 {1} 分前に開始",
        "Add remote host title": "リモートホストを追加",
        "Add remote subtitle": "Parallels 以外で SSH 接続可能なマシンを登録します。",
        "Required info": "必須",
        "Optional fields": "任意 (あとでマシン設定から変更可)",
        "Display name": "表示名",
        "Display name placeholder": "例: 事務所デスクトップ",
        "Display name hint": "マシン一覧に表示される名前。自由に。",
        "SSH target": "SSH ターゲット",
        "SSH target placeholder": "例: ubuntu@192.168.1.10",
        "SSH target hint": "user@host 形式。非標準ポートは追加後にマシン設定で指定。",
        "Workdir": "作業ディレクトリ",
        "Workdir placeholder": "例: /home/ubuntu/vbox",
        "Workdir hint placeholder": "空欄ならホーム下の vbox/ に自動設定。",
        "OS label": "OS ラベル",
        "OS label placeholder": "例: Ubuntu 24.04",
        "OS label hint": "一覧に表示する OS 名。ディストロ名でアイコン自動選択。",
        "Config title": "マシン設定",
        "SSH section": "SSH 接続",
        "SSH footer": "このマシンへの vbox コマンドは上記 SSH 経由で通信します。",
        "Field user": "ユーザー",
        "Field user placeholder": "例: ubuntu",
        "Field user hint": "ゲストに SSH ログインするアカウント名。",
        "Field host": "ホスト",
        "Field host placeholder": "例: 192.168.1.10",
        "Field host hint": "IP アドレスまたはドメイン。",
        "Field ssh port": "SSH ポート",
        "Field ssh port placeholder": "例: 22",
        "Field ssh port hint": "非標準ポートを使う場合のみ入力。",
        "Field identity": "秘密鍵ファイル",
        "Field identity placeholder": "例: ~/.ssh/vbox_ed25519",
        "Field identity hint": "特定の SSH 秘密鍵を強制する場合のみ。",
        "Field identity browse": "選択…",
        "Field password": "パスワード",
        "Field password placeholder": "任意 — macOS キーチェーンに保存",
        "Field password hint": "パスワードはローカルの macOS キーチェーンにのみ保存され、平文でディスクに書き込まれることはありません。",
        "Field password current": "キーチェーンに保存済み",
        "Field password clear": "クリア",
        "Field password change placeholder": "空欄のままで既存の値を維持",
        "Machine info": "マシン情報",
        "Identity section": "識別情報",
        "Custom overrides section": "カスタム設定",
        "Failed to load info": "情報の読み込みに失敗しました",
        "Probe section": "接続診断",
        "Run probe": "診断を実行",
        "Re-probe": "再診断",
        "Probing": "診断中…",
        "Reachable": "到達可能",
        "Unreachable": "到達不可",
        "Guest path section": "ゲストパス",
        "Field guestdir": "作業ディレクトリ",
        "Field guestdir placeholder": "例: /home/ubuntu/vbox",
        "Field guestdir hint": "ゲスト上で vbox のソースとバイナリを置く場所。",
        "Viewer section": "接続 & ビューア",
        "Viewer footer": "空欄なら vbox が自動で適切な値を選びます。",
        "Field port": "サーバーポート",
        "Field port placeholder": "例: 5710",
        "Field port hint": "vbox データチャネルで使うポート。",
        "Field socket": "Wayland ソケット",
        "Field socket placeholder": "例: vbox-0",
        "Field socket hint": "ゲストの Wayland セッション名。",
        "Field width": "ウィンドウ幅 (px)",
        "Field width placeholder": "例: 1280",
        "Field width hint": "macOS ビューア初期幅。空ならオート。",
        "Field height": "ウィンドウ高さ (px)",
        "Field height placeholder": "例: 800",
        "Field height hint": "macOS ビューア初期高さ。空ならオート。",
        "Flags section": "フラグ",
        "Flag debug": "デバッグログ",
        "Flag debug hint": "接続/入力/ウィンドウ管理のワイヤメッセージを詳細に記録。",
        "Flag tls": "mTLS 直接接続",
        "Flag tls hint": "SSH トンネルなしで証明書を使い vbox 制御チャネルに直接接続。",
        "Memo section": "メモ",
        "Memo placeholder": "例: 事務所デスクトップ — Rust ビルド専用",
        "Memo footer": "vbox の動作には影響しない個人メモ。",
        "Current label": "現在",
        "Library fresh": "最新の状態",
        "Refresh failed": "更新失敗",
        "Library reloaded {0}": "アプリ一覧 + Launchpad を更新 ({0} 個)",
        "Reload library": "アプリ一覧を再読込",
        "Launchpad bundles refresh": "Launchpad バンドルを更新中",
        "Bundle started {0}": "{0} バンドルを起動中…",
        "Viewer started {0}": "{0} をビューで起動中…",
        "Run failed": "起動失敗",
        "Bundle run failed {0}": "バンドル起動失敗: {0}",
        "Bundle missing": "バンドル未インストール — 先にアプリ一覧取得 / Launchpad トグルを",
        "Active machine no count {0}": "{0} アクティブ",
        "Active machine with count {0} {1}": "{0} アクティブ — {1} アプリ",
        "Active machine empty cache {0}": "{0} アクティブ — まだ未読み込み",
        "Default guest active": "デフォルトゲストがアクティブ",
        "Saved settings {0}": "{0} の設定を保存しました",
        "Save failed": "一部キーの保存に失敗",
        "Add remote failed": "リモートホストの追加に失敗",
        "Add remote ok {0}": "リモートホスト追加: {0}",
        "Remove remote ok {0}": "リモートホスト削除: {0}",
        "Remove remote failed": "リモートホストの削除に失敗",
        "Search": "検索",
        "Bundle progress phase {0} {1}": "バンドル作成 ({0}/{1})",
        "Bundle progress phase with name {0} {1} {2}": "バンドル作成 ({0}/{1}) · {2}",
        "Apps count {0}": "{0} アプリ",
        "Toolbar refresh icons help": "アイコンを更新",
        "Toolbar reload help": "アプリ一覧を再読込 + Launchpad バンドル更新",
        "Machines toolbar help": "ゲストマシン一覧 / アクティブ選択",
        "View running help": "ゲストで実行中のプロセスを表示",
        "Global settings": "グローバル設定",
        "Show macOS titlebar": "macOS タイトルバーを表示",
        "Titlebar hint": "オフにするとビューア窓がボーダーレスに。Firefox/Chrome のように独自ヘッダがあるアプリは自然に感じることが多い。",
        "Settings reapply hint": "変更は次にビューアを起動した時から反映。すでに開いている窓は元のまま。",
    ]
}

struct AppConfig: Sendable {
    let root: String
    let cliPath: String
    let stateDir: String
    let launcherDir: String
    let iconCacheDir: String
    let distroIconDir: String
    let guest: String
    let guestDir: String
    let port: String
    let socket: String
    let width: String
    let height: String
    let suffix: String

    static func load() -> AppConfig {
        AppConfig(
            root: readResource("Root", fallback: ""),
            cliPath: readResource("CliPath", fallback: ""),
            stateDir: readResource("StateDir", fallback: ""),
            launcherDir: readResource("LauncherDir", fallback: ""),
            iconCacheDir: readResource("IconCacheDir", fallback: ""),
            distroIconDir: readResource("DistroIconDir", fallback: ""),
            guest: readResource("Guest", fallback: ""),
            guestDir: readResource("GuestDir", fallback: ""),
            port: readResource("Port", fallback: "5710"),
            socket: readResource("Socket", fallback: "vbox-0"),
            width: readResource("Width", fallback: "1024"),
            height: readResource("Height", fallback: "768"),
            suffix: readResource("Suffix", fallback: "")
        )
    }

    private static func readResource(_ name: String, fallback: String) -> String {
        guard let url = Bundle.main.url(forResource: name, withExtension: "txt"),
              let value = try? String(contentsOf: url, encoding: .utf8) else {
            return fallback
        }
        return value.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct GuestApp: Identifiable, Equatable {
    let id: String
    let name: String
    let execLine: String
    let icon: String
    let desktop: String
    let categories: String
    let argvB64: String
    var installed: Bool
}

struct RunningProcess: Identifiable, Equatable, Hashable {
    let pid: Int32
    let name: String
    let command: String
    let startedAt: Date

    var id: Int32 { pid }
}

struct CommandResult: Sendable {
    let status: Int32
    let output: String
}

@MainActor
final class LibraryModel: ObservableObject {
    @Published var apps: [GuestApp] = []
    @Published var search = ""
    @Published var selectedID: String?
    @Published var isWorking = false
    @Published var status = ""
    // Launchpad 번들 일괄 설치 진행률. value 0..1, nil 이면 indeterminate 또는 진행 안 함.
    @Published var bundleProgress: Double? = nil
    @Published var bundleProgressLabel: String = ""
    @Published var runningProcesses: [RunningProcess] = []
    @Published var selectedRunningPID: Int32?
    @Published var isRefreshingProcesses = false
    @Published var processesStatus = ""

    // active guest override — MachinesModel 이 set 하면 모든 외부 vbox 호출이
    // 이 guest 의 ssh user/host/dir 을 사용. nil 이면 AppConfig 기본 값.
    @Published var activeGuestOverride: GuestMachine? = nil

    private var pollingTask: Task<Void, Never>?

    let config = AppConfig.load()

    // 현재 활성 guest 의 ssh string (예: "pista@10.211.55.11"). 표시용.
    var activeGuestString: String {
        if let g = activeGuestOverride { return g.sshString }
        return config.guest
    }

    var filteredApps: [GuestApp] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return apps }
        return apps.filter { app in
            "\(app.name) \(app.id) \(app.categories)".lowercased().contains(query)
        }
    }

    var selectedApp: GuestApp? {
        if let selectedID, let app = apps.first(where: { $0.id == selectedID }) {
            return app
        }
        return filteredApps.first
    }

    init() {
        reload()
    }

    func reload() {
        let loaded = loadApps()
        apps = loaded
        if let selectedID, loaded.contains(where: { $0.id == selectedID }) {
            return
        }
        selectedID = loaded.first?.id
    }

    func refresh() {
        guard !isWorking else { return }
        isWorking = true
        status = L("Refresh")
        Task {
            let result = await runVBoxNative(["cache-icons", "--refresh"])
            reload()
            status = result.status == 0 ? L("Library fresh") : trimmedOutput(result.output, fallback: L("Refresh failed"))
            isWorking = false
        }
    }

    func loadLibrary() {
        guard !isWorking else { return }
        isWorking = true
        status = L("Reload library")
        Task {
            // Step 1: refresh the guest app library so newly-installed GNOME
            // apps show up.
            let refresh = await runVBoxNative(["library", "--refresh"])
            if refresh.status != 0 {
                reload()
                status = trimmedOutput(refresh.output, fallback: L("Refresh failed"))
                isWorking = false
                return
            }
            // 새 cache 가 들어왔으니 미리 reload — install 진행률의 분모 (total) 를 알기 위해서도.
            reload()
            let total = apps.count
            // Step 2: install-apps 를 streaming 으로 호출해 진행률 표시.
            //   bash 측이 각 앱마다 `[vbox] installed: <name> -> <path>` 한 줄을 stdout 으로 출력.
            //   라인 별 callback 에서 done counter 증가 → bundleProgress 갱신.
            status = L("Launchpad bundles refresh")
            bundleProgress = 0
            bundleProgressLabel = "0 / \(total)"
            let cfg = config
            let override = activeGuestOverride
            var doneCount = 0
            let install = await VBoxRunner.runStreaming(["install-apps"], config: cfg, override: override) { line in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    if line.hasPrefix("[vbox] installed:") {
                        // "[vbox] installed: <name> -> <path>"
                        let after = line.replacingOccurrences(of: "[vbox] installed:", with: "")
                        let name = after.components(separatedBy: " -> ").first?
                            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                        doneCount += 1
                        if total > 0 {
                            self.bundleProgress = min(1.0, Double(doneCount) / Double(total))
                        }
                        self.bundleProgressLabel = total > 0
                            ? "\(doneCount) / \(total)"
                            : L("Apps count {0}", arg: "\(doneCount)")
                        self.status = L("Bundle progress phase with name {0} {1} {2}", "\(doneCount)", "\(max(total, doneCount))", name)
                    } else if line.hasPrefix("[vbox] ") {
                        // 빌드/동기화 같은 사전 단계 메시지도 status 에 흘려보냄.
                        // ex) "[vbox] build host client" → "build host client"
                        let trimmed = line.dropFirst("[vbox] ".count).trimmingCharacters(in: .whitespacesAndNewlines)
                        if !trimmed.isEmpty { self.status = trimmed }
                    }
                }
            }
            reload()
            bundleProgress = nil
            bundleProgressLabel = ""
            if install.status == 0 {
                status = L("Library reloaded {0}", arg: "\(apps.count)")
            } else {
                status = trimmedOutput(install.output, fallback: L("Save failed"))
            }
            isWorking = false
        }
    }

    // 기본 동작: 번들이 설치돼 있으면 번들로, 아니면 뷰에서. (호환성 alias)
    func runSelected() {
        guard let app = selectedApp else { return }
        if app.installed {
            runAsBundle(app)
        } else {
            runInViewer(app)
        }
    }

    // (1) 뷰 실행 — 이미 열려있는 viewer 가 있으면 그 위에, 없으면 새 viewer 띄움.
    //     모든 게스트 앱이 단일 macOS 창 (winit + softbuffer) 안에 nested toplevel 로 동작.
    func runInViewer(_ app: GuestApp) {
        guard !isWorking else { return }
        isWorking = true
        status = L("Viewer started {0}", arg: app.name)
        Task {
            let result = await runVBoxNative(["launch-id", app.id])
            if result.status != 0 {
                status = trimmedOutput(result.output, fallback: L("Run failed"))
            } else {
                try? await Task.sleep(nanoseconds: 800_000_000)
                await refreshProcesses(silent: true)
            }
            isWorking = false
        }
    }

    // (2) 앱 번들 실행 — ~/Applications/vbox/<App>.app 을 독립 macOS 앱처럼 띄움.
    //     Launchpad/Dock 에서 보이고 자체 winit 창. install-apps 로 번들이 만들어져 있어야.
    func runAsBundle(_ app: GuestApp) {
        guard !isWorking else { return }
        let bundlePath = launcherPath(for: app)
        guard FileManager.default.fileExists(atPath: bundlePath) else {
            status = L("Bundle missing")
            return
        }
        isWorking = true
        status = L("Bundle started {0}", arg: app.name)
        Task {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            process.arguments = ["-n", bundlePath]
            do {
                try process.run()
                process.waitUntilExit()
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                await refreshProcesses(silent: true)
            } catch {
                status = L("Bundle run failed {0}", arg: error.localizedDescription)
            }
            isWorking = false
        }
    }

    var selectedProcess: RunningProcess? {
        guard let selectedRunningPID else { return runningProcesses.first }
        return runningProcesses.first { $0.pid == selectedRunningPID }
    }

    func refreshProcesses(silent: Bool = false) async {
        if !silent {
            isRefreshingProcesses = true
        }
        let result = await runVBoxNative(["processes"])
        if result.status == 0 {
            let parsed = parseProcesses(result.output)
            runningProcesses = parsed
            if let selectedRunningPID, !parsed.contains(where: { $0.pid == selectedRunningPID }) {
                self.selectedRunningPID = parsed.first?.pid
            } else if selectedRunningPID == nil {
                selectedRunningPID = parsed.first?.pid
            }
            processesStatus = parsed.isEmpty ? L("No running apps") : L("Running with count", arg: "\(parsed.count)")
        } else {
            processesStatus = trimmedOutput(result.output, fallback: L("Refresh failed"))
        }
        if !silent {
            isRefreshingProcesses = false
        }
    }

    func killProcess(_ process: RunningProcess) {
        guard !isRefreshingProcesses else { return }
        let pid = process.pid
        processesStatus = L("PID {0}", arg: "\(pid)") + " — " + L("End")
        Task {
            isRefreshingProcesses = true
            let result = await runVBoxNative(["kill-pid", String(pid)])
            if result.status != 0 {
                processesStatus = trimmedOutput(result.output, fallback: L("Run failed"))
            }
            await refreshProcesses(silent: true)
            isRefreshingProcesses = false
        }
    }

    func startPollingProcesses(interval: UInt64 = 2_000_000_000) {
        guard pollingTask == nil else { return }
        pollingTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshProcesses(silent: true)
                try? await Task.sleep(nanoseconds: interval)
            }
        }
    }

    func stopPollingProcesses() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    private func parseProcesses(_ text: String) -> [RunningProcess] {
        text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line -> RunningProcess? in
                let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
                guard parts.count >= 4, let pid = Int32(parts[0]) else { return nil }
                let started = Double(parts[1]).map { Date(timeIntervalSince1970: $0) } ?? Date()
                return RunningProcess(pid: pid, name: parts[2], command: parts[3], startedAt: started)
            }
            .sorted { lhs, rhs in
                if lhs.startedAt == rhs.startedAt { return lhs.pid < rhs.pid }
                return lhs.startedAt > rhs.startedAt
            }
    }

    func setLaunchpad(_ app: GuestApp, enabled: Bool) {
        guard !isWorking else { return }
        isWorking = true
        status = enabled ? "Launchpad" : "Launchpad"
        Task {
            let command = enabled ? "install-launcher" : "remove-launcher"
            let result = await runVBoxNative([command, app.id])
            reload()
            if result.status == 0 {
                status = enabled ? "Launchpad ✓" : "Launchpad ✗"
            } else {
                status = trimmedOutput(result.output, fallback: L("Save failed"))
            }
            isWorking = false
        }
    }

    func icon(for app: GuestApp) -> NSImage? {
        let launcherIcon = launcherPath(for: app).appending("/Contents/Resources/AppIcon.icns")
        if FileManager.default.fileExists(atPath: launcherIcon) {
            return NSImage(contentsOfFile: launcherIcon)
        }

        let safeID = sanitizeID(app.id)
        for dir in [activeIconDir, config.iconCacheDir] where !dir.isEmpty {
            for ext in ["png", "jpg", "jpeg", "tiff", "tif", "icns"] {
                let path = "\(dir)/\(safeID).\(ext)"
                if FileManager.default.fileExists(atPath: path), let image = NSImage(contentsOfFile: path) {
                    return image
                }
            }
        }
        return nil
    }

    // 활성 머신용 cache dir. VBOX_GUEST 의 sanitize 결과 디렉토리.
    // bash 의 sanitize_guest_id() 와 동일 규칙: '@'→'_at_', 그 외 비-alnum/./-/_ → '_', 연속 '_' 합치기.
    private var activeMachineDir: String {
        "\(config.stateDir)/machines/\(LibraryModel.sanitizeGuestId(activeGuestString))"
    }

    private var activeIconDir: String { "\(activeMachineDir)/icons" }

    static func sanitizeGuestId(_ guest: String) -> String {
        let withAt = guest.replacingOccurrences(of: "@", with: "_at_")
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        var out = ""
        var prevUnderscore = false
        for sc in withAt.unicodeScalars {
            if allowed.contains(sc) {
                out.unicodeScalars.append(sc)
                prevUnderscore = false
            } else if !prevUnderscore {
                out.append("_")
                prevUnderscore = true
            }
        }
        return out
    }

    private func loadApps() -> [GuestApp] {
        let cachePath = "\(activeMachineDir)/app-library.tsv"
        let fallbackCachePath = "\(config.stateDir)/app-library.tsv"
        let effectiveCachePath = FileManager.default.fileExists(atPath: cachePath) ? cachePath : fallbackCachePath
        guard let text = try? String(contentsOfFile: effectiveCachePath, encoding: .utf8) else {
            return []
        }

        return text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line in
                let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
                guard parts.count >= 7 else { return nil }
                let base = GuestApp(
                    id: parts[0],
                    name: parts[1],
                    execLine: parts[2],
                    icon: parts[3],
                    desktop: parts[4],
                    categories: parts[5],
                    argvB64: parts[6],
                    installed: false
                )
                var app = base
                app.installed = launcherInstalled(for: base)
                return app
            }
    }

    private func launcherInstalled(for app: GuestApp) -> Bool {
        let appPath = launcherPath(for: app)
        let macosPath = "\(appPath)/Contents/MacOS"
        let hasClient = (try? FileManager.default.contentsOfDirectory(atPath: macosPath))?
            .contains(where: { item in item.hasPrefix("vbox-client") }) ?? false
        return hasClient
            && FileManager.default.fileExists(atPath: "\(appPath)/Contents/Info.plist")
            && FileManager.default.fileExists(atPath: "\(appPath)/Contents/Resources/AppID.txt")
    }

    private func launcherPath(for app: GuestApp) -> String {
        let display: String
        if config.suffix.isEmpty {
            display = app.name
        } else {
            display = "\(app.name) (\(config.suffix))"
        }
        return "\(config.launcherDir)/\(safeAppFilename(display)).app"
    }

    private func safeAppFilename(_ value: String) -> String {
        value
            .replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: "-")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func sanitizeID(_ value: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        var result = ""
        var previousWasDash = false
        for scalar in value.unicodeScalars {
            if allowed.contains(scalar) {
                result.unicodeScalars.append(scalar)
                previousWasDash = false
            } else if !previousWasDash {
                result.append("-")
                previousWasDash = true
            }
        }
        return result.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    }

    private func runVBoxNative(_ args: [String]) async -> CommandResult {
        let config = self.config
        let override = self.activeGuestOverride
        return await Task.detached(priority: .userInitiated) {
            await VBoxRunner.run(args, config: config, override: override)
        }.value
    }

    private func trimmedOutput(_ output: String, fallback: String) -> String {
        let text = output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return fallback }
        return String(text.prefix(180))
    }
}

// Single source of truth for cross-view UserDefaults keys used by this
// launcher. Keeping the literal strings here means a typo can't drift
// between LibraryWindow's @AppStorage and VBoxRunner's manual read.
enum VBoxDefaults {
    /// Mirrors the VBOX_HOST_CHROME env probe in the Rust viewer
    /// (see crates/client/src/viewer/env.rs::host_chrome_enabled).
    /// Default: true (standard macOS titlebar). VBoxRunner reads this
    /// key on every launch to inject VBOX_HOST_CHROME into the viewer
    /// child process.
    static let hostChromeKey = "vbox.hostChrome"
}

struct LibraryWindow: View {
    @StateObject private var model: LibraryModel
    @StateObject private var machinesModel: MachinesModel
    @State private var showRunning = false
    @State private var showMachines = false
    @State private var showSettings = false
    // 머신 설정 sheet 의 nested-sheet 제약 회피 — machines sheet 닫고 띄움.
    @State private var configMachine: GuestMachine? = nil
    @State private var infoMachine: GuestMachine? = nil
    @State private var showAddRemoteAtRoot = false
    @AppStorage(VBoxDefaults.hostChromeKey) private var hostChromeEnabled: Bool = true

    init() {
        let lib = LibraryModel()
        _model = StateObject(wrappedValue: lib)
        _machinesModel = StateObject(wrappedValue: MachinesModel(config: lib.config))
    }

    var body: some View {
        NavigationSplitView {
            appList
                .navigationSplitViewColumnWidth(min: 280, ideal: 330, max: 420)
        } detail: {
            detail
        }
        .searchable(text: $model.search, placement: .sidebar, prompt: L("Search"))
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button {
                    model.refresh()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(model.isWorking)
                .help(L("Refresh icons"))
            }

            ToolbarItem(placement: .navigation) {
                Button {
                    model.loadLibrary()
                } label: {
                    Image(systemName: "square.and.arrow.down")
                }
                .disabled(model.isWorking)
                .help(L("Reload library + Launchpad"))
            }

            ToolbarItem(placement: .principal) {
                Button {
                    showMachines = true
                } label: {
                    Label(machinesButtonLabel, systemImage: "rectangle.3.group")
                }
                .help(L("Machines toolbar help"))
            }

            ToolbarItem(placement: .primaryAction) {
                Button {
                    showRunning = true
                } label: {
                    Label(runningButtonLabel, systemImage: "bolt.horizontal.circle")
                }
                .help(L("View running help"))
            }

            ToolbarItem(placement: .primaryAction) {
                Button {
                    showSettings.toggle()
                } label: {
                    Image(systemName: "gearshape")
                }
                .help(L("Global settings"))
                .popover(isPresented: $showSettings, arrowEdge: .bottom) {
                    SettingsPopover(hostChromeEnabled: $hostChromeEnabled)
                }
            }
            // The big "실행" button is already in the detail pane next to
            // the selected app; a second toolbar copy was redundant, so
            // it was removed.
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(.regularMaterial)
        .overlay(WindowConfigurator().allowsHitTesting(false))
        .sheet(isPresented: $showRunning) {
            RunningProcessesSheet(model: model)
                .frame(minWidth: 560, minHeight: 380)
        }
        .sheet(isPresented: $showMachines) {
            MachinesSheet(
                model: machinesModel,
                activeUUID: model.activeGuestOverride?.uuid,
                onConfigRequest: { machine in
                    showMachines = false
                    configMachine = machine
                },
                onAddRemoteRequest: {
                    showMachines = false
                    showAddRemoteAtRoot = true
                },
                onInfoRequest: { machine in
                    showMachines = false
                    infoMachine = machine
                })
        }
        .sheet(item: $configMachine) { machine in
            MachineConfigSheet(machine: machine, model: machinesModel)
        }
        .sheet(item: $infoMachine) { machine in
            MachineInfoSheet(machine: machine, model: machinesModel)
        }
        .sheet(isPresented: $showAddRemoteAtRoot) {
            AddRemoteSheet { name, ssh, dir, osRaw, identityFile, password in
                Task {
                    let ok = await machinesModel.addRemote(name: name, ssh: ssh,
                                                            guestDir: dir, osRaw: osRaw,
                                                            identityFile: identityFile,
                                                            password: password)
                    if ok { showAddRemoteAtRoot = false }
                }
            }
        }
        .task {
            machinesModel.onSelect = { machine in
                // 선택이 바뀌면 LibraryModel 의 override 를 교체하고, 새 guest 의
                // app cache + 프로세스를 다시 로드. 머신마다 cache 가 분리됨.
                model.activeGuestOverride = machine
                model.reload()
                Task { await model.refreshProcesses(silent: true) }
                // status 라인에 활성 머신 + 캐시 상태 표기.
                if let m = machine {
                    let count = model.apps.count
                    model.status = count > 0
                        ? L("Active machine with count {0} {1}", m.name, "\(count)")
                        : L("Active machine empty cache {0}", arg: m.name)
                } else {
                    model.status = L("Default guest active")
                }
            }
            model.startPollingProcesses()
            await machinesModel.refresh()
        }
        .onDisappear {
            model.stopPollingProcesses()
        }
    }

    private var machinesButtonLabel: String {
        let base: String
        if let g = model.activeGuestOverride {
            base = g.name
        } else {
            let host = model.config.guest.split(separator: "@").last.map(String.init) ?? model.config.guest
            base = host.isEmpty ? L("Machines") : host
        }
        let n = model.apps.count
        return n > 0 ? base + " · " + L("Apps count {0}", arg: "\(n)") : base
    }

    private var runningButtonLabel: String {
        let count = model.runningProcesses.count
        return count > 0 ? "실행 중 (\(count))" : L("Status running")
    }

    private var appList: some View {
        List(selection: $model.selectedID) {
            ForEach(model.filteredApps) { app in
                AppRow(app: app, image: model.icon(for: app))
                    .tag(Optional(app.id))
            }
        }
        .listStyle(.sidebar)
    }

    @ViewBuilder
    private var detail: some View {
        if let app = model.selectedApp {
            AppDetail(app: app, image: model.icon(for: app), model: model)
        } else {
            EmptyLibraryView(model: model)
        }
    }
}

// 머신 cache 가 비었을 때 detail 영역. 사용자가 "왜 비었는지" 알게 하고,
// 한 번에 그 머신의 라이브러리를 받아올 수 있는 진입점 제공.
struct EmptyLibraryView: View {
    @ObservedObject var model: LibraryModel

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "square.grid.2x2")
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(.secondary)
            Text(L("Library empty title"))
                .font(.title3.weight(.semibold))
            Text(L("Library empty subtitle", arg: model.activeGuestString))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            Button {
                model.loadLibrary()
            } label: {
                Label(L("Fetch this machine's library"), systemImage: "square.and.arrow.down")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(model.isWorking)
            if model.isWorking {
                VStack(spacing: 6) {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(model.status).font(.caption).foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    if let progress = model.bundleProgress {
                        ProgressView(value: progress)
                            .progressViewStyle(.linear)
                            .frame(maxWidth: 320)
                        Text(model.bundleProgressLabel)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.regularMaterial)
    }
}

struct AppRow: View {
    let app: GuestApp
    let image: NSImage?

    var body: some View {
        HStack(spacing: 12) {
            AppIcon(image: image, size: 34)

            VStack(alignment: .leading, spacing: 2) {
                Text(app.name)
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                Text(app.id)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            Spacer(minLength: 8)

            Image(systemName: app.installed ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(app.installed ? Color.green : Color.secondary)
                .font(.system(size: 14, weight: .semibold))
        }
        .padding(.vertical, 5)
    }
}

struct AppDetail: View {
    let app: GuestApp
    let image: NSImage?
    @ObservedObject var model: LibraryModel

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack(alignment: .center, spacing: 18) {
                AppIcon(image: image, size: 72)

                VStack(alignment: .leading, spacing: 5) {
                    Text(app.name)
                        .font(.largeTitle.weight(.semibold))
                        .lineLimit(1)
                    Text(app.id)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }

                Spacer()

                runButtons(for: app)
            }

            Divider()

            HStack(spacing: 14) {
                Label("Launchpad", systemImage: "square.grid.2x2.fill")
                    .font(.title3.weight(.semibold))
                Spacer()
                Toggle("", isOn: Binding(
                    get: { app.installed },
                    set: { enabled in model.setLaunchpad(app, enabled: enabled) }
                ))
                .toggleStyle(.switch)
                .labelsHidden()
                .disabled(model.isWorking)
            }

            VStack(alignment: .leading, spacing: 8) {
                Text(L("Command"))
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Text(app.execLine)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(2)
            }

            if !app.categories.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L("Category"))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Text(app.categories)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }

            Spacer()

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    if model.isWorking {
                        ProgressView()
                            .controlSize(.small)
                    }
                    Text(model.status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer()
                    if !model.bundleProgressLabel.isEmpty {
                        Text(model.bundleProgressLabel)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
                if let progress = model.bundleProgress {
                    ProgressView(value: progress)
                        .progressViewStyle(.linear)
                }
            }
        }
        .padding(28)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.regularMaterial)
    }

    // 앱을 두 가지 모드로 실행. 번들 미설치면 그 버튼은 disabled + 안내.
    @ViewBuilder
    private func runButtons(for app: GuestApp) -> some View {
        HStack(spacing: 8) {
            Button {
                model.runAsBundle(app)
            } label: {
                Label(L("Bundle"), systemImage: "app.badge")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
            .disabled(!app.installed || model.isWorking)
            .help(app.installed
                  ? L("Bundle help installed")
                  : L("Bundle help missing"))

            Button {
                model.runInViewer(app)
            } label: {
                Label(L("Run in viewer"), systemImage: "rectangle.stack.badge.plus")
            }
            .controlSize(.large)
            .buttonStyle(.bordered)
            .disabled(model.isWorking)
            .help(L("Viewer help"))
        }
    }
}

struct RunningProcessesSheet: View {
    @ObservedObject var model: LibraryModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .background(.regularMaterial)
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "bolt.horizontal.circle.fill")
                .font(.title2)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(L("Running processes"))
                    .font(.headline)
                Text(L("Processes subtitle", arg: model.config.socket))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                Task { await model.refreshProcesses() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .disabled(model.isRefreshingProcesses)
            .help(L("Refresh processes"))

            Button(L("Close")) { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    @ViewBuilder
    private var content: some View {
        if model.runningProcesses.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "moon.zzz")
                    .font(.system(size: 32, weight: .semibold))
                    .foregroundStyle(.secondary)
                Text(L("No running apps"))
                    .font(.title3.weight(.semibold))
                Text(L("No running apps subtitle"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(28)
        } else {
            List(selection: $model.selectedRunningPID) {
                ForEach(model.runningProcesses) { proc in
                    ProcessRow(
                        proc: proc,
                        disabled: model.isRefreshingProcesses,
                        onKill: { model.killProcess(proc) }
                    )
                    .tag(Optional(proc.pid))
                }
            }
            .listStyle(.inset)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if model.isRefreshingProcesses {
                ProgressView().controlSize(.small)
            }
            Text(model.processesStatus)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer()
            Text(L("Auto refresh hint"))
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
    }
}

struct ProcessRow: View {
    let proc: RunningProcess
    let disabled: Bool
    let onKill: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "app.fill")
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 28, height: 28)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(proc.name)
                        .font(.body.weight(.semibold))
                        .lineLimit(1)
                    Text("PID \(proc.pid)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(.quaternary, in: Capsule())
                }
                Text(proc.command)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            Spacer(minLength: 8)

            VStack(alignment: .trailing, spacing: 4) {
                Text(uptimeText(from: proc.startedAt))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Button(role: .destructive) {
                    onKill()
                } label: {
                    Label(L("End"), systemImage: "stop.fill")
                }
                .controlSize(.small)
                .buttonStyle(.bordered)
                .tint(.red)
                .disabled(disabled)
            }
        }
        .padding(.vertical, 4)
    }

    private func uptimeText(from start: Date) -> String {
        let seconds = Int(Date().timeIntervalSince(start))
        if seconds < 60 { return L("started seconds ago {0}", arg: "\(seconds)") }
        if seconds < 3600 { return L("started minutes ago {0}", arg: "\(seconds / 60)") }
        let h = seconds / 3600
        let m = (seconds % 3600) / 60
        return m == 0 ? L("started hours ago {0}", arg: "\(h)") : L("started hours minutes ago {0} {1}", "\(h)", "\(m)")
    }
}

struct AppIcon: View {
    let image: NSImage?
    let size: CGFloat

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
            } else {
                ZStack {
                    RoundedRectangle(cornerRadius: size * 0.22, style: .continuous)
                        .fill(.thinMaterial)
                    Image(systemName: "app.dashed")
                        .font(.system(size: size * 0.45, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: size, height: size)
    }
}

// Global settings panel surfaced from the toolbar gear icon. Today this
// holds the macOS titlebar toggle (mirrors VBOX_HOST_CHROME in the Rust
// viewer); future cross-cutting preferences should land here too rather
// than scattering toggles across the toolbar.
struct SettingsPopover: View {
    @Binding var hostChromeEnabled: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label(L("Global settings"), systemImage: "gearshape")
                .font(.headline)
                .labelStyle(.titleAndIcon)

            VStack(alignment: .leading, spacing: 6) {
                Toggle(isOn: $hostChromeEnabled) {
                    Label(
                        L("Show macOS titlebar"),
                        systemImage: hostChromeEnabled ? "macwindow" : "rectangle.dashed"
                    )
                }
                .toggleStyle(.switch)
                Text(L("Titlebar hint"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(L("Settings reapply hint"))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(16)
        .frame(width: 320)
    }
}

struct WindowConfigurator: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            window.titlebarAppearsTransparent = true
            window.toolbarStyle = .unified
            window.isMovableByWindowBackground = true
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - VBoxRunner — 외부 ./vbox 호출 어댑터 (단일 책임)
// 기존 LibraryModel 의 인라인 Process 실행을 분리. activeGuestOverride 가 있으면
// 그 머신의 ssh user/host/dir 을 환경변수로 주입. 그 외에는 AppConfig 기본 값.
// ───────────────────────────────────────────────────────────────────────────

enum VBoxRunner {
    // 환경변수 구성. run() 과 runStreaming() 둘 다 같은 환경을 쓴다.
    private static func makeEnvironment(config: AppConfig, override: GuestMachine?) -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        env["VBOX_STATE_DIR"] = config.stateDir
        env["VBOX_GUEST"] = override?.sshString ?? config.guest
        env["VBOX_GUEST_DIR"] = override?.guestDir.isEmpty == false ? override!.guestDir : config.guestDir
        env["VBOX_PORT"] = config.port
        env["VBOX_SOCKET"] = config.socket
        env["VBOX_WIDTH"] = config.width
        env["VBOX_HEIGHT"] = config.height
        let hostChromeOn = UserDefaults.standard.object(forKey: VBoxDefaults.hostChromeKey) as? Bool ?? true
        env["VBOX_HOST_CHROME"] = hostChromeOn ? "1" : "0"
        return env
    }

    static func run(_ args: [String], config: AppConfig, override: GuestMachine?,
                    stdinText: String? = nil) async -> CommandResult {
        let process = makeProcess(args: args, config: config, override: override)
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        let stdinPipe: Pipe? = stdinText != nil ? Pipe() : nil
        process.standardInput = stdinPipe

        do {
            try process.run()
            if let stdinPipe, let data = stdinText?.data(using: .utf8) {
                stdinPipe.fileHandleForWriting.write(data)
                try? stdinPipe.fileHandleForWriting.close()
            }
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8) ?? ""
            return CommandResult(status: process.terminationStatus, output: output)
        } catch {
            return CommandResult(status: 1, output: error.localizedDescription)
        }
    }

    private static func makeProcess(args: [String], config: AppConfig,
                                    override: GuestMachine?) -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: config.cliPath)
        process.arguments = ["--no-build"] + args
        process.environment = makeEnvironment(config: config, override: override)
        return process
    }

    // stdout/stderr 를 라인 단위로 받아 onLine 콜백 호출. install-apps 같이
    // 진행률 표시가 필요한 long-running 명령 전용. onLine 은 임의 thread 에서
    // 호출될 수 있으므로 호출 측이 @MainActor 로 넘겨받아야 한다.
    static func runStreaming(_ args: [String],
                             config: AppConfig,
                             override: GuestMachine?,
                             onLine: @escaping @Sendable (String) -> Void) async -> CommandResult {
        let process = makeProcess(args: args, config: config, override: override)
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        let buffer = VBoxRunnerLineBuffer()
        let handle = pipe.fileHandleForReading
        handle.readabilityHandler = { fh in
            let data = fh.availableData
            guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
            for line in buffer.append(text) {
                onLine(line)
            }
        }

        do {
            try process.run()
            process.waitUntilExit()
            handle.readabilityHandler = nil
            // 남은 부분 flush
            for line in buffer.flushRemaining() { onLine(line) }
            return CommandResult(status: process.terminationStatus, output: buffer.fullOutput())
        } catch {
            handle.readabilityHandler = nil
            return CommandResult(status: 1, output: error.localizedDescription)
        }
    }
}

// line-단위 buffer (Sendable, 임의 thread 접근 안전).
final class VBoxRunnerLineBuffer: @unchecked Sendable {
    private var partial = ""
    private var all = ""
    private let lock = NSLock()

    func append(_ chunk: String) -> [String] {
        lock.lock(); defer { lock.unlock() }
        all += chunk
        partial += chunk
        var lines: [String] = []
        while let r = partial.range(of: "\n") {
            lines.append(String(partial[..<r.lowerBound]))
            partial.removeSubrange(partial.startIndex..<r.upperBound)
        }
        return lines
    }

    func flushRemaining() -> [String] {
        lock.lock(); defer { lock.unlock() }
        guard !partial.isEmpty else { return [] }
        let last = partial
        partial = ""
        return [last]
    }

    func fullOutput() -> String {
        lock.lock(); defer { lock.unlock() }
        return all
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - GuestMachine 데이터 모델 (순수 값 타입)
// ───────────────────────────────────────────────────────────────────────────

enum MachineStatus: String {
    case running
    case stopped
    case suspended
    case paused
    case invalid
    case remote    // 사용자 추가 원격 SSH 호스트 — prlctl 외 머신.
    case unknown

    init(rawText: String) {
        self = MachineStatus(rawValue: rawText.lowercased()) ?? .unknown
    }

    // GUI 선택 가능성 판단용. remote 는 외부 머신 (ssh 됨) 이라 항상 활성 가능.
    var isRunning: Bool { self == .running || self == .remote }
    var isLaunchable: Bool {
        // vbox 가 prlctl 로 부팅 가능한 상태. remote 는 외부라 통제 불가 → 버튼 숨김.
        switch self {
        case .running, .stopped, .suspended, .paused: return true
        case .invalid, .remote, .unknown: return false
        }
    }

    var displayLabel: String {
        switch self {
        case .running:   return L("Status running")
        case .stopped:   return L("Status stopped")
        case .suspended: return L("Status suspended")
        case .paused:    return L("Status paused")
        case .invalid:   return L("Status invalid")
        case .remote:    return L("Status remote")
        case .unknown:   return L("Status unknown")
        }
    }
}

enum MachineOSKind: String {
    case linux, windows, macos, unknown

    init(rawText: String) {
        self = MachineOSKind(rawValue: rawText.lowercased()) ?? .unknown
    }

    var isSupportedByVBox: Bool { self == .linux }

    // SF Symbol 이름. Linux 는 PNG 캐시 우선이지만, 못 찾을 때 fallback 으로
    // "terminal" 사용 — 다른 호출자가 생겨도 안전하게 SF Symbol 이 나오도록.
    var iconSystemName: String {
        switch self {
        case .linux:   return "terminal"
        case .windows: return "macwindow"
        case .macos:   return "applelogo"
        case .unknown: return "questionmark.diamond"
        }
    }
}

// distro raw 문자열을 보고 캐시 디렉토리 안의 PNG path 반환.
// 우선순위: distro-specific PNG → tux.png. 둘 다 없으면 nil.
// PNG 파일은 `./vbox distro-icons fetch` 가 devicon 에서 받아와 캐시.
func linuxDistroIconPath(_ osRaw: String, name: String = "", iconDir: String) -> String? {
    guard !iconDir.isEmpty else { return nil }
    let s = (osRaw + " " + name).lowercased()
    var ids: [String] = []
    if s.contains("fedora") { ids.append("fedora") }
    if s.contains("ubuntu") { ids.append("ubuntu") }
    if s.contains("debian") { ids.append("debian") }
    if s.contains("arch")   { ids.append("arch") }
    if s.contains("centos") || s.contains("rhel") || s.contains("rocky") || s.contains("alma") {
        ids.append("centos")
    }
    if s.contains("mint") { ids.append("mint") }
    if s.contains("suse") || s.contains("opensuse") { ids.append("opensuse") }
    // 미캐시 distro (alpine/kali 등) 는 자동으로 Tux fallback.
    ids.append("tux")
    for id in ids {
        let path = "\(iconDir)/\(id).png"
        if FileManager.default.fileExists(atPath: path) {
            return path
        }
    }
    return nil
}

struct GuestMachine: Identifiable, Equatable, Hashable {
    let uuid: String
    let name: String
    let status: MachineStatus
    let ip: String
    let osRaw: String
    let osKind: MachineOSKind
    let sshUser: String
    let sshHost: String
    let guestDir: String

    var id: String { uuid }

    // "user@host" 형태. host 가 비면 빈 문자열 (UI 에서 disable 처리).
    var sshString: String {
        guard !sshHost.isEmpty else { return "" }
        return "\(sshUser)@\(sshHost)"
    }

    // GUI 에서 활성 guest 로 선택 가능한가? Linux + (running 또는 remote) + ip 있음.
    var isSelectable: Bool {
        osKind.isSupportedByVBox && status.isRunning && !sshHost.isEmpty
    }

    // 사용자가 추가한 원격 호스트 (Parallels 외) — 삭제 버튼 노출 등 분기에 사용.
    var isRemote: Bool { uuid.hasPrefix("remote:") }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachineListParser — vbox machines --tsv → [GuestMachine]
// 순수 함수. 입력만으로 출력이 정해지므로 테스트 쉬움.
// ───────────────────────────────────────────────────────────────────────────

enum MachineListParser {
    static func parse(_ tsv: String) -> [GuestMachine] {
        tsv.split(separator: "\n", omittingEmptySubsequences: true).compactMap { line in
            let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
            guard parts.count >= 9 else { return nil }
            return GuestMachine(
                uuid: parts[0],
                name: parts[1],
                status: MachineStatus(rawText: parts[2]),
                ip: parts[3],
                osRaw: parts[4],
                osKind: MachineOSKind(rawText: parts[5]),
                sshUser: parts[6],
                sshHost: parts[7],
                guestDir: parts[8]
            )
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachinesModel — 머신 목록 상태 + 액션
// LibraryModel 과 분리. 자체적으로 vbox machines 호출만 담당.
// 선택이 바뀌면 onSelect 콜백을 통해 외부 (LibraryModel) 로 알린다.
// ───────────────────────────────────────────────────────────────────────────

@MainActor
final class MachinesModel: ObservableObject {
    @Published var machines: [GuestMachine] = []
    @Published var selectedUUID: String? = nil
    @Published var isRefreshing = false
    @Published var status = ""

    let config: AppConfig
    var onSelect: ((GuestMachine?) -> Void)?

    init(config: AppConfig) {
        self.config = config
    }

    var selectedMachine: GuestMachine? {
        guard let uuid = selectedUUID else { return nil }
        return machines.first(where: { $0.uuid == uuid })
    }

    func refresh() async {
        isRefreshing = true
        status = L("Refresh")
        let result = await VBoxRunner.run(["machines", "list", "--tsv"], config: config, override: nil)
        if result.status == 0 {
            let parsed = MachineListParser.parse(result.output)
            machines = parsed
            if let selectedUUID, !parsed.contains(where: { $0.uuid == selectedUUID }) {
                self.selectedUUID = nil
                onSelect?(nil)
            }
            status = parsed.isEmpty ? L("No machines") : "\(parsed.count)대 발견"
        } else {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Refresh failed")
        }
        isRefreshing = false
    }

    func select(_ machine: GuestMachine?) {
        guard let machine else {
            selectedUUID = nil
            onSelect?(nil)
            status = ""
            return
        }
        // 선택 불가 머신은 silent return 대신 사유를 footer에 노출.
        guard machine.isSelectable else {
            status = unselectableReason(for: machine)
            return
        }
        selectedUUID = machine.uuid
        onSelect?(machine)
        status = L("Active machine no count {0}", arg: machine.name)
    }

    private func unselectableReason(for m: GuestMachine) -> String {
        m.name + ": " + L("Cannot select")
    }

    func start(_ machine: GuestMachine) async {
        await runControl(args: ["machines", "start", machine.uuid],
                        busyLabel: L("Power on VM") + " — " + machine.name)
    }

    func stop(_ machine: GuestMachine) async {
        await runControl(args: ["machines", "stop", machine.uuid],
                        busyLabel: L("Power off VM") + " — " + machine.name)
    }

    // 머신 별 config (overrides) 로드 — `./vbox machines config UUID --json`.
    func loadConfig(for machine: GuestMachine) async -> [String: String] {
        let result = await VBoxRunner.run(
            ["machines", "config", machine.uuid, "--json"],
            config: config, override: nil)
        guard result.status == 0,
              let data = result.output.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return [:] }
        var out: [String: String] = [:]
        for (k, v) in obj { out[k] = String(describing: v) }
        return out
    }

    // 변경된 키만 set/unset 호출. 빈 값은 unset (default 로 fallback).
    func saveConfig(for machine: GuestMachine, changes: [(String, String)]) async -> Bool {
        var ok = true
        for (key, value) in changes {
            let args: [String]
            if value.isEmpty {
                args = ["machines", "unset", machine.uuid, key]
            } else {
                args = ["machines", "set", machine.uuid, key, value]
            }
            let result = await VBoxRunner.run(args, config: config, override: nil)
            if result.status != 0 { ok = false }
        }
        status = ok ? L("Saved settings {0}", arg: machine.name) : L("Save failed")
        await refresh()
        return ok
    }

    // 원격 호스트 등록 — vbox remote add 호출 후 목록 refresh.
    func setPassword(for machine: GuestMachine, password: String) async -> Bool {
        let result = await VBoxRunner.run(
            ["machines", "set", machine.uuid, "password", password],
            config: config, override: nil)
        return result.status == 0
    }

    func clearPassword(for machine: GuestMachine) async -> Bool {
        let result = await VBoxRunner.run(
            ["machines", "unset", machine.uuid, "password"],
            config: config, override: nil)
        return result.status == 0
    }

    func addRemote(name: String, ssh: String, guestDir: String, osRaw: String,
                   identityFile: String = "", password: String = "") async -> Bool {
        var args = ["remote", "add", "--name", name, "--ssh", ssh]
        if !guestDir.isEmpty     { args += ["--dir", guestDir] }
        if !osRaw.isEmpty        { args += ["--os-raw", osRaw] }
        if !identityFile.isEmpty { args += ["--identity-file", identityFile] }
        let stdinText: String?
        if !password.isEmpty {
            args.append("--password-stdin")
            stdinText = password
        } else {
            stdinText = nil
        }
        let result = await VBoxRunner.run(args, config: config, override: nil, stdinText: stdinText)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Add remote failed")
            return false
        }
        status = L("Add remote ok {0}", arg: name)
        await refresh()
        return true
    }

    // 원격 호스트 삭제 — 선택된 머신이 그것이면 selection 해제 + override 초기화.
    func removeRemote(_ machine: GuestMachine) async {
        guard machine.isRemote else { return }
        let result = await VBoxRunner.run(["remote", "remove", machine.name],
                                          config: config, override: nil)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Remove remote failed")
            return
        }
        if selectedUUID == machine.uuid {
            selectedUUID = nil
            onSelect?(nil)
        }
        status = L("Remove remote ok {0}", arg: machine.name)
        await refresh()
    }

    private func runControl(args: [String], busyLabel: String) async {
        isRefreshing = true
        status = busyLabel
        let result = await VBoxRunner.run(args, config: config, override: nil)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Run failed")
            isRefreshing = false
            return
        }
        // start/stop 후 prlctl 가 상태를 갱신하기까지 약간의 지연.
        try? await Task.sleep(nanoseconds: 1_500_000_000)
        await refresh()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachineRow / MachinesSheet — 머신 리스트 UI
// 각 View 는 자기 책임만: row 는 한 줄 표시 + 액션 콜백, sheet 는 list + footer.
// ───────────────────────────────────────────────────────────────────────────

struct MachineStatusBadge: View {
    let status: MachineStatus

    var body: some View {
        Text(status.displayLabel)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch status {
        case .running:   return .green
        case .stopped:   return .secondary
        case .suspended, .paused: return .orange
        case .invalid:   return .red
        case .remote:    return .blue
        case .unknown:   return .secondary
        }
    }
}

struct MachineRow: View {
    let machine: GuestMachine
    let isActive: Bool
    let isBusy: Bool
    let distroIconDir: String
    let onSelect: () -> Void
    let onStart: () -> Void
    let onStop: () -> Void
    let onRemove: () -> Void  // remote 머신 전용
    let onConfig: () -> Void  // 머신 설정 sheet 열기
    let onInfo: () -> Void    // 머신 info sheet 열기

    var body: some View {
        HStack(spacing: 12) {
            // 좌측 정보만 tap target. controls 의 button 들은 자체 hit-test 유지.
            HStack(spacing: 12) {
                osIcon
                    .frame(width: 28, height: 28)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(machine.name)
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                            .foregroundStyle(machine.osKind.isSupportedByVBox ? .primary : .secondary)
                        MachineStatusBadge(status: machine.status)
                        if isActive {
                            Text(L("Active"))
                                .font(.caption2.weight(.bold))
                                .padding(.horizontal, 6).padding(.vertical, 1)
                                .background(Color.accentColor.opacity(0.2), in: Capsule())
                                .foregroundStyle(Color.accentColor)
                        }
                    }
                    Text(detailLine)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
            }
            .contentShape(Rectangle())
            .onTapGesture { onSelect() }
            .help(machine.isSelectable ? L("Tap to activate guest") : L("Cannot activate"))

            controls
        }
        .padding(.vertical, 4)
        .opacity(machine.isSelectable ? 1.0 : 0.55)
    }

    private var detailLine: String {
        let ip = machine.ip.isEmpty || machine.ip == "-" ? "ip ?" : machine.ip
        let os = machine.osRaw.isEmpty ? machine.osKind.rawValue : machine.osRaw
        return "\(os) · \(ip)"
    }

    @ViewBuilder
    private var osIcon: some View {
        if machine.osKind == .linux,
           let path = linuxDistroIconPath(machine.osRaw, name: machine.name, iconDir: distroIconDir),
           let img = NSImage(contentsOfFile: path) {
            Image(nsImage: img)
                .resizable()
                .aspectRatio(contentMode: .fit)
        } else {
            // distro PNG 캐시 미설치 또는 Linux 가 아닐 때 SF Symbol fallback.
            Image(systemName: machine.osKind.iconSystemName)
                .font(.title3)
                .foregroundStyle(machine.osKind.isSupportedByVBox ? Color.accentColor : .secondary)
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 6) {
            // Parallels VM: 상태별 start/stop. Remote 호스트는 vbox 가 부팅 통제 못 함 → 대신 삭제 버튼.
            if machine.isRemote {
                Button(role: .destructive) { onRemove() } label: { Image(systemName: "trash") }
                    .help(L("Remove remote host"))
                    .controlSize(.small)
                    .buttonStyle(.bordered)
                    .disabled(isBusy)
            } else if machine.status == .running {
                Button { onStop() } label: { Image(systemName: "stop.fill") }
                    .help(L("Power off VM"))
                    .controlSize(.small).buttonStyle(.bordered).disabled(isBusy)
            } else if machine.status.isLaunchable {
                Button { onStart() } label: { Image(systemName: "play.fill") }
                    .help(L("Power on VM"))
                    .controlSize(.small).buttonStyle(.bordered).disabled(isBusy)
            }
            Button { onInfo() } label: { Image(systemName: "info.circle") }
                .help(L("Machine info"))
                .controlSize(.small)
                .buttonStyle(.bordered)
                .disabled(isBusy)
            Button { onConfig() } label: { Image(systemName: "gearshape") }
                .help(L("Open settings"))
                .controlSize(.small)
                .buttonStyle(.bordered)
                .disabled(isBusy)
            Button { onSelect() } label: {
                Image(systemName: isActive ? "checkmark.circle.fill" : "arrow.right.circle")
            }
            .help(machine.isSelectable ? L("Activate guest") : L("Cannot select"))
            .controlSize(.small)
            .buttonStyle(.bordered)
            .disabled(!machine.isSelectable || isBusy)
        }
    }
}

struct MachinesSheet: View {
    @ObservedObject var model: MachinesModel
    let activeUUID: String?
    let onConfigRequest: (GuestMachine) -> Void   // nested-sheet 우회: 부모가 띄움
    let onAddRemoteRequest: () -> Void            // 동일 이유
    let onInfoRequest: (GuestMachine) -> Void     // 동일 이유 — read-only info
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .background(.regularMaterial)
        .frame(minWidth: 620, minHeight: 420)
        .task {
            if model.machines.isEmpty {
                await model.refresh()
            }
        }
        // nested sheet (machines sheet 안 또 sheet) 는 macOS SwiftUI 한계로 안 떠.
        // 두 후속 sheet 모두 LibraryWindow 가 띄우도록 onConfigRequest / onAddRemoteRequest 콜백 사용.
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "rectangle.3.group.fill")
                .font(.title2).foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(L("Guest Machines"))
                    .font(.headline)
                Text(L("Machines subtitle"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                onAddRemoteRequest()
            } label: { Image(systemName: "plus") }
                .help(L("Add remote host"))
                .disabled(model.isRefreshing)
            Button {
                Task { await model.refresh() }
            } label: { Image(systemName: "arrow.clockwise") }
                .disabled(model.isRefreshing)
                .help(L("Refresh"))
            Button(L("Close")) { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    @ViewBuilder
    private var content: some View {
        if model.machines.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "tray")
                    .font(.system(size: 32, weight: .semibold))
                    .foregroundStyle(.secondary)
                Text(L("No machines")).font(.title3.weight(.semibold))
                Text(L("No machines hint"))
                    .font(.caption).foregroundStyle(.secondary)
                Button { onAddRemoteRequest() } label: {
                    Label(L("Add remote host"), systemImage: "plus")
                }
                .buttonStyle(.borderedProminent)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(28)
        } else {
            List {
                ForEach(model.machines) { machine in
                    MachineRow(
                        machine: machine,
                        isActive: machine.uuid == activeUUID,
                        isBusy: model.isRefreshing,
                        distroIconDir: model.config.distroIconDir,
                        onSelect: { model.select(machine) },
                        onStart: { Task { await model.start(machine) } },
                        onStop:  { Task { await model.stop(machine) } },
                        onRemove: { Task { await model.removeRemote(machine) } },
                        onConfig: { onConfigRequest(machine) },
                        onInfo: { onInfoRequest(machine) }
                    )
                }
            }
            .listStyle(.inset)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if model.isRefreshing {
                ProgressView().controlSize(.small)
            }
            Text(model.status).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            Spacer()
        }
        .padding(.horizontal, 20).padding(.vertical, 10)
    }
}

// 새 원격 SSH 호스트 입력 폼. 검증 후 onSubmit 콜백.
// 원격 SSH 호스트 추가 폼. MachineConfigSheet 와 같은 시각 언어 (grouped Form +
// labeledField helper). 필수: 이름 + SSH 타겟. 나머지는 선택.
struct AddRemoteSheet: View {
    let onSubmit: (_ name: String, _ ssh: String, _ guestDir: String, _ osRaw: String,
                   _ identityFile: String, _ password: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var ssh = ""
    @State private var guestDir = ""
    @State private var osRaw = ""
    @State private var identityFile = ""
    @State private var password = ""

    private var canSubmit: Bool {
        !name.trimmed().isEmpty
            && ssh.contains("@")
            && !ssh.hasPrefix("@") && !ssh.hasSuffix("@")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "network").font(.title2).foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("Add remote host")).font(.headline)
                    Text(L("Add remote subtitle"))
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    configSectionBox(L("Required info")) {
                        labeledField(L("Display name"), placeholder: L("Display name placeholder"),
                                     text: $name,
                                     hint: L("Display name hint"))
                        labeledField(L("SSH target"), placeholder: L("SSH target placeholder"),
                                     text: $ssh,
                                     hint: L("SSH target hint"))
                    }

                    configSectionBox(L("Optional fields")) {
                        identityFileRow(text: $identityFile,
                                        hint: L("Field identity hint"))
                        passwordRow(text: $password,
                                    placeholder: L("Field password placeholder"),
                                    hint: L("Field password hint"))
                        labeledField(L("Workdir"), placeholder: L("Workdir placeholder"),
                                     text: $guestDir,
                                     hint: L("Workdir hint placeholder"))
                        labeledField(L("OS label"), placeholder: L("OS label placeholder"),
                                     text: $osRaw,
                                     hint: L("OS label hint"))
                    }
                }
                .padding(.vertical, 4)
            }

            HStack {
                Spacer()
                Button(L("Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(L("Add")) {
                    onSubmit(name.trimmed(), ssh.trimmed(),
                             guestDir.trimmed(), osRaw.trimmed(),
                             identityFile.trimmed(), password)
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(!canSubmit)
            }
        }
        .padding(20)
        .frame(minWidth: 560, idealWidth: 620, minHeight: 620, idealHeight: 660)
    }
}

// 머신 별 overrides 편집 폼. vbox machines config 로 현 값 로드 → 저장 시
// 변경된 키만 set/unset. 빈 값 = unset (default 로 fallback).
struct MachineConfigSheet: View {
    let machine: GuestMachine
    @ObservedObject var model: MachinesModel
    @Environment(\.dismiss) private var dismiss

    @State private var sshUser = ""
    @State private var sshHost = ""
    @State private var sshPort = ""
    @State private var identityFile = ""
    @State private var password = ""
    @State private var clearPasswordRequested = false
    @State private var originalHasPassword = false
    @State private var guestDir = ""
    @State private var port = ""
    @State private var socket = ""
    @State private var width = ""
    @State private var height = ""
    @State private var debug = false
    @State private var tls = false
    @State private var notes = ""
    @State private var original: [String: String] = [:]
    @State private var isLoading = false
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "gearshape.fill").font(.title2).foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("Open settings")).font(.headline)
                    Text(machine.name).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
            }

            if isLoading {
                ProgressView(L("Loading")).frame(maxWidth: .infinity)
            } else {
                ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        Color.clear.frame(height: 1).id("top")
                        configSectionBox(L("SSH section"),
                                         footer: L("SSH footer")) {
                            labeledField(L("Field user"), placeholder: L("Field user placeholder"),
                                         currentValue: machine.sshUser,
                                         text: $sshUser,
                                         hint: L("Field user hint"))
                            labeledField(L("Field host"), placeholder: L("Field host placeholder"),
                                         currentValue: machine.sshHost,
                                         text: $sshHost,
                                         hint: L("Field host hint"))
                            labeledField(L("Field ssh port"), placeholder: L("Field ssh port placeholder"),
                                         text: $sshPort,
                                         hint: L("Field ssh port hint"))
                            identityFileRow(text: $identityFile,
                                            hint: L("Field identity hint"))
                            passwordChangeRow(text: $password,
                                              hasExisting: originalHasPassword,
                                              clearRequested: $clearPasswordRequested,
                                              hint: L("Field password hint"))
                        }

                        configSectionBox(L("Guest path section")) {
                            labeledField(L("Field guestdir"), placeholder: L("Field guestdir placeholder"),
                                         currentValue: machine.guestDir,
                                         text: $guestDir,
                                         hint: L("Field guestdir hint"))
                        }

                        configSectionBox(L("Viewer section"),
                                         footer: L("Viewer footer")) {
                            labeledField(L("Field port"), placeholder: L("Field port placeholder"),
                                         text: $port,
                                         hint: L("Field port hint"))
                            labeledField(L("Field socket"), placeholder: L("Field socket placeholder"),
                                         text: $socket,
                                         hint: L("Field socket hint"))
                            labeledField(L("Field width"), placeholder: L("Field width placeholder"),
                                         text: $width,
                                         hint: L("Field width hint"))
                            labeledField(L("Field height"), placeholder: L("Field height placeholder"),
                                         text: $height,
                                         hint: L("Field height hint"))
                        }

                        configSectionBox(L("Flags section")) {
                            flagToggle(title: L("Flag debug"),
                                       hint: L("Flag debug hint"),
                                       isOn: $debug)
                            flagToggle(title: L("Flag tls"),
                                       hint: L("Flag tls hint"),
                                       isOn: $tls)
                        }

                        configSectionBox(L("Memo section"),
                                         footer: L("Memo footer")) {
                            TextField(L("Memo placeholder"),
                                      text: $notes, axis: .vertical)
                                .textFieldStyle(.roundedBorder)
                                .multilineTextAlignment(.leading)
                                .lineLimit(3...6)
                        }
                    }
                    .padding(.vertical, 4)
                }
                .onAppear { proxy.scrollTo("top", anchor: .top) }
                }
            }

            HStack {
                Spacer()
                Button(L("Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(isSaving ? L("Saving") : L("Save")) {
                    Task { await save() }
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(isLoading || isSaving)
            }
        }
        .padding(20)
        .frame(minWidth: 560, idealWidth: 600, minHeight: 720, idealHeight: 780)
        .task { await load() }
    }

    private func load() async {
        isLoading = true
        let map = await model.loadConfig(for: machine)
        original = map
        sshUser = map["ssh_user"] ?? ""
        sshHost = map["ssh_host"] ?? ""
        sshPort = map["ssh_port"] ?? ""
        identityFile = map["identity_file"] ?? ""
        password = ""
        clearPasswordRequested = false
        originalHasPassword = (map["has_password"] ?? "") == "true"
        guestDir = map["guest_dir"] ?? ""
        port = map["port"] ?? ""
        socket = map["socket"] ?? ""
        width = map["width"] ?? ""
        height = map["height"] ?? ""
        debug = (map["debug"] ?? "") == "true"
        tls = (map["tls"] ?? "") == "true"
        notes = map["notes"] ?? ""
        isLoading = false
    }

    private func save() async {
        isSaving = true
        let current: [(String, String)] = [
            ("ssh_user", sshUser.machineConfigTrim()),
            ("ssh_host", sshHost.machineConfigTrim()),
            ("ssh_port", sshPort.machineConfigTrim()),
            ("identity_file", identityFile.machineConfigTrim()),
            ("guest_dir", guestDir.machineConfigTrim()),
            ("port", port.machineConfigTrim()),
            ("socket", socket.machineConfigTrim()),
            ("width", width.machineConfigTrim()),
            ("height", height.machineConfigTrim()),
            ("debug", debug ? "true" : ""),
            ("tls", tls ? "true" : ""),
            ("notes", notes),
        ]
        let changes = current.filter { (k, v) in (original[k] ?? "") != v }
        if !changes.isEmpty {
            _ = await model.saveConfig(for: machine, changes: changes)
        }
        if !password.isEmpty {
            _ = await model.setPassword(for: machine, password: password)
        } else if clearPasswordRequested && originalHasPassword {
            _ = await model.clearPassword(for: machine)
        }
        isSaving = false
        dismiss()
    }
}

private extension String {
    func machineConfigTrim() -> String { trimmingCharacters(in: .whitespacesAndNewlines) }
    func trimmed() -> String { trimmingCharacters(in: .whitespacesAndNewlines) }
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

// 좌측 정렬된 설정 섹션 박스. macOS Form 의 자동 LabeledContent 변환을 피하기 위해
// ScrollView + VStack + 자체 GroupBox 스타일로 직접 작성.
@ViewBuilder
private func configSectionBox<Content: View>(_ title: String,
                                             footer: String = "",
                                             @ViewBuilder content: () -> Content) -> some View {
    VStack(alignment: .leading, spacing: 8) {
        Text(title)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
        VStack(alignment: .leading, spacing: 14) {
            content()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.background.opacity(0.6), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(.quaternary, lineWidth: 1)
        )
        if !footer.isEmpty {
            Text(footer)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// 플래그 toggle: 좌측에 라벨+힌트, 우측에 toggle. SwiftUI Toggle 의 standard label-trailing 패턴.
@ViewBuilder
private func flagToggle(title: String, hint: String, isOn: Binding<Bool>) -> some View {
    HStack(alignment: .top) {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.callout.weight(.medium))
            Text(hint).font(.caption2).foregroundStyle(.secondary)
        }
        Spacer()
        Toggle("", isOn: isOn).labelsHidden()
    }
}

// 한 줄짜리 설정 필드: 라벨 → 입력 → hint + "현재: <기본값>" 보조 정보.
// 입력 텍스트 / placeholder / 커서 모두 좌측 (leading) 정렬. currentValue 는
// hint 와 별 줄로 분리해서 시각적으로 명확.
@ViewBuilder
private func labeledField(_ label: String,
                          placeholder: String,
                          currentValue: String = "",
                          text: Binding<String>,
                          hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        Text(label)
            .font(.callout.weight(.medium))
            .frame(maxWidth: .infinity, alignment: .leading)
        TextField(placeholder, text: text)
            .textFieldStyle(.roundedBorder)
            .multilineTextAlignment(.leading)
        Text(hint)
            .font(.caption2)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
        if !currentValue.isEmpty {
            HStack(spacing: 4) {
                Text(L("Current label"))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                Text(currentValue)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
}

@main
struct VBoxLibraryApp: App {
    var body: some Scene {
        WindowGroup("vbox") {
            LibraryWindow()
        }
        .windowToolbarStyle(.unified)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
    }
}

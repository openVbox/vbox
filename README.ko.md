# vbox

[English](./README.md) · **한국어** · [日本語](./README.ja.md) · [简体中文](./README.zh.md) · [Español](./README.es.md)

[![codecov](https://codecov.io/gh/openVbox/vbox/branch/main/graph/badge.svg)](https://codecov.io/gh/openVbox/vbox)

macOS 호스트에서 Linux 게스트 GUI 앱을 rootless 멀티윈도우로 띄우기 위한
자체 프로토콜 + Wayland nested compositor 구현. XPRA 의존성 없이 어떤 언어의
키보드 입력이든 직접 처리하는 것이 목표.

![Linux GNOME 앱이 vbox로 macOS 네이티브 창에 rootless 표시](./images/hero.png)

## 왜 vbox?

- XPRA의 macOS 클라이언트는 호스트 IME 합성을 게스트로 전달하지 않아
  비라틴(한/일/중 등) 키보드 입력이 깨짐.
- XPRA macOS cask는 Gatekeeper 미통과로 deprecated — 향후 macOS에서 끊길
  예정.
- wire format과 클라이언트를 모두 직접 소유해야 장기 유지보수가 가능.
- Wayland를 기본 백엔드로 잡아 X11 캡처/주입 특수 경로를 처음부터 제외.

## 비슷한 도구와 비교

| 측면 | **vbox** | XPRA | VNC | `ssh -X` | Waypipe |
|---|---|---|---|---|---|
| 표시 단위 | rootless 멀티윈도우 | rootless 멀티윈도우 | 전체 데스크톱 | 단일 창 | rootless 멀티윈도우 |
| 게스트 디스플레이 | Wayland | X11 우선 | X11 / Wayland (전체) | X11 | Wayland |
| macOS 클라이언트 | 네이티브 (winit) | cask deprecated | 외부 (RealVNC 등) | XQuartz | 없음 |
| 한/일/중 IME | ✓ (text-input-v3) | ✗ (호스트 IME 미전달) | ✗ | ✗ | ✓ (Wayland 패스스루) |
| 전송 | QUIC + TCP / `ssh -L` fallback | TCP / SSH | RFB over TCP | SSH X11 | SSH 또는 Unix socket |
| 주 사용 경로 | macOS ↔ Linux | 임의 ↔ 임의 | 임의 ↔ 임의 | 임의 ↔ 임의 | Linux ↔ Linux |

## 아키텍처

```
   macOS host                            Linux guest
┌──────────────┐                ┌──────────────────────────────┐
│              │                │  ┌────────────────────────┐  │
│              │  data plane    │  │  vbox-server           │  │
│              │ ◀─QUIC(UDP)──▶ │  │  (data plane)          │  │
│              │   default      │  │  Wayland compositor    │  │
│              │  TCP+ssh -L    │  │  + frame stream        │  │
│  vbox-client │   fallback     │  │  (QUIC + TCP listen)   │  │
│   (viewer)   │                │  └───────────▲────────────┘  │
│              │                │              │ spawns        │
│              │  ctl : 5711    │  ┌───────────┴────────────┐  │
│              │ ◀─token/mTLS─▶ │  │  vbox-controld         │  │
│              │  (bootstrap)   │  │  (control plane)       │  │
│              │                │  │  instance / app RPC    │  │
└──────────────┘                │  └────────────────────────┘  │
                                └──────────────────────────────┘
```

- **vbox-server (게스트)** — *화면을 보내고 입력을 받는다.*
  Linux 게스트에서 Wayland 컴포지터로 동작. 앱 창의 픽셀을 호스트로
  스트리밍하고, 호스트의 마우스 / 키 / IME를 앱에 그대로 주입.
  통신: QUIC(UDP) 우선, 막히면 TCP / `ssh -L` fallback.
- **vbox-controld (게스트)** — *매니저.*
  vbox-server를 띄우고, 앱을 실행/종료하고, 뷰어에게 접속 정보(주소,
  토큰, QUIC 인증서 해시)를 발급. 인증은 token 또는 mTLS.
- **vbox-client (macOS)** — *뷰어.*
  게스트 앱 창 하나당 macOS 네이티브 창 하나를 띄우고, 마우스 / 스크롤 /
  키 / IME / 리사이즈 / 닫기를 그 창과 동기화. 먼저 controld에서
  부트스트랩 정보를 받고, 그 다음 vbox-server에 직접 연결.

### 크레이트 구조

```
crates/
├── proto/      # 와이어 포맷, 핸드셰이크, RPC 타입 (no I/O)
├── server/     # 게스트 데이터 plane: vbox-server
├── controld/   # 게스트 제어 plane: vbox-controld
├── client/     # macOS 클라이언트: vbox-client (ping/view/ctl)
└── vbox-cli/   # 사용자용 clap CLI: vbox
```

## macOS 설치 (Homebrew)

```bash
brew tap openVbox/vbox https://github.com/openVbox/vbox.git
brew install --cask vbox
```

업그레이드 / 제거:

```bash
brew upgrade --cask vbox
brew uninstall --cask vbox --zap   # ~/Applications/vbox + ~/.vbox 도 정리
```

## Linux 게스트 설치

```bash
curl -fsSL https://raw.githubusercontent.com/openVbox/vbox/main/scripts/install-linux.sh | sh
```

### 소스에서 빌드 (오래된 glibc, 다른 아키텍처, 또는 개발용)

```bash
# 게스트에서 — git + cargo + 빌드 의존성
sudo apt install -y git cargo build-essential pkg-config libxkbcommon-dev   # 또는 dnf 등가
git clone https://github.com/openVbox/vbox.git ~/vbox && cd ~/vbox
./install.sh   # cargo build → systemd user unit → linger (plain-token 전용)
```

## Changelog

[CHANGELOG.md](./CHANGELOG.md) 참고.

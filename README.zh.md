# vbox

[English](./README.md) · [한국어](./README.ko.md) · [日本語](./README.ja.md) · **简体中文** · [Español](./README.es.md)

在 macOS 主机上以 rootless 多窗口方式运行 Linux 客户机 GUI 应用的自定义协议
+ Wayland nested compositor 实现。目标是无需 XPRA 依赖，原生支持任何语言的
键盘输入。

![Linux GNOME 应用通过 vbox 以 rootless 多窗口在 macOS 上显示](./images/hero.png)

## 为什么是 vbox?

- XPRA 的 macOS 客户端不会把主机的 IME 合成转发给客户机，导致非拉丁
  （中/日/韩等）键盘输入失效。
- XPRA macOS cask 因无法通过 Gatekeeper 被标记为 deprecated — 在未来的
  macOS 上将无法继续使用。
- 必须同时掌控线协议格式与客户端，才能保障长期维护。
- 以 Wayland 作为默认后端，从一开始就避开 X11 抓取/注入的特殊路径。

## 与同类工具的对比

| 维度 | **vbox** | XPRA | VNC | `ssh -X` | Waypipe |
|---|---|---|---|---|---|
| 显示单位 | rootless 多窗口 | rootless 多窗口 | 整桌面 | 单窗口 | rootless 多窗口 |
| 客户机显示 | Wayland | 优先 X11 | X11 / Wayland (整体) | X11 | Wayland |
| macOS 客户端 | 原生 (winit) | cask 已 deprecated | 第三方 (RealVNC 等) | XQuartz | 无 |
| 中/日/韩 IME | ✓ (text-input-v3) | ✗ (主机 IME 不转发) | ✗ | ✗ | ✓ (Wayland 直通) |
| 通信 | QUIC + TCP / `ssh -L` 回退 | TCP / SSH | RFB over TCP | SSH X11 | SSH 或 Unix socket |
| 典型场景 | macOS ↔ Linux | 任意 ↔ 任意 | 任意 ↔ 任意 | 任意 ↔ 任意 | Linux ↔ Linux |

## 架构

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

- **vbox-server (客户机)** — *推画面、收输入。*
  在 Linux 客户机上作为 Wayland 合成器运行。把每个应用窗口的像素流到
  主机，并把主机的鼠标 / 按键 / IME 直接注入应用。
  通信：优先使用 QUIC (UDP)，被封堵时回退到 TCP / `ssh -L`。
- **vbox-controld (客户机)** — *管理者。*
  小型 RPC 守护进程：启动 vbox-server、启动 / 关闭应用、向查看器发放
  连接信息（地址、token、QUIC 证书哈希）。认证使用 token 或 mTLS。
- **vbox-client (macOS)** — *查看器。*
  为客户机的每个应用窗口打开一个 macOS 原生窗口，把鼠标 / 滚动 / 按键 /
  IME / 调整大小 / 关闭与它同步。先通过 controld 拿到 bootstrap 信息，
  之后直接连接到 vbox-server。

### crate 结构

```
crates/
├── proto/      # 线协议格式、握手、RPC 类型 (no I/O)
├── server/     # 客户机数据平面: vbox-server
├── controld/   # 客户机控制平面: vbox-controld
├── client/     # macOS 客户端: vbox-client (ping/view/ctl)
└── vbox-cli/   # 面向用户的 clap CLI: vbox
```

## 在 macOS 上安装 (Homebrew)

```bash
brew tap openVbox/vbox https://github.com/openVbox/vbox.git
brew install --cask vbox
```

升级 / 卸载:

```bash
brew upgrade --cask vbox
brew uninstall --cask vbox --zap   # 同时清理 ~/Applications/vbox + ~/.vbox
```

## 在 Linux 客户机上安装

```bash
curl -fsSL https://raw.githubusercontent.com/openVbox/vbox/main/scripts/install-linux.sh | sh
```

### 从源码构建 (旧 glibc、其他架构、或开发用)

```bash
# 客户机上 — git + cargo + 构建依赖
sudo apt install -y git cargo build-essential pkg-config libxkbcommon-dev   # 或 dnf 等价
git clone https://github.com/openVbox/vbox.git ~/vbox && cd ~/vbox
./install.sh   # cargo build → systemd user unit → linger (仅 plain-token)
```

## 运行

在 Launchpad 启动 `vbox.app` 并通过 GUI 选择，或直接通过 CLI 调用：

```bash
# 一次性完成: 客户机同步 + server + ssh tunnel + viewer + 启动应用
vbox run gnome-calculator

# 分开执行
vbox view                    # 只启动 server + tunnel + viewer
vbox app gnome-calculator    # 在已打开的 viewer 中启动应用

# 清理
vbox stop
```

指定客户机/端口:

```bash
vbox --guest USER@HOST --guest-dir /path/to/vbox --port 5710 run gnome-calculator
```

## Changelog

详见 [CHANGELOG.md](./CHANGELOG.md)。

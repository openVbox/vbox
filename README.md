# vbox

**English** · [한국어](./README.ko.md) · [日本語](./README.ja.md) · [简体中文](./README.zh.md) · [Español](./README.es.md)

A self-contained protocol + Wayland nested compositor for running Linux
guest GUI apps as rootless multi-window on a macOS host. The goal is
full keyboard input support for any language, without any XPRA dependency.

![GNOME apps running as rootless macOS windows via vbox](./images/hero.png)

## Why vbox?

- XPRA's macOS client never forwards host IME composition to the guest,
  so non-Latin keyboard input (Korean/Japanese/Chinese, etc.) is broken.
- The XPRA macOS cask is deprecated — it fails Gatekeeper and will not
  survive future macOS releases.
- Owning both the wire format and the client is the only way to keep
  long-term maintenance in our hands.
- Targeting Wayland as the canonical backend avoids the X11 capture/
  injection special-cases from day one.

## How vbox compares

| Aspect | **vbox** | XPRA | VNC | `ssh -X` | Waypipe |
|---|---|---|---|---|---|
| Display unit | rootless windows | rootless windows | whole desktop | single window | rootless windows |
| Guest display | Wayland | X11 first | X11 / Wayland (whole) | X11 | Wayland |
| macOS client | native (winit) | cask deprecated | external (RealVNC, etc.) | XQuartz | none |
| Asian IME (KR/JA/ZH) | yes (text-input-v3) | no (host IME not forwarded) | no | no | yes (Wayland passthrough) |
| Transport | QUIC + TCP / `ssh -L` fallback | TCP / SSH | RFB over TCP | SSH X11 | SSH or Unix socket |
| Main use case | macOS ↔ Linux | any ↔ any | any ↔ any | any ↔ any | Linux ↔ Linux |

## Architecture

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

- **vbox-server (guest)** — *streams the screen, receives the input.*
  Runs as a Wayland compositor on the Linux guest. Streams each app
  window's pixels to the host and injects the host's mouse / keys / IME
  back into the app.
  Transport: QUIC (UDP) preferred, TCP / `ssh -L` as fallback.
- **vbox-controld (guest)** — *the manager.*
  Spawns vbox-server, launches / stops apps, and hands the viewer the
  connection info (address, token, QUIC cert hash). Token or mTLS auth.
- **vbox-client (macOS)** — *the viewer.*
  Opens one native macOS window per guest app window and keeps mouse /
  scroll / keys / IME / resize / close in sync with it. Bootstraps via
  controld first, then talks to vbox-server directly.

### Crate layout

```
crates/
├── proto/      # wire format, handshake, RPC types (no I/O)
├── server/     # guest data plane: vbox-server
├── controld/   # guest control plane: vbox-controld
├── client/     # macOS client: vbox-client (ping/view/ctl)
└── vbox-cli/   # user-facing clap CLI: vbox
```

## Install on macOS (Homebrew)

```bash
brew tap openVbox/vbox https://github.com/openVbox/vbox.git
brew install --cask vbox
```

Upgrade or remove later:

```bash
brew upgrade --cask vbox
brew uninstall --cask vbox --zap   # also clears ~/Applications/vbox + ~/.vbox
```

## Install on the Linux guest

```bash
curl -fsSL https://raw.githubusercontent.com/openVbox/vbox/main/scripts/install-linux.sh | sh
```

### From source (older glibc, other arches, or development)

```bash
# on the guest — git + cargo + build deps
sudo apt install -y git cargo build-essential pkg-config libxkbcommon-dev   # or dnf equivalent
git clone https://github.com/openVbox/vbox.git ~/vbox && cd ~/vbox
./install.sh   # cargo build → systemd user unit → linger (plain-token only)
```

## Run

Launch `vbox.app` from Launchpad and pick from the GUI, or invoke the
CLI directly:

```bash
# guest sync + server + ssh tunnel + viewer + app launch in one shot
vbox run gnome-calculator

# step-by-step
vbox view                    # server + tunnel + viewer only
vbox app gnome-calculator    # spawn an app into the open viewer

# tear down
vbox stop
```

Specify guest/port:

```bash
vbox --guest USER@HOST --guest-dir /path/to/vbox --port 5710 run gnome-calculator
```

## Changelog

See [CHANGELOG.md](./CHANGELOG.md).

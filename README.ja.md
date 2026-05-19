# vbox

[English](./README.md) · [한국어](./README.ko.md) · **日本語** · [简体中文](./README.zh.md) · [Español](./README.es.md)

macOS ホストから Linux ゲストの GUI アプリを rootless マルチウィンドウで表示する
ための独自プロトコル + Wayland nested compositor 実装。XPRA 依存なしで、あらゆる
言語のキーボード入力に対応することが目標。

## なぜ vbox?

- XPRA の macOS クライアントはホストの IME 合成をゲストへ転送しないため、
  非ラテン文字（日本語/韓国語/中国語など）の入力が壊れる。
- XPRA macOS cask は Gatekeeper を通らず deprecated 扱い — 将来の macOS では
  動かなくなる。
- ワイヤフォーマットとクライアントを自分で所有しないと長期的な保守ができない。
- Wayland をデフォルトバックエンドに据えて、X11 のキャプチャ/注入の特殊経路を
  最初から排除。

## 類似ツールとの比較

| 観点 | **vbox** | XPRA | VNC | `ssh -X` | Waypipe |
|---|---|---|---|---|---|
| 表示単位 | rootless ウィンドウ | rootless ウィンドウ | デスクトップ全体 | 単一ウィンドウ | rootless ウィンドウ |
| ゲストディスプレイ | Wayland | X11 中心 | X11 / Wayland (全体) | X11 | Wayland |
| macOS クライアント | ネイティブ (winit) | cask deprecated | 外部 (RealVNC など) | XQuartz | なし |
| IME (日/韓/中) | ✓ (text-input-v3) | ✗ (ホスト IME を転送せず) | ✗ | ✗ | ✓ (Wayland パススルー) |
| 通信 | QUIC + TCP / `ssh -L` フォールバック | TCP / SSH | RFB over TCP | SSH X11 | SSH または Unix socket |
| 主な用途 | macOS ↔ Linux | 任意 ↔ 任意 | 任意 ↔ 任意 | 任意 ↔ 任意 | Linux ↔ Linux |

## アーキテクチャ

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

- **vbox-server (ゲスト)** — *画面を送り、入力を受け取る。*
  Linux ゲスト上で Wayland コンポジタとして動作。アプリウィンドウの
  ピクセルをホストへストリーミングし、ホストのマウス / キー / IME を
  アプリにそのまま注入する。
  通信: QUIC (UDP) を優先、塞がれていれば TCP / `ssh -L` にフォール
  バック。
- **vbox-controld (ゲスト)** — *マネージャ。*
  vbox-server を起動し、アプリの起動 / 停止を行い、ビューアへ接続情報
  (アドレス、トークン、QUIC 証明書ハッシュ) を発行する小さな RPC
  デーモン。認証は token または mTLS。
- **vbox-client (macOS)** — *ビューア。*
  ゲストのアプリウィンドウごとに macOS ネイティブウィンドウを 1 つ
  開き、マウス / スクロール / キー / IME / リサイズ / クローズをそれに
  同期する。最初に controld から bootstrap 情報を受け取り、そのあとは
  vbox-server へ直接接続。

### クレート構成

```
crates/
├── proto/      # ワイヤフォーマット、ハンドシェイク、RPC 型 (no I/O)
├── server/     # ゲストデータプレーン: vbox-server
├── controld/   # ゲストコントロールプレーン: vbox-controld
├── client/     # macOS クライアント: vbox-client (ping/view/ctl)
└── vbox-cli/   # ユーザー向け clap CLI: vbox
```

## macOS にインストール (Homebrew)

```bash
brew tap openVbox/vbox https://github.com/openVbox/vbox.git
brew install --cask vbox
```

アップグレード / 削除:

```bash
brew upgrade --cask vbox
brew uninstall --cask vbox --zap   # ~/Applications/vbox + ~/.vbox も削除
```

## Linux ゲストへのインストール

```bash
curl -fsSL https://raw.githubusercontent.com/openVbox/vbox/main/scripts/install-linux.sh | sh
```

### ソースからビルド (古い glibc、別アーキ、または開発用)

```bash
# ゲストで — git + cargo + ビルド依存
sudo apt install -y git cargo build-essential pkg-config libxkbcommon-dev   # または dnf 等価
git clone https://github.com/openVbox/vbox.git ~/vbox && cd ~/vbox
./install.sh   # cargo build → systemd user unit → linger (plain-token 専用)
```

## 実行

Launchpad から `vbox.app` を起動して GUI で選ぶか、CLI から直接呼び出します。

```bash
# ゲスト sync + server + ssh tunnel + viewer + アプリ起動を一度に
vbox run gnome-calculator

# 分離して実行
vbox view                    # server + tunnel + viewer のみ
vbox app gnome-calculator    # 起動済み viewer にアプリを spawn

# 後片付け
vbox stop
```

ゲスト/ポートの指定:

```bash
vbox --guest USER@HOST --guest-dir /path/to/vbox --port 5710 run gnome-calculator
```

## Changelog

[CHANGELOG.md](./CHANGELOG.md) を参照。

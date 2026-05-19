cask "vbox" do
  version "0.1.1"
  sha256 "ea1d5e276a6120516856d087d9c1ba231d62420c07d93ad853d72edaf9ff280a"

  url "https://github.com/openVbox/vbox/releases/download/v#{version}/vbox-universal.tar.gz"
  name "vbox"
  desc "Wayland nested compositor client for running Linux GUI apps as rootless windows"
  homepage "https://github.com/openVbox/vbox"

  depends_on macos: :ventura

  binary "bin/vbox"

  zap trash: [
    "~/.vbox",
    "~/Applications/vbox",
  ]

  caveats <<~EOS
    vbox builds your macOS .app launchers from your guest config at runtime,
    because each .app embeds your guest host, port and CLI path. After install:

      # (one-time) make sure Xcode command line tools are installed:
      xcode-select --install

      # create ~/Applications/vbox/vbox.app + per-guest-app launchers:
      vbox install-apps                   # all guest GUI apps
      vbox install-apps calculator text   # just specific ones

    Run:
      vbox run gnome-calculator           # one-shot start
      vbox view                           # server + tunnel + viewer
      vbox stop                           # tear down
  EOS
end

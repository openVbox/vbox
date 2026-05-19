cask "vbox" do
  version "0.1.0"
  sha256 "REPLACE_WITH_dist/vbox-0.1.0-universal.tar.gz.sha256"

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

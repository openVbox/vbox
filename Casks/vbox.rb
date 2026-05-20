cask "vbox" do
  version "0.1.1"
  sha256 "17a4a9be0bdd0e4683df81bcff456d823971a9693227fd16986ab256ea4c9147"

  url "https://github.com/openVbox/vbox/releases/download/v#{version}/vbox-universal.tar.gz"
  name "vbox"
  desc "Wayland nested compositor client for running Linux GUI apps as rootless windows"
  homepage "https://github.com/openVbox/vbox"

  depends_on macos: :ventura

  binary "bin/vbox"
  app "vbox.app"

  zap trash: [
    "~/.vbox",
    "~/Applications/vbox",
  ]

  caveats <<~EOS
    vbox is now in Launchpad. Per-Linux-app launchers are still generated on
    your machine because each .app embeds your guest host, port and CLI path:

      # (one-time) make sure Xcode command line tools are installed:
      xcode-select --install

      # extra "(Linux)" launchers in ~/Applications/vbox/
      vbox install-apps                   # all guest GUI apps
      vbox install-apps calculator text   # just specific ones

    Run:
      vbox run gnome-calculator           # one-shot start
      vbox view                           # server + tunnel + viewer
      vbox stop                           # tear down
  EOS
end

cask "trident" do
  version "1.0.0"
  sha256 "520f365f092fc4a5ccb300c36b73b24bc96c7259c377005d12ae06f266340255"

  url "https://github.com/amagomori/trident/releases/download/v#{version}/Trident-#{version}-macos-universal.dmg"
  name "Trident"
  desc "Fast, GPU-rendered terminal emulator"
  homepage "https://github.com/amagomori/trident"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on :macos

  app "Trident.app"
  binary "#{appdir}/Trident.app/Contents/MacOS/trident"

  # Only ad-hoc signed, so Gatekeeper refuses to open a quarantined copy.
  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Trident.app"]
  end

  zap trash: [
    "~/.config/trident",
    "~/Library/Saved Application State/com.amagomori.trident.savedState",
  ]
end

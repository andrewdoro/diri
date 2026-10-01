cask "diri" do
  version "0.8.11"
  sha256 "53103e9a7f18846fbf9885845826eeac0ce7f0eaaa0acfb8fdbe18648bd8577f"

  url "https://github.com/cristicretu/diri/releases/download/v#{version}/diri-#{version}-universal.dmg"
  name "diri"
  desc "Terminal workspace for running coding agents in parallel"
  homepage "https://diri.sh/"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates true
  depends_on macos: :sequoia

  app "diri.app"

  # ~/Library/Application Support/Dirijor is intentionally not zapped: it holds
  # user-written notes, saved agent account logins and the state of sessions
  # whose agent processes keep running after the app quits.
  zap trash: [
    "~/Library/Application Support/diri",
    "~/Library/Caches/com.dirijor.diri",
    "~/Library/Caches/diri",
    "~/Library/HTTPStorages/com.dirijor.diri",
    "~/Library/HTTPStorages/com.dirijor.diri.binarycookies",
    "~/Library/Preferences/com.dirijor.diri.plist",
    "~/Library/Saved Application State/com.dirijor.diri.savedState",
    "~/Library/WebKit/com.dirijor.diri",
  ]
end

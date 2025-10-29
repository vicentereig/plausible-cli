class Plausible < Formula
  desc "Automate Plausible Analytics from the CLI"
  homepage "https://github.com/vicentereig/plausible-cli"
  version "__VERSION__"
  license "MIT"
  url "https://github.com/vicentereig/plausible-cli/archive/refs/tags/__VERSION__.tar.gz"
  sha256 "__SHA256__"
  head "https://github.com/vicentereig/plausible-cli.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match "plausible", shell_output("#{bin}/plausible --help")
  end
end

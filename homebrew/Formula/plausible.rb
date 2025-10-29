class Plausible < Formula
  desc "Automate Plausible Analytics from the CLI"
  homepage "https://github.com/vicentereig/plausible-cli"
  version "1.0.0"
  license "MIT"
  url "https://github.com/vicentereig/plausible-cli/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "6583138f83143119725d124a0fb279f6e9380c47e4cec81b380d68c5c5525d55"
  head "https://github.com/vicentereig/plausible-cli.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match "plausible", shell_output("#{bin}/plausible --help")
  end
end

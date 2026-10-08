#!/bin/bash
# Exercise the real installers against built artifacts in an isolated user directory.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
profile=$(cd "$1" && pwd)
shift
artifact_dir=
architecture=
target=
cargo_args=(test --locked)
if (($#)) && [[ "$1" != --* ]]; then artifact_dir=$1; shift; fi
while (($#)); do
  case "$1" in
    --release) cargo_args+=(--release); shift ;;
    --arch|--target)
      (($# >= 2)) || { printf 'Missing value for %s\n' "$1" >&2; exit 1; }
      if [[ "$1" == --arch ]]; then architecture=$2; else target=$2; fi
      shift 2 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 1 ;;
  esac
done
if [[ -z "$architecture" ]]; then
  architecture=$(uname -m)
  if [[ $(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then architecture=arm64; fi
fi
[[ "$architecture" == x86_64 || "$architecture" == arm64 ]]
platform="darwin-$architecture"
if [[ -n "$target" ]]; then
  expected_target=x86_64-apple-darwin
  if [[ "$architecture" == arm64 ]]; then expected_target=aarch64-apple-darwin; fi
  [[ "$target" == "$expected_target" ]]
  cargo_args+=(--target "$target")
fi
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/xrun-installer-test.XXXXXX")
test_dir=$(cd "$test_dir" && pwd -P)
cleanup() {
  local status=$?
  trap - EXIT
  rm -rf -- "$test_dir"
  exit "$status"
}
trap cleanup EXIT
artifacts=${artifact_dir:-"$test_dir/artifacts"}
if [[ -z "$artifact_dir" ]]; then
  mkdir -p "$artifacts"
  cp "$profile/bundle/macos/xrun.app.zip" "$artifacts/xrun-app-$platform.zip"
  bun "$root/desktop/scripts/package-manifest.ts" --platform "$platform" --directory "$artifacts"
fi
version=$(/usr/bin/plutil -extract version raw -o - "$artifacts/xrun-$platform.json")
app_dir="$test_dir/安装 App with spaces"
mkdir -p "$test_dir/home"
result="$test_dir/result.json"
run_install() {
  env HOME="$test_dir/home" ZDOTDIR="$test_dir/home" bash "$root/scripts/install.sh" --version "$version" --arch "$architecture" "$@" > "$result"
}
field() { /usr/bin/plutil -extract "$1" raw -o - "$result"; }
expect_failure() {
  local code=$1
  shift
  if run_install "$@"; then printf 'Expected %s failure\n' "$code" >&2; exit 1; fi
  [[ $(field ok) == false && $(field error.code) == "$code" ]]
}
run_install --source-dir "$artifacts" --install-dir "$app_dir"
[[ $(field ok) == true && $(field changed) == true && $(field self_check.version) == "$version" ]]
[[ $(field platform) == "$platform" ]]
[[ $(field path) == "$app_dir/xrun.app" ]]
app_binary=$(field executable)
/usr/bin/lipo "$app_binary" -verify_arch "$architecture"
/usr/bin/lipo "$(field desktop_executable)" -verify_arch "$architecture"
codesign --verify --deep --strict "$app_dir/xrun.app"
[[ -L "$test_dir/home/.local/bin/xrun" ]]
[[ $(HOME="$test_dir/home" ZDOTDIR="$test_dir/home" /bin/zsh -lc 'xrun --version') == "xrun $version" ]]
run_install --source-dir "$artifacts" --install-dir "$app_dir"
[[ $(field changed) == false ]]

# A corrupt download must leave an existing App intact.
bad="$test_dir/corrupt"
mkdir -p "$bad"
cp "$artifacts/xrun-$platform.json" "$artifacts/xrun-app-$platform.zip" "$bad/"
before=$(shasum -a 256 "$app_binary")
printf corrupt >> "$bad/xrun-app-$platform.zip"
expect_failure CHECKSUM_MISMATCH --source-dir "$bad" --install-dir "$app_dir"
[[ $(shasum -a 256 "$app_binary") == "$before" ]]
/usr/bin/plutil -replace version -string 9.9.9 "$bad/xrun-$platform.json"
expect_failure INVALID_MANIFEST --source-dir "$bad" --install-dir "$app_dir"
cp "$artifacts/xrun-$platform.json" "$bad/"
/usr/bin/plutil -replace platform -string wrong-platform "$bad/xrun-$platform.json"
expect_failure INVALID_MANIFEST --source-dir "$bad" --install-dir "$app_dir"
cp "$artifacts/xrun-$platform.json" "$bad/"
/usr/bin/plutil -replace artifacts.app.file -string other-architecture.zip "$bad/xrun-$platform.json"
expect_failure INVALID_MANIFEST --source-dir "$bad" --install-dir "$app_dir"
[[ $(shasum -a 256 "$app_binary") == "$before" ]]

cd "$root"
XRUN_TEST_BINARY="$app_binary" cargo "${cargo_args[@]}" --test smoke -- --nocapture
printf 'Automatic macOS App installation and terminal CLI passed.\n'

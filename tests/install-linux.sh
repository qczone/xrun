#!/bin/bash
# Exercise the published Linux CLI installer without changing the runner's home.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
profile=$(cd "$1" && pwd)
shift
artifact_dir=
target=
cargo_args=(test --locked)
if (($#)) && [[ "$1" != --* ]]; then artifact_dir=$1; shift; fi
while (($#)); do
  case "$1" in
    --release) cargo_args+=(--release); shift ;;
    --target) target=$2; cargo_args+=(--target "$target"); shift 2 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 1 ;;
  esac
done
architecture=$(uname -m)
if [[ "$architecture" == aarch64 ]]; then architecture=arm64; fi
[[ "$architecture" == x86_64 || "$architecture" == arm64 ]]
platform="linux-$architecture"
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/xrun-installer-test.XXXXXX")
trap 'rm -rf -- "$test_dir"' EXIT
artifacts=${artifact_dir:-"$test_dir/artifacts"}
if [[ -z "$artifact_dir" ]]; then
  mkdir -p "$artifacts"
  tar -C "$profile" -czf "$artifacts/xrun-$platform.tar.gz" xrun
  bun "$root/desktop/scripts/package-manifest.ts" --platform "$platform" --directory "$artifacts"
fi
[[ $(tar -tzf "$artifacts/xrun-$platform.tar.gz") == xrun ]]
version=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$artifacts/xrun-$platform.json")
install_dir="$test_dir/安装 CLI with spaces"
mkdir -p "$test_dir/home"
result="$test_dir/result.json"
run_install() {
  env HOME="$test_dir/home" ZDOTDIR="$test_dir/home" bash "$root/scripts/install.sh" \
    --version "$version" --install-dir "$install_dir" "$@" > "$result"
}
field() {
  python3 - "$result" "$1" <<'PYTHON'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    value = json.load(source)
for key in sys.argv[2].split("."):
    value = value[key]
print(str(value).lower() if isinstance(value, bool) else value)
PYTHON
}
expect_failure() {
  local code=$1
  shift
  if run_install "$@"; then printf 'Expected %s failure\n' "$code" >&2; exit 1; fi
  [[ $(field ok) == false && $(field error.code) == "$code" ]]
}
run_install --source-dir "$artifacts"
[[ $(field ok) == true && $(field changed) == true && $(field platform) == "$platform" ]]
binary=$(field executable)
[[ "$binary" == "$install_dir/xrun" && $("$binary" --version) == "xrun $version" ]]
[[ $(env HOME="$test_dir/home" bash --noprofile --norc -c '. "$HOME/.profile"; xrun --version') == "xrun $version" ]]
run_install --source-dir "$artifacts"
[[ $(field changed) == false ]]
[[ $(awk '/^# >>> xrun CLI >>>$/ { n++ } END { print n }' "$test_dir/home/.bashrc") == 1 ]]

bad="$test_dir/corrupt"
mkdir -p "$bad"
cp "$artifacts/xrun-$platform.json" "$artifacts/xrun-$platform.tar.gz" "$bad/"
before=$(sha256sum "$binary")
printf corrupt >> "$bad/xrun-$platform.tar.gz"
expect_failure CHECKSUM_MISMATCH --source-dir "$bad"
[[ $(sha256sum "$binary") == "$before" ]]

# A candidate that fails after placement must restore the installed executable.
rollback="$test_dir/rollback"
mkdir -p "$rollback" "$test_dir/fixture"
cat > "$test_dir/fixture/xrun" <<'FIXTURE'
#!/bin/bash
if [[ "$0" == "$XRUN_INSTALL_TEST_BAD_PATH" ]]; then
  printf 'xrun wrong-version\n'
else
  printf 'xrun %s\n' "$XRUN_INSTALL_TEST_VERSION"
fi
FIXTURE
chmod 755 "$test_dir/fixture/xrun"
tar -C "$test_dir/fixture" -czf "$rollback/xrun-$platform.tar.gz" xrun
bun "$root/desktop/scripts/package-manifest.ts" --platform "$platform" --directory "$rollback"
export XRUN_INSTALL_TEST_BAD_PATH="$binary" XRUN_INSTALL_TEST_VERSION="$version"
expect_failure SELF_CHECK_FAILED --source-dir "$rollback"
[[ $(sha256sum "$binary") == "$before" ]]

cd "$root"
XRUN_TEST_BINARY="$binary" cargo "${cargo_args[@]}" --test smoke -- --nocapture
printf 'Automatic Linux CLI installation, terminal PATH and rollback passed.\n'

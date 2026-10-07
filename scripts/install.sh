#!/bin/bash
# Install a published xrun App or CLI on macOS without opening the desktop UI.
set -Eeuo pipefail

component=app
architecture=
version=
install_dir=
source_dir=
base_url=
work_dir=
stage_dir=
destination=
backup=
replaced=0
changed=false

json_string() {
  local value=$1 char number i
  printf '"'
  for ((i = 0; i < ${#value}; i++)); do
    char=${value:i:1}
    case "$char" in
      '"') printf '\\"' ;;
      '\') printf '\\\\' ;;
      *)
        printf -v number '%d' "'$char"
        if ((number >= 0 && number < 32)); then printf '\\u%04x' "$number"; else printf '%s' "$char"; fi
        ;;
    esac
  done
  printf '"'
}
fail() {
  printf '{"ok":false,"error":{"code":'
  json_string "$1"
  printf ',"message":'
  json_string "$2"
  printf '}}\n'
  exit 1
}
cleanup() {
  local status=$?
  trap - EXIT
  if ((status != 0)); then
    if [[ -n "$backup" && -e "$backup" ]]; then
      rm -rf -- "$destination"
      mv -- "$backup" "$destination"
    elif ((replaced)); then
      rm -rf -- "$destination"
    fi
  fi
  if [[ -n "$stage_dir" ]]; then rm -rf -- "$stage_dir"; fi
  if [[ -n "$work_dir" ]]; then rm -rf -- "$work_dir"; fi
  exit "$status"
}
trap cleanup EXIT
trap 'fail INSTALL_FAILED "Installation failed; see stderr for details."' ERR
trap 'fail INTERRUPTED "Installation interrupted."' INT TERM

while (($#)); do
  case "$1" in
    --help|-h)
      printf '%s\n' 'Usage: bash install.sh --version VERSION [--component app|cli] [--arch x86_64|arm64] [--install-dir DIR] [--source-dir DIR | --base-url HTTPS_URL]'
      exit 0 ;;
    --version|--component|--arch|--install-dir|--source-dir|--base-url)
      (($# >= 2)) || fail INVALID_ARGUMENT "Missing value for $1."
      case "$1" in
        --version) version=${2#v} ;;
        --component) component=$2 ;;
        --arch) architecture=$2 ;;
        --install-dir) install_dir=$2 ;;
        --source-dir) source_dir=$2 ;;
        --base-url) base_url=$2 ;;
      esac
      shift 2 ;;
    *) fail INVALID_ARGUMENT "Unknown argument: $1." ;;
  esac
done
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || fail INVALID_ARGUMENT 'Specify the full release version with --version.'
[[ "$component" == app || "$component" == cli ]] || fail INVALID_ARGUMENT 'Component must be app or cli.'
[[ -z "$architecture" || "$architecture" == x86_64 || "$architecture" == arm64 ]] || fail INVALID_ARGUMENT 'Architecture must be x86_64 or arm64.'
[[ -z "$source_dir" || -z "$base_url" ]] || fail INVALID_ARGUMENT 'Choose source-dir or base-url.'
[[ $(uname -s) == Darwin ]] || fail UNSUPPORTED_PLATFORM 'install.sh supports macOS; use the platform CLI archive on Linux.'
host_architecture=$(uname -m)
# A shell running under Rosetta still selects the native Apple Silicon release.
if [[ $(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then host_architecture=arm64; fi
[[ "$host_architecture" == x86_64 || "$host_architecture" == arm64 ]] || fail UNSUPPORTED_PLATFORM 'This release supports macOS x86_64 and arm64.'
if [[ -z "$architecture" ]]; then architecture=$host_architecture; fi
[[ "$host_architecture" == arm64 || "$architecture" == x86_64 ]] || fail UNSUPPORTED_PLATFORM 'An arm64 release cannot run on an Intel Mac.'
platform="darwin-$architecture"
if [[ -z "$install_dir" ]]; then
  if [[ "$component" == app ]]; then install_dir="$HOME/Applications"; else install_dir="$HOME/.local/bin"; fi
fi
if [[ -z "$base_url" ]]; then base_url="https://github.com/qczone/xrun/releases/download/v$version"; fi
[[ "$base_url" == https://* ]] || fail INVALID_ARGUMENT 'base-url must use HTTPS.'
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/xrun-download.XXXXXX")

fetch() {
  if [[ -n "$source_dir" ]]; then
    [[ -f "$source_dir/$1" ]] || fail ARTIFACT_NOT_FOUND "Missing artifact: $1."
    cp -- "$source_dir/$1" "$work_dir/$1"
  else
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --retry 2 --connect-timeout 20 --max-time 600 "${base_url%/}/$1" -o "$work_dir/$1" || fail DOWNLOAD_FAILED "Could not download $1."
  fi
}
field() { /usr/bin/plutil -extract "$1" raw -o - "$work_dir/xrun-$platform.json"; }
fetch "xrun-$platform.json"
[[ $(field schema) == 1 && $(field version) == "$version" && $(field platform) == "$platform" ]] || fail INVALID_MANIFEST 'Manifest version or platform does not match the requested release.'
artifact=$(field "artifacts.$component.file")
digest=$(field "artifacts.$component.sha256")
if [[ "$component" == app ]]; then expected="xrun-app-$platform.zip"; else expected="xrun-$platform.tar.gz"; fi
[[ "$artifact" == "$expected" && "$digest" =~ ^[0-9a-f]{64}$ ]] || fail INVALID_MANIFEST 'Manifest artifact name or SHA-256 is invalid.'
fetch "$artifact"
actual=$(shasum -a 256 "$work_dir/$artifact")
[[ ${actual%% *} == "$digest" ]] || fail CHECKSUM_MISMATCH 'Artifact SHA-256 does not match the manifest.'

mkdir -p -- "$install_dir"
install_dir=$(cd -- "$install_dir" && pwd -P)
stage_dir=$(mktemp -d "$install_dir/.xrun-install.XXXXXX")
if [[ "$component" == app ]]; then
  ditto -x -k "$work_dir/$artifact" "$stage_dir"
  staged="$stage_dir/xrun.app"
  destination="$install_dir/xrun.app"
  executable="$destination/Contents/MacOS/xrun"
  desktop_executable="$destination/Contents/MacOS/xrun-desktop"
  [[ -d "$staged" && ! -L "$staged" ]] || fail INVALID_ARTIFACT 'Archive does not contain xrun.app.'
  codesign --verify --deep --strict "$staged" >&2 || fail INVALID_SIGNATURE 'App signature verification failed.'
  staged_binary="$staged/Contents/MacOS/xrun"
else
  tar -xzf "$work_dir/$artifact" -C "$stage_dir"
  staged="$stage_dir/xrun"
  destination="$install_dir/xrun"
  executable="$destination"
  desktop_executable=
  staged_binary="$staged"
fi
[[ -f "$staged_binary" && ! -L "$staged_binary" && -x "$staged_binary" ]] || fail INVALID_ARTIFACT 'Archive does not contain an executable xrun helper.'
[[ $("$staged_binary" --version) == "xrun $version" ]] || fail VERSION_MISMATCH 'Artifact executable version does not match the manifest.'
if [[ "$component" == app ]]; then
  "$staged/Contents/MacOS/xrun-desktop" --self-check > "$work_dir/check.json" || fail SELF_CHECK_FAILED 'Staged App self-check failed.'
  [[ $(/usr/bin/plutil -extract version raw -o - "$work_dir/check.json") == "$version" ]] || fail VERSION_MISMATCH 'App version does not match the manifest.'
fi
[[ ! -L "$destination" ]] || fail INVALID_DESTINATION 'Installation destination must not be a symbolic link.'
if [[ -e "$destination" ]] && diff -qr "$staged" "$destination" > /dev/null 2>&1; then
  changed=false
else
  if [[ -e "$destination" ]]; then
    if [[ "$component" == app ]]; then
      if ps -axo comm= | grep -Fx -- "$desktop_executable" > /dev/null; then
        fail APP_RUNNING 'Close the installed xrun App before updating.'
      fi
      old_binary="$executable"
    else
      old_binary="$destination"
    fi
    [[ -f "$old_binary" && -x "$old_binary" ]] || fail INVALID_DESTINATION 'Existing destination is not an xrun installation.'
    [[ $("$old_binary" --version) == xrun\ * ]] || fail INVALID_DESTINATION 'Existing executable is not xrun.'
    "$old_binary" daemon stop >&2 || fail UPDATE_PREPARE_FAILED 'Could not stop the daemon before updating.'
    backup="$stage_dir/previous"
    mv -- "$destination" "$backup"
  fi
  replaced=1
  mv -- "$staged" "$destination"
  changed=true
fi
if [[ "$component" == app ]]; then
  "$desktop_executable" --self-check > "$work_dir/check.json" || fail SELF_CHECK_FAILED 'Installed App self-check failed.'
  [[ $(/usr/bin/plutil -extract version raw -o - "$work_dir/check.json") == "$version" ]] || fail VERSION_MISMATCH 'Installed App version does not match the manifest.'
  self_check=$(cat "$work_dir/check.json")
else
  [[ $("$executable" --version) == "xrun $version" ]] || fail SELF_CHECK_FAILED 'Installed CLI version check failed.'
  self_check='{"version":"'"$version"'"}'
fi
printf '{"ok":true,"component":"%s","version":"%s","platform":"%s","changed":%s,"path":' "$component" "$version" "$platform" "$changed"
json_string "$destination"
printf ',"executable":'
json_string "$executable"
printf ',"desktop_executable":'
if [[ -n "$desktop_executable" ]]; then json_string "$desktop_executable"; else printf null; fi
printf ',"sha256":"%s","self_check":%s}\n' "$digest" "$self_check"
replaced=0

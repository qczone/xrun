#!/bin/bash
# Install the macOS desktop App or Linux CLI from a versioned release.
set -Eeuo pipefail

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
path_temp=

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
  if [[ -n "$path_temp" ]]; then rm -f -- "$path_temp"; fi
  exit "$status"
}
trap cleanup EXIT
trap 'fail INSTALL_FAILED "Installation failed; see stderr for details."' ERR
trap 'fail INTERRUPTED "Installation interrupted."' INT TERM

while (($#)); do
  case "$1" in
    --help|-h)
      printf '%s\n' 'Usage: bash install.sh --version VERSION [--arch x86_64|arm64] [--install-dir DIR] [--source-dir DIR | --base-url HTTPS_URL]'
      exit 0 ;;
    --version|--arch|--install-dir|--source-dir|--base-url)
      (($# >= 2)) || fail INVALID_ARGUMENT "Missing value for $1."
      case "$1" in
        --version) version=${2#v} ;;
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
[[ -z "$architecture" || "$architecture" == x86_64 || "$architecture" == arm64 ]] || fail INVALID_ARGUMENT 'Architecture must be x86_64 or arm64.'
[[ -z "$source_dir" || -z "$base_url" ]] || fail INVALID_ARGUMENT 'Choose source-dir or base-url.'
host_architecture=$(uname -m)
case $(uname -s) in
  Darwin)
    system=darwin
    component=app
    # A shell running under Rosetta still selects the native Apple Silicon release.
    if [[ $(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then host_architecture=arm64; fi
    ;;
  Linux)
    system=linux
    component=cli
    if [[ "$host_architecture" == aarch64 ]]; then host_architecture=arm64; fi
    if command -v jq > /dev/null; then parser=jq
    elif command -v python3 > /dev/null; then parser=python3
    else fail DEPENDENCY_MISSING 'Linux installation requires jq or Python 3 to read the release manifest.'; fi
    ;;
  *) fail UNSUPPORTED_PLATFORM 'install.sh supports macOS and Linux.' ;;
esac
[[ "$host_architecture" == x86_64 || "$host_architecture" == arm64 ]] || fail UNSUPPORTED_PLATFORM 'This release supports x86_64 and arm64.'
if [[ -z "$architecture" ]]; then architecture=$host_architecture; fi
if [[ "$system" == darwin ]]; then
  [[ "$host_architecture" == arm64 || "$architecture" == x86_64 ]] || fail UNSUPPORTED_PLATFORM 'An arm64 release cannot run on an Intel Mac.'
else
  [[ "$architecture" == "$host_architecture" ]] || fail UNSUPPORTED_PLATFORM 'Linux installation requires the native architecture.'
fi
platform="$system-$architecture"
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
field() {
  local manifest="$work_dir/xrun-$platform.json" value
  if [[ "$system" == darwin ]]; then
    value=$(/usr/bin/plutil -extract "$1" raw -o - "$manifest") || fail INVALID_MANIFEST 'Cannot read the release manifest.'
  elif [[ "$parser" == jq ]]; then
    value=$(jq -er --arg key "$1" 'getpath($key | split(".")) | strings, numbers' "$manifest") || fail INVALID_MANIFEST 'Cannot read the release manifest.'
  else
    value=$(python3 - "$manifest" "$1" <<'PYTHON'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source)
    for key in sys.argv[2].split("."):
        value = value[key]
    if type(value) not in (str, int):
        raise ValueError("manifest field must be a string or number")
    print(value)
except (OSError, ValueError, KeyError, TypeError) as error:
    print(error, file=sys.stderr)
    sys.exit(1)
PYTHON
    ) || fail INVALID_MANIFEST 'Cannot read the release manifest.'
  fi
  printf '%s\n' "$value"
}
fetch "xrun-$platform.json"
[[ $(field schema) == 1 && $(field version) == "$version" && $(field platform) == "$platform" ]] || fail INVALID_MANIFEST 'Manifest version or platform does not match the requested release.'
artifact=$(field "artifacts.$component.file") || fail INVALID_MANIFEST 'Cannot read the artifact name.'
digest=$(field "artifacts.$component.sha256") || fail INVALID_MANIFEST 'Cannot read the artifact SHA-256.'
if [[ "$component" == app ]]; then expected="xrun-app-$platform.zip"; else expected="xrun-$platform.tar.gz"; fi
[[ "$artifact" == "$expected" && "$digest" =~ ^[0-9a-f]{64}$ ]] || fail INVALID_MANIFEST 'Manifest artifact name or SHA-256 is invalid.'
fetch "$artifact"
if [[ "$system" == darwin ]]; then actual=$(shasum -a 256 "$work_dir/$artifact")
else actual=$(sha256sum "$work_dir/$artifact"); fi
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
  "$desktop_executable" --install-cli >&2 || fail CLI_INSTALL_FAILED 'Could not configure the terminal xrun command.'
else
  [[ $("$executable" --version) == "xrun $version" ]] || fail SELF_CHECK_FAILED 'Installed CLI version check failed.'
  self_check='{"version":"'"$version"'"}'
  configure_path() {
    local file=$1 mode=600 quoted block existing=
    if [[ -L "$file" ]]; then file=$(readlink -f -- "$file"); fi
    [[ ! -e "$file" || -f "$file" ]] || fail CLI_INSTALL_FAILED "Shell configuration is not a file: $file."
    quoted=${install_dir//\'/\'\\\'\'}
    block=$(printf '%s\n' '# >>> xrun CLI >>>' 'case ":${PATH-}:" in' \
      "  *':$quoted:'*) ;;" "  *) export PATH='$quoted'\${PATH:+:\$PATH} ;;" 'esac' '# <<< xrun CLI <<<')
    if [[ -f "$file" ]]; then
      existing=$(sed -n '/^# >>> xrun CLI >>>$/,/^# <<< xrun CLI <<<$/{p;}' "$file")
      if [[ "$existing" == "$block" ]]; then return; fi
      if grep -Fxq '# >>> xrun CLI >>>' "$file" || grep -Fxq '# <<< xrun CLI <<<' "$file"; then
        grep -Fxq '# >>> xrun CLI >>>' "$file" && grep -Fxq '# <<< xrun CLI <<<' "$file" \
          || fail CLI_INSTALL_FAILED "Incomplete xrun PATH block in $file."
      fi
      mode=$(stat -c '%a' -- "$file")
      [[ -w "$file" ]] && (( (8#$mode & 0222) != 0 )) || fail CLI_INSTALL_FAILED "Shell configuration is not writable: $file."
    fi
    path_temp=$(mktemp "$(dirname "$file")/.xrun-path.XXXXXX")
    if [[ -f "$file" ]]; then
      sed '/^# >>> xrun CLI >>>$/,/^# <<< xrun CLI <<<$/{d;}' "$file" > "$path_temp"
    fi
    printf '\n%s\n' "$block" >> "$path_temp"
    chmod "$mode" "$path_temp"
    mv -f -- "$path_temp" "$file"
    path_temp=
  }
  zsh_directory=${ZDOTDIR:-"$HOME"}
  for name in .zprofile .zshrc; do configure_path "$zsh_directory/$name"; done
  login="$HOME/.profile"
  for name in .bash_profile .bash_login .profile; do
    if [[ -e "$HOME/$name" ]]; then login="$HOME/$name"; break; fi
  done
  configure_path "$login"
  configure_path "$HOME/.bashrc"
fi
printf '{"ok":true,"component":"%s","version":"%s","platform":"%s","changed":%s,"path":' "$component" "$version" "$platform" "$changed"
json_string "$destination"
printf ',"executable":'
json_string "$executable"
printf ',"desktop_executable":'
if [[ -n "$desktop_executable" ]]; then json_string "$desktop_executable"; else printf null; fi
printf ',"sha256":"%s","self_check":%s}\n' "$digest" "$self_check"
replaced=0

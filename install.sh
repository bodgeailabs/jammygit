#!/usr/bin/env bash
# Supports macOS's Bash 3.2 as well as current Bash on Linux.
set -euo pipefail

temporary=''
staged_binary=''
child=''
blue='' bold='' dim='' reset=''
interactive=false

cleanup() {
  if [[ -n "$child" ]]; then kill "$child" 2>/dev/null || true; fi
  if [[ -n "$staged_binary" ]]; then rm -f -- "$staged_binary"; fi
  if [[ -n "$temporary" ]]; then rm -rf -- "$temporary"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

fail() { printf '\n  %sError:%s %s\n' "$bold" "$reset" "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "Required command not found: $1"; }

usage() {
  cat <<'HELP'
jammygit installer

Usage: bash install.sh [--version VERSION] [--bin-dir PATH] [--repo OWNER/REPO]

  --version VERSION  Install a specific tag (for example v0.1.0); default: latest
  --bin-dir PATH     Installation directory; default: ~/.local/bin
  --repo OWNER/REPO  GitHub repository; default: initiumailab/jammygit
  -h, --help         Show this help

Environment:
  JAMMYGIT_INSTALL_DIR  Override the default installation directory
  JAMMYGIT_REPO         Override the default repository
  NO_COLOR              Disable color and animated progress

Public releases use curl. Authenticated GitHub CLI (gh) is used when available,
including for private repositories. No sudo, Rust toolchain, or shell-file edits.
HELP
}

step() { printf '  %s%s/4%s  %s\n' "$blue" "$1" "$reset" "$2"; }
detail() { printf '       %s%s%s\n' "$dim" "$*" "$reset"; }

run_step() {
  local label=$1
  shift
  if [[ "$interactive" != true ]]; then
    if ! "$@" >"$temporary/step.log" 2>&1; then
      cat "$temporary/step.log" >&2
      fail "$label failed."
    fi
    return
  fi
  "$@" >"$temporary/step.log" 2>&1 &
  child=$!
  local frames=$'|/-\\' index=0 status=0
  while kill -0 "$child" 2>/dev/null; do
    printf '\r       %s%s%s %s' "$blue" "${frames:index%4:1}" "$reset" "$label"
    index=$((index + 1))
    sleep 0.08
  done
  wait "$child" || status=$?
  child=''
  printf '\r\033[2K'
  if [[ "$status" -ne 0 ]]; then
    cat "$temporary/step.log" >&2
    fail "$label failed."
  fi
  detail "$label complete"
}

download_files() {
  if [[ "$backend" == gh ]]; then
    GH_HOST=github.com gh release download "$tag" --repo "$repository" --pattern "$asset" --pattern SHA256SUMS --dir "$temporary"
  else
    local base="https://github.com/$repository/releases/download/$tag"
    curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 --max-time 180 \
      --proto '=https' --proto-redir '=https' --output "$temporary/$asset" "$base/$asset" || return 1
    curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 --max-time 60 \
      --proto '=https' --proto-redir '=https' --output "$temporary/SHA256SUMS" "$base/SHA256SUMS"
  fi
}

verify_archive() {
  local expected
  expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { count++; hash=$1 } END { if (count != 1) exit 1; print hash }' "$temporary/SHA256SUMS") || {
    printf 'Release has no unique checksum for %s\n' "$asset" >&2
    return 1
  }
  [[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || { printf 'Invalid SHA-256 checksum\n' >&2; return 1; }
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$temporary" && printf '%s  %s\n' "$expected" "$asset" | sha256sum -c -)
  else
    (cd "$temporary" && printf '%s  %s\n' "$expected" "$asset" | shasum -a 256 -c -)
  fi
}

install_binary() {
  # Stream only the binary member; archive paths are never extracted onto disk.
  # run_step checks this function in an `if`, which disables implicit errexit.
  # Propagate failures explicitly before running or replacing any binary.
  tar -xOzf "$temporary/$asset" -- jammygit > "$temporary/jammygit" || return 1
  chmod 755 "$temporary/jammygit" || return 1
  local actual
  actual=$("$temporary/jammygit" --version) || {
    printf 'The downloaded binary cannot run on this system.\n' >&2
    return 1
  }
  [[ "$actual" == "jammygit ${tag#v}" ]] || {
    printf 'Binary version mismatch: expected %s, got %s\n' "${tag#v}" "$actual" >&2
    return 1
  }
  cp -- "$temporary/jammygit" "$staged_binary" || return 1
  chmod 755 "$staged_binary" || return 1
  mv -f -- "$staged_binary" "$install_dir/jammygit"
}

main() {
  local repository=${JAMMYGIT_REPO:-initiumailab/jammygit}
  local install_dir=${JAMMYGIT_INSTALL_DIR:-${HOME:?HOME must be set}/.local/bin}
  local tag=latest backend=curl os arch target asset resolved
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version|--bin-dir|--repo)
        [[ $# -ge 2 && -n "$2" ]] || fail "$1 requires a value."
        case "$1" in --version) tag=$2 ;; --bin-dir) install_dir=$2 ;; --repo) repository=$2 ;; esac
        shift 2 ;;
      -h|--help) usage; return ;;
      *) fail "Unknown argument: $1. Use --help." ;;
    esac
  done
  [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail 'Repository must be OWNER/REPO.'
  if [[ "$tag" != latest ]]; then
    tag="v${tag#v}"
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || fail 'Invalid version. Use a tag such as v0.1.0.'
  fi
  if [[ -t 1 && ${TERM:-dumb} != dumb && -z ${NO_COLOR+x} ]]; then
    interactive=true
    blue=$'\033[38;2;105;153;224m'; bold=$'\033[1m'; dim=$'\033[2m'; reset=$'\033[0m'
  fi
  printf '\n  %s%sjammygit%s\n  %sGit for your terminal.%s\n\n' "$bold" "$blue" "$reset" "$dim" "$reset"
  need uname; need tar; need awk; need git
  if ! command -v sha256sum >/dev/null 2>&1; then need shasum; fi
  os=$(uname -s); arch=$(uname -m)
  case "$os" in Darwin) target=apple-darwin ;; Linux) target=unknown-linux-musl ;; *) fail "Unsupported OS: $os. Supported: macOS and Linux." ;; esac
  case "$arch" in arm64|aarch64) target="aarch64-$target" ;; x86_64|amd64) target="x86_64-$target" ;; *) fail "Unsupported CPU: $arch. Supported: x86-64 and ARM64." ;; esac
  step 1 'Detect platform'
  detail "$os / $arch → $target"
  temporary=$(mktemp -d "${TMPDIR:-/tmp}/jammygit-install.XXXXXX")
  if command -v gh >/dev/null 2>&1 && gh auth status --hostname github.com >/dev/null 2>&1; then
    backend=gh
  else
    need curl
  fi
  if [[ "$tag" == latest ]]; then
    if [[ "$backend" == gh ]]; then
      tag=$(GH_HOST=github.com gh release view --repo "$repository" --json tagName --jq .tagName) || fail "No release available. Check repository access and published releases."
    else
      resolved=$(curl --fail --silent --show-error --location --head --retry 3 --connect-timeout 15 --max-time 60 \
        --proto '=https' --proto-redir '=https' --output /dev/null --write-out '%{url_effective}' "https://github.com/$repository/releases/latest") || \
        fail 'No public release available. For a private repository, install gh and run gh auth login first.'
      tag=${resolved##*/}
    fi
  fi
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || fail 'GitHub did not return a valid release version.'
  asset="jammygit-$target.tar.gz"
  step 2 "Download $tag"
  run_step 'Downloading release' download_files
  step 3 'Verify SHA-256'
  run_step 'Verifying archive' verify_archive
  step 4 'Install binary'
  mkdir -p -- "$install_dir"
  install_dir=$(cd "$install_dir" && pwd -P)
  [[ ! -d "$install_dir/jammygit" ]] || fail "Destination is a directory: $install_dir/jammygit"
  staged_binary=$(mktemp "$install_dir/.jammygit.XXXXXX")
  run_step 'Installing jammygit' install_binary
  staged_binary=''
  printf '\n  %s%sReady — jammygit %s%s\n' "$bold" "$blue" "${tag#v}" "$reset"
  detail "$install_dir/jammygit"
  case ":${PATH:-}:" in
    *":$install_dir:"*) printf '\n  Run: %sjammygit%s\n\n' "$bold" "$reset" ;;
    *) printf '\n  Add it to your PATH:\n    export PATH=%q:"%s"\n\n  Then run: jammygit\n\n' "$install_dir" "\$PATH" ;;
  esac
}

main "$@"

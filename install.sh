#!/bin/sh
set -eu

repo="oshabana/aliaz"
version="${ALIAZ_VERSION:-latest}"
install_dir="${ALIAZ_INSTALL_DIR:-$HOME/.local/bin}"
release_base_url="${ALIAZ_RELEASE_BASE_URL:-}"

say() {
  printf '%s\n' "$*"
}

fail() {
  say "aliaz install: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

has_tty() {
  [ -r /dev/tty ] && [ -w /dev/tty ]
}

ask() {
  prompt="$1"
  default="${2:-}"
  answer=""

  printf '%s' "$prompt" > /dev/tty
  IFS= read -r answer < /dev/tty || answer=""
  if [ -z "$answer" ]; then
    answer="$default"
  fi
  printf '%s\n' "$answer"
}

download() {
  url="$1"
  output="$2"

  if need_cmd curl; then
    curl -fsSL "$url" -o "$output"
  elif need_cmd wget; then
    wget -qO "$output" "$url"
  else
    fail "curl or wget is required to download release assets"
  fi
}

platform_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os:$arch" in
    Darwin:arm64 | Darwin:aarch64)
      printf '%s\n' "aarch64-apple-darwin"
      ;;
    Darwin:x86_64 | Darwin:amd64)
      printf '%s\n' "x86_64-apple-darwin"
      ;;
    Linux:arm64 | Linux:aarch64)
      printf '%s\n' "aarch64-unknown-linux-gnu"
      ;;
    Linux:x86_64 | Linux:amd64)
      printf '%s\n' "x86_64-unknown-linux-gnu"
      ;;
    *)
      fail "unsupported platform: $os $arch"
      ;;
  esac
}

verify_checksum() {
  archive="$1"
  checksums="$2"
  checksum_file="$3"

  grep "  $archive\$" "$checksums" > "$checksum_file" ||
    fail "checksum for $archive was not found"

  if need_cmd sha256sum; then
    sha256sum -c "$checksum_file"
  elif need_cmd shasum; then
    shasum -a 256 -c "$checksum_file"
  else
    say "aliaz install: sha256sum or shasum not found; skipping checksum verification"
  fi
}

default_shells() {
  case "$(basename "${SHELL:-}")" in
    zsh | bash | fish)
      basename "$SHELL"
      ;;
    *)
      printf '%s\n' "zsh"
      ;;
  esac
}

configure_shells() {
  shells="${ALIAZ_INSTALL_SHELLS:-}"

  if [ -z "$shells" ]; then
    if has_tty; then
      default="$(default_shells)"
      shells="$(ask "aliaz install: configure shells [${default}; zsh bash fish, skip]: " "$default")"
    else
      say "aliaz install: shell integration skipped; set ALIAZ_INSTALL_SHELLS to configure non-interactively"
      return 0
    fi
  fi

  case "$shells" in
    skip | none | no)
      say "aliaz install: shell integration skipped"
      return 0
      ;;
    all)
      shells="zsh bash fish"
      ;;
  esac

  for shell in $shells; do
    case "$shell" in
      zsh | bash | fish)
        say "aliaz install: configuring $shell"
        "$install_dir/aliaz" init "$shell"
        ;;
      *)
        fail "unsupported shell for integration: $shell"
        ;;
    esac
  done
}

setup_sync() {
  mode="${ALIAZ_INSTALL_SYNC:-}"

  if [ -z "$mode" ]; then
    if ! has_tty; then
      return 0
    fi

    sync_answer="$(ask "aliaz install: set up encrypted sync now? [y/N]: " "n")"
    case "$sync_answer" in
      y | Y | yes | YES)
        mode="$(ask "aliaz install: sync command [login/register]: " "login")"
        ;;
      *)
        return 0
        ;;
    esac
  fi

  case "$mode" in
    skip | none | no)
      return 0
      ;;
    login | register)
      ;;
    *)
      fail "unsupported ALIAZ_INSTALL_SYNC value: $mode"
      ;;
  esac

  username="${ALIAZ_SYNC_USERNAME:-}"
  if [ -z "$username" ]; then
    if has_tty; then
      username="$(ask "aliaz install: username: " "")"
    else
      fail "ALIAZ_SYNC_USERNAME is required for non-interactive sync setup"
    fi
  fi

  [ -n "$username" ] || fail "username is required for sync setup"
  "$install_dir/aliaz" "$mode" --username "$username"
}

target="$(platform_target)"
archive="aliaz-$target.tar.gz"

if [ -n "$release_base_url" ]; then
  base_url="${release_base_url%/}"
elif [ "$version" = "latest" ]; then
  base_url="https://github.com/$repo/releases/latest/download"
else
  base_url="https://github.com/$repo/releases/download/$version"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/aliaz-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

say "aliaz install: downloading $archive"
download "$base_url/$archive" "$tmp_dir/$archive"
download "$base_url/checksums.txt" "$tmp_dir/checksums.txt"

(
  cd "$tmp_dir"
  verify_checksum "$archive" "checksums.txt" "$archive.sha256"
  tar -xzf "$archive"
)

[ -f "$tmp_dir/aliaz" ] || fail "release archive did not contain aliaz"

mkdir -p "$install_dir"
cp "$tmp_dir/aliaz" "$install_dir/aliaz"
chmod 755 "$install_dir/aliaz"

"$install_dir/aliaz" --help >/dev/null 2>&1 ||
  fail "installed binary did not run: $install_dir/aliaz"

say "aliaz install: installed $install_dir/aliaz"

configure_shells
setup_sync

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    say "aliaz install: add $install_dir to PATH to run aliaz from any shell"
    ;;
esac

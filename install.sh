#!/bin/sh
set -eu

repo="oshabana/aliaz"
version="${ALIAZ_VERSION:-latest}"
install_dir="${ALIAZ_INSTALL_DIR:-$HOME/.local/bin}"
release_base_url="${ALIAZ_RELEASE_BASE_URL:-}"

say() {
  printf '%s\n' "$*"
}

say_blank() {
  printf '\n'
}

fail() {
  say "aliaz install: $*" >&2
  exit 1
}

display_path() {
  path="$1"
  if [ -n "${HOME:-}" ]; then
    case "$path" in
      "$HOME")
        printf '%s\n' "~"
        return 0
        ;;
      "$HOME"/*)
        printf '~/%s\n' "${path#"$HOME"/}"
        return 0
        ;;
    esac
  fi
  printf '%s\n' "$path"
}

choice_label() {
  case "$1" in
    skip)
      printf '%s\n' "Skip for now"
      ;;
    register)
      printf '%s\n' "Register a new account"
      ;;
    login)
      printf '%s\n' "Log in to an existing account"
      ;;
    *)
      printf '%s\n' "$1"
      ;;
  esac
}

status_ok() {
  printf '  OK  %s\n' "$*"
}

status_note() {
  printf '  ->  %s\n' "$*"
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

pick_choice() {
  index="$1"
  shift

  i=1
  for option in "$@"; do
    if [ "$i" = "$index" ]; then
      printf '%s\n' "$option"
      return 0
    fi
    i=$((i + 1))
  done

  return 1
}

menu_choice() {
  prompt="$1"
  default="$2"
  shift 2

  printf '%s\n\n' "$prompt" > /dev/tty
  i=1
  for option in "$@"; do
    printf '  %s) %s\n' "$i" "$(choice_label "$option")" > /dev/tty
    i=$((i + 1))
  done
  printf '\n' > /dev/tty

  while :; do
    printf '%s' "Choice [$default]: " > /dev/tty
    IFS= read -r answer < /dev/tty || answer=""
    if [ -z "$answer" ]; then
      answer="$default"
    fi

    case "$answer" in
      *[!0-9]*)
        ;;
      *)
        choice="$(pick_choice "$answer" "$@")" || choice=""
        if [ -n "$choice" ]; then
          printf '%s\n' "$choice"
          return 0
        fi
        ;;
    esac

    for option in "$@"; do
      if [ "$answer" = "$option" ]; then
        printf '%s\n' "$option"
        return 0
      fi
    done

    printf '%s\n' "Invalid choice. Enter a number or option name." > /dev/tty
  done
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
    sha256sum -c "$checksum_file" >/dev/null
  elif need_cmd shasum; then
    shasum -a 256 -c "$checksum_file" >/dev/null
  else
    say "aliaz install: sha256sum or shasum not found; skipping checksum verification"
  fi
}

default_shell() {
  case "$(basename "${SHELL:-}")" in
    zsh | bash | fish)
      basename "$SHELL"
      ;;
    *)
      printf '%s\n' "zsh"
      ;;
  esac
}

shells_from_selection() {
  selection="$1"
  resolved=""

  case "$selection" in
    skip | none | no)
      printf '%s\n' ""
      return 0
      ;;
    all)
      printf '%s\n' "zsh bash fish"
      return 0
      ;;
  esac

  for token in $selection; do
    case "$token" in
      1 | zsh)
        choice="zsh"
        ;;
      2 | bash)
        choice="bash"
        ;;
      3 | fish)
        choice="fish"
        ;;
      4 | all)
        printf '%s\n' "zsh bash fish"
        return 0
        ;;
      5 | skip | none | no)
        printf '%s\n' ""
        return 0
        ;;
      *)
        return 1
        ;;
    esac

    if [ -z "$resolved" ]; then
      resolved="$choice"
    else
      case " $resolved " in
        *" $choice "*) continue ;;
      esac
      resolved="$resolved $choice"
    fi
  done

  printf '%s\n' "$resolved"
}

configure_shells() {
  shells="${ALIAZ_INSTALL_SHELLS:-}"
  configured_shells=""

  if [ -z "$shells" ]; then
    if has_tty; then
      current_shell="$(default_shell)"
      default_choice=1
      case "$current_shell" in
        zsh)
          default_choice=1
          ;;
        bash)
          default_choice=2
          ;;
        fish)
          default_choice=3
          ;;
      esac
      say_blank
      say "Shell integration"
      say
      say "  1) zsh  recommended"
      say "  2) bash"
      say "  3) fish"
      say "  4) all"
      say "  5) skip"
      say
      say "Press Enter for your current shell: $current_shell"
      say

      while :; do
        printf '%s' "Choice [$default_choice]: " > /dev/tty
        IFS= read -r answer < /dev/tty || answer=""
        if [ -z "$answer" ]; then
          answer="$default_choice"
        fi

        shells="$(shells_from_selection "$answer")" || shells=""
        if [ -n "$shells" ] || [ "$answer" = "5" ] || [ "$answer" = "skip" ] || [ "$answer" = "none" ] || [ "$answer" = "no" ]; then
          break
        fi

        say "Invalid choice. Enter one or more numbers, shell names, all, or skip."
      done
    else
      status_note "shell integration skipped; set ALIAZ_INSTALL_SHELLS to configure non-interactively"
      return 0
    fi
  fi

  case "$shells" in
    skip | none | no)
      status_note "shell integration skipped"
      return 0
      ;;
    all)
      shells="zsh bash fish"
      ;;
  esac

  for shell in $shells; do
    case "$shell" in
      zsh | bash | fish)
        status_note "configuring $shell"
        "$install_dir/aliaz" init "$shell"
        if [ -z "$configured_shells" ]; then
          configured_shells="$shell"
        else
          configured_shells="$configured_shells $shell"
        fi
        ;;
      *)
        fail "unsupported shell for integration: $shell"
        ;;
    esac
  done
}

setup_sync() {
  mode="${ALIAZ_INSTALL_SYNC:-}"
  sync_summary="Skipped"

  if [ -z "$mode" ]; then
    if ! has_tty; then
      return 0
    fi

    say_blank
    mode="$(menu_choice "Aliaz encrypted sync" 1 skip register login)"
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
  sync_summary="$mode as $username"
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

say "Aliaz installer"
say_blank
say "System"
printf '  %-12s %s\n' "Platform" "$target"
printf '  %-12s %s\n' "Version" "$version"
printf '  %-12s %s\n' "Install dir" "$(display_path "$install_dir")"
say_blank
say "Download"
download "$base_url/$archive" "$tmp_dir/$archive"
status_ok "$archive"
download "$base_url/checksums.txt" "$tmp_dir/checksums.txt"
status_ok "checksums.txt"

(
  cd "$tmp_dir"
  verify_checksum "$archive" "checksums.txt" "$archive.sha256"
  tar -xzf "$archive"
)
status_ok "checksum verified"

[ -f "$tmp_dir/aliaz" ] || fail "release archive did not contain aliaz"

say_blank
say "Install"
mkdir -p "$install_dir"
cp "$tmp_dir/aliaz" "$install_dir/aliaz"
chmod 755 "$install_dir/aliaz"
status_ok "copied to $(display_path "$install_dir")/aliaz"

"$install_dir/aliaz" --help >/dev/null 2>&1 ||
  fail "installed binary did not run: $install_dir/aliaz"
status_ok "binary verified"

configure_shells
setup_sync

path_note=""
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    path_note="Add $(display_path "$install_dir") to PATH to run aliaz from any shell."
    ;;
esac

say_blank
say "Aliaz is installed"
say_blank
printf '  %-8s %s\n' "Binary" "$(display_path "$install_dir")/aliaz"
if [ -n "$configured_shells" ]; then
  printf '  %-8s %s configured\n' "Shell" "$configured_shells"
else
  printf '  %-8s %s\n' "Shell" "Not configured"
fi
printf '  %-8s %s\n' "Sync" "$sync_summary"
printf '  %-8s %s\n' "Verify" "aliaz --help"

if [ -n "$path_note" ]; then
  say_blank
  status_note "$path_note"
fi

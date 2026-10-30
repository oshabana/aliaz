use super::*;

pub(super) fn sh(binary: &str, source: &str) -> Vec<String> {
    let script = r#"# Managed by Aliaz. Do not edit.
unalias aliaz 2>/dev/null || :
__aliaz_bin=__BINARY__
__aliaz_choose() {
  local __aliaz_result __aliaz_status
  __aliaz_result=$("$__aliaz_bin" select --print-command "$@"; __aliaz_status=$?; printf '\036'; exit "$__aliaz_status")
  __aliaz_status=$?
  __aliaz_selected_command=${__aliaz_result%?}
  __aliaz_selected_command=${__aliaz_selected_command%$'\n'}
  return "$__aliaz_status"
}
__aliaz_pick() {
  local __aliaz_selected_command
  if [ -n "${BASH_VERSION:-}" ] && [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
    printf '%s\n' 'Command insertion is unavailable in this Bash. Type aliaz to review and copy a command.' >&2
    return 1
  fi
  __aliaz_choose || return $?
  [ -n "$__aliaz_selected_command" ] || return 0
  if [ -n "${ZSH_VERSION:-}" ]; then
    BUFFER=$__aliaz_selected_command
    CURSOR=${#BUFFER}
    zle redisplay
  else
    READLINE_LINE=$__aliaz_selected_command
    local LC_ALL=C
    READLINE_POINT=${#READLINE_LINE}
  fi
}
__aliaz_last() {
  local __aliaz_offset __aliaz_value __aliaz_classify __aliaz_event
  if [ -n "${ZSH_VERSION:-}" ]; then
    zmodload zsh/parameter || return 1
  fi
  __aliaz_offset=1
  while [ "$__aliaz_offset" -le 50 ]; do
    if [ -n "${ZSH_VERSION:-}" ]; then
      __aliaz_event=$((HISTCMD - __aliaz_offset + 1))
      __aliaz_value=${history[$__aliaz_event]-}
    else
      __aliaz_value=$(fc -ln -"$__aliaz_offset" -"$__aliaz_offset" 2>/dev/null; printf '\036')
      __aliaz_value=${__aliaz_value%?}
      __aliaz_value=${__aliaz_value%$'\n'}
      # fc's display prefix is a tab and one space; command whitespace follows it.
      __aliaz_value=${__aliaz_value#$'\t '}
    fi
    __aliaz_classify=${__aliaz_value#"${__aliaz_value%%[![:space:]]*}"}
    case "$__aliaz_classify" in
      ''|aliaz|aliaz\ *|command\ aliaz\ *) ;;
      *) ALIAZ_LAST_COMMAND=$__aliaz_value; export ALIAZ_LAST_COMMAND; return 0 ;;
    esac
    __aliaz_offset=$((__aliaz_offset + 1))
  done
  unset ALIAZ_LAST_COMMAND
}
aliaz() {
  local __aliaz_status __aliaz_shell __aliaz_selected_command __aliaz_arg __aliaz_run __aliaz_raw __aliaz_options
  if [ "${1:-}" = save ] && [ "${2:-}" = --last ]; then __aliaz_last; fi
  if [ $# -eq 0 ] && [[ $- != *i* ]]; then "$__aliaz_bin"; return $?; fi
  if [ $# -eq 0 ] || [ "${1:-}" = select ]; then
    [ $# -eq 0 ] || shift
    __aliaz_run=0; __aliaz_raw=0; __aliaz_options=1
    for __aliaz_arg in "$@"; do
      if [ "$__aliaz_arg" = -- ]; then __aliaz_options=0; continue; fi
      [ "$__aliaz_options" = 1 ] || continue
      [ "$__aliaz_arg" != --run ] || __aliaz_run=1
      [ "$__aliaz_arg" != --print-command ] || __aliaz_raw=1
      [ "$__aliaz_arg" != --print0 ] || __aliaz_raw=1
      [ "$__aliaz_arg" != --help ] || __aliaz_raw=1
      [ "$__aliaz_arg" != -h ] || __aliaz_raw=1
    done
    if [ "$__aliaz_raw" = 1 ]; then "$__aliaz_bin" select "$@"; return $?; fi
    if [ "$__aliaz_run" = 1 ]; then
      # --run is the only path that evaluates a selected command.
      local -a __aliaz_args
      __aliaz_args=()
      __aliaz_options=1
      for __aliaz_arg in "$@"; do
        if [ "$__aliaz_arg" = -- ]; then __aliaz_options=0; fi
        if [ "$__aliaz_options" = 0 ] || [ "$__aliaz_arg" != --run ]; then __aliaz_args+=("$__aliaz_arg"); fi
      done
      __aliaz_choose "${__aliaz_args[@]}" || return $?
      [ -n "$__aliaz_selected_command" ] || return 0
      eval "$__aliaz_selected_command"
      return $?
    fi
    __aliaz_choose "$@" || return $?
    [ -n "$__aliaz_selected_command" ] || return 0
    if [ -n "${ZSH_VERSION:-}" ] && [[ -o interactive ]]; then
      print -rz -- "$__aliaz_selected_command"
    else
      printf '%s\n' "$__aliaz_selected_command"
      if [ -n "${BASH_VERSION:-}" ] && [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
        printf '%s\n' 'Command ready to copy. Use aliaz select --run to run a selection explicitly.' >&2
      else
        printf '%s\n' 'Command ready to copy. Use the Aliaz shortcut to insert it for editing, or aliaz select --run to run it.' >&2
      fi
    fi
    return 0
  fi
  if [ "${1:-}" = save ]; then
    # Also accept --last after other save options.
    for __aliaz_arg in "$@"; do [ "$__aliaz_arg" != --last ] || __aliaz_last; done
  fi
  "$__aliaz_bin" "$@"
  __aliaz_status=$?
  if [ "${1:-}" = save ]; then unset ALIAZ_LAST_COMMAND; fi
  if [ "$__aliaz_status" -eq 0 ]; then
    case "${1:-}" in
      add|save|describe|favorite|edit|rm|delete|undo|restore|collection|migrate|import|sync|login)
        __aliaz_shell=""
        if [ -n "${ZSH_VERSION:-}" ]; then __aliaz_shell=zsh
        elif [ -n "${BASH_VERSION:-}" ]; then __aliaz_shell=bash; fi
        if [ -n "$__aliaz_shell" ]; then "$__aliaz_bin" init "$__aliaz_shell" >/dev/null && __SOURCE__; fi
        ;;
    esac
  fi
  return "$__aliaz_status"
}
if [ -n "${ZSH_VERSION:-}" ]; then
  if [[ -o interactive ]]; then
    zle -N __aliaz_pick
    if [ "${ALIAZ_PICKER_KEY:-\C-x\C-a}" != off ] && [ "${__aliaz_bound_key:-}" != "${ALIAZ_PICKER_KEY:-\C-x\C-a}" ]; then
      if [[ $(bindkey "${ALIAZ_PICKER_KEY:-\C-x\C-a}") = *undefined-key* ]]; then
        bindkey "${ALIAZ_PICKER_KEY:-\C-x\C-a}" __aliaz_pick
        __aliaz_bound_key=${ALIAZ_PICKER_KEY:-\C-x\C-a}
      else
        printf '%s\n' 'Aliaz shortcut is already bound. Choose an unused ALIAZ_PICKER_KEY or set it to off.' >&2
      fi
    fi
  fi
elif [ -n "${BASH_VERSION:-}" ] && [[ $- = *i* ]]; then
  if [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
    if [ "${ALIAZ_PICKER_KEY:-default}" != off ] && [ -z "${__aliaz_old_bash_notice:-}" ]; then
      printf '%s\n' 'Command insertion is unavailable in this Bash. Type aliaz to review and copy a command.' >&2
      __aliaz_old_bash_notice=1
    fi
  elif [ "${ALIAZ_PICKER_KEY:-\C-x\C-a}" != off ] && [ "${__aliaz_bound_key:-}" != "${ALIAZ_PICKER_KEY:-\C-x\C-a}" ]; then
    if { bind -p; bind -s; bind -X 2>/dev/null; } | grep -F -- "\"${ALIAZ_PICKER_KEY:-\C-x\C-a}\":" >/dev/null; then
      printf '%s\n' 'Aliaz shortcut is already bound. Choose an unused ALIAZ_PICKER_KEY or set it to off.' >&2
    else
      bind -x "\"${ALIAZ_PICKER_KEY:-\C-x\C-a}\":__aliaz_pick"
      __aliaz_bound_key=${ALIAZ_PICKER_KEY:-\C-x\C-a}
    fi
  fi
fi
"#;
    script
        .replace("__BINARY__", &shell_quote(binary))
        .replace("__SOURCE__", source)
        .lines()
        .map(str::to_owned)
        .collect()
}

pub(super) fn fish(binary: &str, path: &Path) -> Vec<String> {
    let script = r#"# Managed by Aliaz. Do not edit.
set -g __aliaz_bin __BINARY__
function __aliaz_choose
  set -g __aliaz_selected_command ''
  "$__aliaz_bin" select --print0 $argv | read --null --global __aliaz_selected_command
  set -l result $pipestatus[1]
  if test $result -ne 0; return $result; end
  return 0
end
function __aliaz_pick
  __aliaz_choose; or return $status
  if test -n "$__aliaz_selected_command"
    commandline --replace -- "$__aliaz_selected_command"
  end
  commandline --function repaint
end
function __aliaz_last
  set -e ALIAZ_LAST_COMMAND
  for entry in (history search --max=50 --null | string split0)
    if not string match -qr '^\s*(command\s+)?aliaz(\s|$)' -- "$entry"
      set -gx ALIAZ_LAST_COMMAND "$entry"
      return
    end
  end
end
function aliaz
  if test (count $argv) -eq 0; and not status is-interactive
    "$__aliaz_bin"
    return $status
  end
  if test (count $argv) -eq 0; or test "$argv[1]" = select
    set -l args $argv
    if test (count $args) -gt 0; set -e args[1]; end
    set -l raw 0
    set -l run 0
    set -l options 1
    set -l filtered
    for arg in $args
      if test "$arg" = --; set options 0; end
      if test $options -eq 1
        if test "$arg" = --print-command; or test "$arg" = --print0; or test "$arg" = --help; or test "$arg" = -h; set raw 1; end
        if test "$arg" = --run; set run 1; continue; end
      end
      set -a filtered "$arg"
    end
    if test $raw -eq 1
      "$__aliaz_bin" select $args
      return $status
    end
    set args $filtered
    __aliaz_choose $args; or return $status
    if test -z "$__aliaz_selected_command"; return 0; end
    if test $run -eq 1
      eval "$__aliaz_selected_command"
      return $status
    end
    if status is-interactive
      commandline --replace -- "$__aliaz_selected_command"
    else
      printf '%s\n' "$__aliaz_selected_command"
      printf '%s\n' 'Command ready to copy. Use the Aliaz shortcut to insert it for editing.' >&2
    end
    return 0
  end
  if test "$argv[1]" = save; and contains -- --last $argv; __aliaz_last; end
  "$__aliaz_bin" $argv
  set -l result $status
  if test "$argv[1]" = save; set -e ALIAZ_LAST_COMMAND; end
  if test $result -eq 0
    switch $argv[1]
      case add save describe favorite edit rm delete undo restore collection migrate import sync login
        "$__aliaz_bin" init fish >/dev/null; and source __PATH__
    end
  end
  return $result
end
if status is-interactive
  set -l key \cx\ca
  if set -q ALIAZ_PICKER_KEY; set key "$ALIAZ_PICKER_KEY"; end
  if test "$key" != off
    if not set -q __aliaz_bound_key; or test "$__aliaz_bound_key" != "$key"
      if bind "$key" >/dev/null 2>&1
        printf '%s\n' 'Aliaz shortcut is already bound. Choose an unused ALIAZ_PICKER_KEY or set it to off.' >&2
      else
        bind "$key" __aliaz_pick
        set -g __aliaz_bound_key "$key"
      end
    end
  end
end
"#;
    script
        .replace("__BINARY__", &shell_quote(binary))
        .replace("__PATH__", &shell_quote(&path.display().to_string()))
        .lines()
        .map(str::to_owned)
        .collect()
}

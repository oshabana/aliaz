use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn cmd(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("aliaz").expect("binary exists");
    command.env("ALIAS_TOOL_HOME", home.path());
    command.env("ALIAZ_CONFIG_HOME", home.path().join(".config"));
    command
}

#[test]
fn add_list_edit_and_delete_aliases() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added gs"));

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"));

    cmd(&home)
        .args(["edit", "gs", "git status --short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated gs"));

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status --short"));

    cmd(&home)
        .args(["rm", "gs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted gs"));

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn init_outputs_shell_safe_alias_definitions() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "quote", r#"printf '%s\n' "$HOME""#])
        .assert()
        .success();

    cmd(&home)
        .args(["generate", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            r#"alias quote='printf '\''%s\n'\'' "$HOME"'"#,
        ));
}

#[test]
fn init_writes_managed_zsh_alias_file() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success();

    cmd(&home)
        .args(["init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            r#"Add this line to your zsh startup file: source "$HOME/.config/aliaz/aliases.sh""#,
        ));

    let aliases = fs::read_to_string(home.path().join(".config/aliaz/aliases.sh"))
        .expect("aliases.sh exists");
    assert!(aliases.contains("alias gs='git status'"));
}

#[test]
fn init_writes_managed_bash_alias_file() {
    let home = TempDir::new().expect("temp home");

    cmd(&home).args(["add", "ll", "ls -lah"]).assert().success();

    cmd(&home)
        .args(["init", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            r#"Add this line to your bash startup file: source "$HOME/.config/aliaz/aliases.sh""#,
        ));

    let aliases = fs::read_to_string(home.path().join(".config/aliaz/aliases.sh"))
        .expect("aliases.sh exists");
    assert!(aliases.contains("alias ll='ls -lah'"));
}

#[test]
fn init_writes_managed_fish_alias_file() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gco", "git checkout"])
        .assert()
        .success();

    cmd(&home)
        .args(["init", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote"));

    let aliases = fs::read_to_string(home.path().join(".config/fish/conf.d/aliaz.fish"))
        .expect("aliaz.fish exists");
    assert!(aliases.contains("alias gco 'git checkout'"));
}

#[test]
fn migrate_imports_aliases_from_zshrc_style_file() {
    let home = TempDir::new().expect("temp home");
    let zshrc = home.path().join(".zshrc");
    fs::write(
        &zshrc,
        r#"
alias gs='git status'
alias ll="ls -lah"
# alias ignored='nope'
function nope() { true; }
"#,
    )
    .expect("write zshrc");

    cmd(&home)
        .args(["migrate", "--from", zshrc.to_str().expect("utf8 path")])
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported 2 aliases"));

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"))
        .stdout(predicate::str::contains("ll\tls -lah"))
        .stdout(predicate::str::contains("ignored").not());
}

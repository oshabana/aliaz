use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::fs;
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

fn cmd(home: &TempDir) -> AssertCommand {
    let mut command = AssertCommand::cargo_bin("aliaz").expect("binary exists");
    command.env("HOME", home.path());
    command.env("ALIAS_TOOL_HOME", home.path());
    command.env("ALIAZ_CONFIG_HOME", home.path().join(".config"));
    command.env("ALIAZ_TEST_SECRET_HOME", home.path().join(".secrets"));
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
        .stdout(predicate::str::contains("Configured"));

    let aliases = fs::read_to_string(home.path().join(".config/aliaz/aliases.sh"))
        .expect("aliases.sh exists");
    assert!(aliases.contains("alias gs='git status'"));
    assert!(aliases.contains("aliaz()"));
    assert!(aliases.contains("source \"$HOME/.config/aliaz/aliases.sh\""));
}

#[test]
fn init_writes_managed_bash_alias_file() {
    let home = TempDir::new().expect("temp home");

    cmd(&home).args(["add", "ll", "ls -lah"]).assert().success();

    cmd(&home)
        .args(["init", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Configured"));

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
    assert!(aliases.contains("function aliaz"));
}

#[test]
fn init_updates_zsh_startup_file_only_once() {
    let home = TempDir::new().expect("temp home");

    cmd(&home).args(["init", "zsh"]).assert().success();
    cmd(&home).args(["init", "zsh"]).assert().success();

    let zshrc = fs::read_to_string(home.path().join(".zshrc")).expect(".zshrc exists");
    let source_line = r#"source "$HOME/.config/aliaz/aliases.sh""#;
    assert_eq!(zshrc.matches(source_line).count(), 1);
}

#[test]
fn bash_wrapper_refreshes_aliases_after_add_in_same_shell() {
    let home = TempDir::new().expect("temp home");
    let bin = assert_cmd::cargo::cargo_bin("aliaz");
    let bin_dir = bin.parent().expect("binary parent");

    let output = ProcessCommand::new("bash")
        .arg("-lc")
        .arg(
            r#"
            shopt -s expand_aliases
            aliaz init bash >/dev/null
            source "$HOME/.config/aliaz/aliases.sh"
            aliaz add hi "printf aliaz-ok" >/dev/null
            hi
            "#,
        )
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_TEST_SECRET_HOME", home.path().join(".secrets"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "bash failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "aliaz-ok");
}

#[test]
fn logout_removes_local_sync_config_and_recovery_phrase() {
    let home = TempDir::new().expect("temp home");
    let config_dir = home.path().join(".config/aliaz");
    let secret_dir = home.path().join(".secrets");
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        config_dir.join("config.json"),
        r#"{
  "sync_url": "https://sync.example",
  "username": "ada",
  "user_id": "user-1",
  "token": "token-1",
  "latest_version": 7
}
"#,
    )
    .expect("config");
    fs::write(secret_dir.join("user-1"), "recovery phrase").expect("secret");

    cmd(&home)
        .args(["logout"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logged out ada"));

    assert!(!config_dir.join("config.json").exists());
    assert!(!secret_dir.join("user-1").exists());
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

#[test]
fn export_and_import_round_trip_aliases() {
    let source = TempDir::new().expect("source home");
    let target = TempDir::new().expect("target home");
    let export_path = source.path().join("aliases.json");

    cmd(&source)
        .args(["add", "gs", "git status"])
        .assert()
        .success();
    cmd(&source)
        .args(["add", "ll", "ls -lah"])
        .assert()
        .success();

    cmd(&source)
        .args([
            "export",
            "--output",
            export_path.to_str().expect("utf8 path"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Exported 2 aliases"));

    cmd(&target)
        .args(["import", export_path.to_str().expect("utf8 path")])
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported 2 aliases"));

    cmd(&target)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"))
        .stdout(predicate::str::contains("ll\tls -lah"));
}

#[test]
fn status_and_doctor_report_local_state() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success();
    cmd(&home).args(["init", "zsh"]).assert().success();

    cmd(&home)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("aliases: 1"))
        .stdout(predicate::str::contains("pending sync records: 1"))
        .stdout(predicate::str::contains("sync: not configured"));

    cmd(&home)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("database: ok"))
        .stdout(predicate::str::contains("zsh/bash integration: ok"))
        .stdout(predicate::str::contains("sync config: missing"));
}

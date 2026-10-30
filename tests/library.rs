use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn cmd(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("aliaz").unwrap();
    cmd.env("HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"));
    cmd
}

fn export(home: &TempDir) -> serde_json::Value {
    let output = cmd(home)
        .args(["export"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn no_arguments_without_a_terminal_prints_help_and_save_explains_script_path() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .assert()
        .success()
        .stdout(predicate::str::contains("Find, save, and reuse"));
    cmd(&home)
        .args(["save"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("aliaz add NAME COMMAND"));
    cmd(&home)
        .args(["select", "--first"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No saved commands"));
}

#[test]
fn purpose_tags_and_full_command_search_preserve_plain_list_output() {
    let home = TempDir::new().unwrap();
    let command = "printf '%s\\n' 'it is quoted' | cat\nprintf done\n";
    cmd(&home)
        .args([
            "add",
            "check",
            command,
            "--description",
            "Review release health",
            "--tags",
            "release,ops",
        ])
        .assert()
        .success();
    for query in ["health", "ops", "quoted", "review release"] {
        cmd(&home)
            .args(["select", "--first", "--print-command", query])
            .assert()
            .success()
            .stdout(format!("{command}\n"));
    }
    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(format!("check\t{command}\n"));
    cmd(&home)
        .args(["list", "--details"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review release health\tops,release",
        ));
    cmd(&home)
        .args(["select", "--first", "--print0", "ops"])
        .assert()
        .success()
        .stdout(format!("{command}\0").into_bytes());
}

#[test]
fn selection_is_safe_and_run_is_explicit() {
    let home = TempDir::new().unwrap();
    let marker = home.path().join("executed");
    let command = format!("printf yes > '{}'", marker.display());
    cmd(&home)
        .args(["add", "marker", &command])
        .assert()
        .success();
    cmd(&home)
        .args(["select", "--first", "marker"])
        .assert()
        .success()
        .stdout(format!("{command}\n"));
    assert!(!marker.exists());
    cmd(&home)
        .env("SHELL", "/bin/sh")
        .args(["select", "--run", "--first", "marker"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(marker).unwrap(), "yes");
}

#[test]
fn favorite_then_recent_selection_orders_an_empty_query() {
    let home = TempDir::new().unwrap();
    for name in ["alpha", "beta", "gamma"] {
        cmd(&home).args(["add", name, name]).assert().success();
    }
    cmd(&home)
        .args(["select", "--first", "--print-command", "beta"])
        .assert()
        .success();
    cmd(&home)
        .args(["select", "--first", "--print-command"])
        .assert()
        .success()
        .stdout("beta\n");
    cmd(&home).args(["favorite", "gamma"]).assert().success();
    cmd(&home)
        .args(["select", "--first", "--print-command"])
        .assert()
        .success()
        .stdout("gamma\n");
    cmd(&home)
        .args(["favorite", "gamma", "--off"])
        .assert()
        .success();
    assert!(
        !export(&home)["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "gamma")
            .unwrap()["favorite"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn old_imports_preserve_details_and_new_exports_round_trip_them() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "add",
            "status",
            "old",
            "--description",
            "Working tree",
            "--tags",
            "git",
            "--favorite",
        ])
        .assert()
        .success();
    let old = home.path().join("old.json");
    fs::write(
        &old,
        r#"{"version":1,"aliases":[{"name":"status","command":"new"}]}"#,
    )
    .unwrap();
    cmd(&home)
        .args(["import", old.to_str().unwrap(), "--replace", "--yes"])
        .assert()
        .success();
    let value = export(&home);
    assert_eq!(value["version"], 3);
    assert_eq!(value["aliases"][0]["description"], "Working tree");
    assert_eq!(value["aliases"][0]["favorite"], true);
    let file = home.path().join("new.json");
    fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
    let second = TempDir::new().unwrap();
    cmd(&second)
        .args(["import", file.to_str().unwrap(), "--yes"])
        .assert()
        .success();
    assert_eq!(export(&second), value);
}

#[test]
fn import_preview_skips_replacements_by_default_and_apply_is_atomic() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["add", "keep", "before"])
        .assert()
        .success();
    let file = home.path().join("aliases.json");
    fs::write(&file, r#"{"version":2,"aliases":[{"name":"keep","command":"after"},{"name":"new","command":"new"}]}"#).unwrap();
    let before = export(&home);
    cmd(&home)
        .args(["import", file.to_str().unwrap(), "--preview", "--replace"])
        .assert()
        .success()
        .stderr(
            predicate::str::contains("1 additions, 1 replacements, 0 skipped")
                .and(predicate::str::contains("before\nNew command:\nafter")),
        );
    assert_eq!(export(&home), before);
    cmd(&home)
        .args(["import", file.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "1 additions, 0 replacements, 1 skipped",
        ));
    assert_eq!(export(&home)["aliases"][0]["command"], "before");
    fs::write(&file, r#"{"version":2,"collections":[{"name":"work"}],"aliases":[{"name":"good","command":"true","collection":"work"},{"name":"bad!","command":"false"}]}"#).unwrap();
    let before = export(&home);
    cmd(&home)
        .args(["import", file.to_str().unwrap(), "--yes"])
        .assert()
        .failure();
    assert_eq!(export(&home), before);
}

#[test]
fn replacements_need_an_explicit_flag_and_undo_restores_all_details() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "add",
            "status",
            "before",
            "--description",
            "Saved description",
            "--tags",
            "one,two",
            "--favorite",
        ])
        .assert()
        .success();
    let before = export(&home);
    cmd(&home)
        .args(["add", "status", "after"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("use --replace"));
    cmd(&home)
        .args([
            "add",
            "status",
            "after",
            "--replace",
            "--description",
            "Changed",
            "--tags",
            "three",
        ])
        .assert()
        .success();
    cmd(&home).args(["undo"]).assert().success();
    assert_eq!(export(&home), before);
    cmd(&home).args(["rm", "status"]).assert().success();
    assert!(export(&home)["aliases"].as_array().unwrap().is_empty());
    cmd(&home).args(["undo"]).assert().success();
    assert_eq!(export(&home), before);
}

#[test]
fn undo_does_not_erase_newer_changes_or_an_unrelated_command() {
    let home = TempDir::new().unwrap();
    cmd(&home).args(["add", "one", "old"]).assert().success();
    cmd(&home)
        .args(["edit", "one", "middle"])
        .assert()
        .success();
    cmd(&home)
        .args(["add", "two", "unrelated"])
        .assert()
        .success();
    cmd(&home).args(["edit", "one", "new"]).assert().success();
    cmd(&home)
        .args(["undo", "2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("changed since"));
    cmd(&home).args(["undo", "4"]).assert().success();
    cmd(&home).args(["undo", "2"]).assert().success();
    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout("one\told\ntwo\tunrelated\n");
}

#[test]
fn migrate_preview_does_not_change_the_source_and_undo_restores_replacements() {
    let home = TempDir::new().unwrap();
    let source = home.path().join("source.sh");
    let contents = "alias one='printf old'\nalias two='printf \"quoted\" | cat'\n";
    fs::write(&source, contents).unwrap();
    cmd(&home)
        .args(["migrate", "--from", source.to_str().unwrap(), "--preview"])
        .assert()
        .success();
    assert!(export(&home)["aliases"].as_array().unwrap().is_empty());
    cmd(&home)
        .args(["migrate", "--from", source.to_str().unwrap(), "--yes"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&source).unwrap(), contents);
    cmd(&home).args(["undo"]).assert().success();
    assert!(export(&home)["aliases"].as_array().unwrap().is_empty());
}

#[test]
fn bash_widget_preserves_multiline_utf8_and_cancelled_buffer_and_byte_cursor() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        cmd(&home).args(["init", "bash"]).assert().success();
        let stub = home.path().join("picker");
        fs::write(&stub, "#!/bin/sh\nif [ \"$ALIAZ_PICKER_FIXTURE\" = cancel ]; then exit 0; fi\nprintf '%s\\n' \"$ALIAZ_PICKER_FIXTURE\"\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let command = "printf '%s' 'café 🐙 quoted' | cat\nprintf done\n\n";
        for cancel in [false, true] {
            let bash = if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/bash"
            } else {
                "bash"
            };
            if !std::process::Command::new(bash)
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "test ${BASH_VERSINFO[0]} -ge 4",
                ])
                .status()
                .is_ok_and(|status| status.success())
            {
                eprintln!("not checked: newer Bash is not installed");
                return;
            }
            let output = std::process::Command::new(bash)
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    r#"
                source "$HOME/.config/aliaz/aliases.sh"
                __aliaz_bin="$HOME/picker"
                READLINE_LINE='existing command'; READLINE_POINT=4
                __aliaz_pick
                printf '%s\000%s' "$READLINE_LINE" "$READLINE_POINT"
            "#,
                ])
                .env("HOME", home.path())
                .env("LC_ALL", "en_US.UTF-8")
                .env(
                    "ALIAZ_PICKER_FIXTURE",
                    if cancel { "cancel" } else { command },
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut fields = output.stdout.split(|byte| *byte == 0);
            assert_eq!(
                fields.next().unwrap(),
                if cancel {
                    b"existing command".as_slice()
                } else {
                    command.as_bytes()
                }
            );
            assert_eq!(
                fields.next().unwrap(),
                if cancel {
                    "4".to_owned()
                } else {
                    command.len().to_string()
                }
                .as_bytes()
            );
        }
    }
}

#[test]
fn generated_shell_wrappers_run_only_explicitly_and_keep_cd_and_environment() {
    for shell in ["bash", "zsh", "fish"] {
        if std::process::Command::new(shell)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("not checked: {shell} is not installed");
            continue;
        }
        let home = TempDir::new().unwrap();
        cmd(&home).args(["init", shell]).assert().success();
        cmd(&home)
            .args([
                "add",
                "change",
                "cd /; export ALIAZ_LIBRARY_FIXTURE=changed",
            ])
            .assert()
            .success();
        let script = if shell == "fish" {
            r#"source "$HOME/.config/fish/conf.d/aliaz.fish"
               aliaz select --first --run change
               printf '%s|%s' "$PWD" "$ALIAZ_LIBRARY_FIXTURE""#
        } else {
            r#"source "$HOME/.config/aliaz/aliases.sh"
               aliaz select --first --run change
               printf '%s|%s' "$PWD" "$ALIAZ_LIBRARY_FIXTURE""#
        };
        let output = std::process::Command::new(shell)
            .args(["-c", script])
            .env("HOME", home.path())
            .env("ALIAZ_DATA_HOME", home.path())
            .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"/|changed", "{shell}");
    }
}

fn check_last_command_capture(shells: &[&str]) {
    use std::io::Write;
    use std::process::Stdio;

    let commands = [
        "printf '%s\\n' 'quoted \"text\"' | cat",
        "  printf '%s\\n' 'leading spaces' | cat",
        "\tprintf '%s\\n' 'leading tab' | cat",
        "  \tprintf '%s\\n' 'first\nsecond' | cat",
    ];
    for shell in shells {
        if std::process::Command::new(shell)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("not checked: {shell} is not installed");
            continue;
        }
        let home = TempDir::new().unwrap();
        let is_bash = shell.ends_with("bash");
        cmd(&home)
            .args(["init", if is_bash { "bash" } else { "zsh" }])
            .assert()
            .success();
        for command in commands {
            let entered_command = if is_bash {
                command.replace('\t', "\u{16}\t")
            } else {
                command.to_owned()
            };
            let history_dump = if is_bash {
                "fc -l 1"
            } else {
                "printf '%s\\n' \"${history[@]}\""
            };
            let script = format!(
                "source \"$HOME/.config/aliaz/aliases.sh\"\naliaz() {{ __aliaz_last; printf '%s' \"$ALIAZ_LAST_COMMAND\" > \"$HOME/captured-command\"; {history_dump} > \"$HOME/entered-history\"; }}\n{entered_command}\naliaz capture\nexit 0\n"
            );
            let mut process = std::process::Command::new(shell);
            if is_bash {
                process.args(["--noprofile", "--norc", "-i"]);
            } else {
                process.args(["-f", "-i"]);
            }
            let mut child = process
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("TERM", "dumb")
                .env("HOME", home.path())
                .env("HISTSIZE", "1000")
                .env("HISTFILESIZE", "1000")
                .env("HISTFILE", home.path().join(".private_history"))
                .env("ALIAZ_PICKER_KEY", "off")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let history = fs::read(home.path().join("entered-history")).unwrap();
            assert!(!history.is_empty(), "{shell}: entered history is empty");
            assert!(
                String::from_utf8_lossy(&history).contains(command),
                "{shell}: fixture command is missing from history: {}",
                String::from_utf8_lossy(&history)
            );
            assert_eq!(
                fs::read(home.path().join("captured-command")).unwrap(),
                command.as_bytes(),
                "{shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn bash_last_command_capture_keeps_exact_whitespace_quotes_pipeline_and_newlines() {
    let shells = if cfg!(target_os = "macos") {
        vec!["/bin/bash", "/opt/homebrew/bin/bash"]
    } else {
        vec!["bash"]
    };
    check_last_command_capture(&shells);
}

#[test]
fn zsh_last_command_capture_keeps_exact_whitespace_quotes_pipeline_and_newlines() {
    check_last_command_capture(&["zsh"]);
}

#[test]
fn bash_three_uses_review_copy_and_explicit_run_without_binding_an_insertion_shortcut() {
    let shell = "/bin/bash";
    let version = std::process::Command::new(shell)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "test ${BASH_VERSINFO[0]} -eq 3",
        ])
        .status();
    if !version.is_ok_and(|status| status.success()) {
        eprintln!("not checked: system Bash 3 is not installed");
        return;
    }
    let home = TempDir::new().unwrap();
    let marker = home.path().join("explicit-run");
    let command = format!("printf explicit > '{}'", marker.display());
    cmd(&home)
        .args(["add", "marker", &command])
        .assert()
        .success();
    cmd(&home).args(["init", "bash"]).assert().success();
    let output = std::process::Command::new(shell)
        .args([
            "--noprofile",
            "--norc",
            "-i",
            "-c",
            r#"
        source "$HOME/.config/aliaz/aliases.sh"
        test -z "${__aliaz_bound_key:-}" || exit 11
        READLINE_LINE='existing buffer'; READLINE_POINT=3
        __aliaz_pick >/dev/null
        test "$?" -eq 1 || exit 12
        test "$READLINE_LINE" = 'existing buffer' || exit 13
        test "$READLINE_POINT" -eq 3 || exit 14
        aliaz select --first marker
        test ! -f "$HOME/explicit-run" || exit 15
        aliaz select --run --first marker
        test -f "$HOME/explicit-run" || exit 16
    "#,
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home.path())
        .env("TERM", "dumb")
        .env("ALIAZ_DATA_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, format!("{command}\n").as_bytes());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Command insertion is unavailable in this Bash")
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "explicit");
}

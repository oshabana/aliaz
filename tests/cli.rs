use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn cmd(home: &TempDir) -> AssertCommand {
    let mut command = AssertCommand::cargo_bin("aliaz").expect("binary exists");
    command.env("HOME", home.path());
    command.env("ALIAZ_DATA_HOME", home.path());
    command.env("XDG_CONFIG_HOME", home.path().join(".config"));
    command.env_remove("ZDOTDIR");
    command
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR");
    command.env("ALIAZ_CONFIG_HOME", home.path().join(".config"));
    command.env("ALIAZ_SECRET_HOME", home.path().join(".secrets"));
    command
}

fn copied_binary(home: &TempDir) -> PathBuf {
    let source = assert_cmd::cargo::cargo_bin("aliaz");
    let target = home.path().join("aliaz-copy");
    fs::copy(&source, &target).expect("copy binary");

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&target).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&target, permissions).expect("set executable bit");
    }

    target
}

fn sync_server_for_login() -> (String, thread::JoinHandle<()>) {
    sync_server_for_login_with_pull(r#"{"latest_version":0,"records":[]}"#.to_owned())
}

fn sync_server_for_login_with_pull(pull_body: String) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind sync server");
    listener
        .set_nonblocking(true)
        .expect("set sync server nonblocking");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("sync server addr")
    );

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("accept sync request: {error}"),
            };

            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            let path = request_line.split_whitespace().nth(1).unwrap_or("/");

            let (status, body) = match path {
                "/v1/login" => (
                    "200 OK",
                    r#"{"user_id":"user-1","token":"token-1","latest_version":0}"#,
                ),
                "/v1/records?after=0" => ("200 OK", pull_body.as_str()),
                _ => ("404 Not Found", r#"{"error":"not found"}"#),
            };

            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write response");

            if path == "/v1/records?after=0" || path == "/checksums.txt" {
                return;
            }
        }
    });

    (base_url, handle)
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
fn aliases_default_to_shared_collection() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added gs to shared"));

    cmd(&home)
        .args(["collection", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shared\tactive"));

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"));

    cmd(&home)
        .args(["list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status\tshared\tactive"));
}

#[test]
fn existing_database_aliases_migrate_into_shared_collection() {
    let home = TempDir::new().expect("temp home");

    cmd(&home).args(["add", "ll", "ls -lah"]).assert().success();

    cmd(&home)
        .args(["list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ll\tls -lah\tshared\tactive"));
}

#[test]
fn inactive_collections_do_not_generate_until_activated() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["collection", "add", "mac"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created collection mac"));

    cmd(&home)
        .args(["add", "pbcopy-path", "pwd | pbcopy", "--collection", "mac"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added pbcopy-path to mac"));

    cmd(&home)
        .args(["generate", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    cmd(&home)
        .args(["collection", "activate", "mac"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Activated mac"));

    cmd(&home)
        .args(["generate", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alias pbcopy-path='pwd | pbcopy'"));
}

#[test]
fn active_collection_aliases_override_shared_aliases() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "lsdash", "ls --color=auto"])
        .assert()
        .success();
    cmd(&home)
        .args(["collection", "add", "mac"])
        .assert()
        .success();
    cmd(&home)
        .args(["add", "lsdash", "ls -G", "--collection", "mac"])
        .assert()
        .success();
    cmd(&home)
        .args(["collection", "activate", "mac"])
        .assert()
        .success();

    cmd(&home)
        .args(["generate", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alias lsdash='ls -G'"))
        .stdout(predicate::str::contains("ls --color=auto").not());
}

#[test]
fn edit_remove_and_move_accept_collection_scope() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["collection", "add", "dev"])
        .assert()
        .success();
    cmd(&home)
        .args([
            "add",
            "serve",
            "python -m http.server",
            "--collection",
            "dev",
        ])
        .assert()
        .success();

    cmd(&home)
        .args([
            "edit",
            "serve",
            "python3 -m http.server",
            "--collection",
            "dev",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated serve in dev"));

    cmd(&home)
        .args([
            "collection",
            "move",
            "serve",
            "--from",
            "dev",
            "--to",
            "shared",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Moved serve from dev to shared"));

    cmd(&home)
        .args(["list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "serve\tpython3 -m http.server\tshared\tactive",
        ));

    cmd(&home)
        .args(["rm", "serve", "--collection", "shared"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted serve from shared"));
}

#[test]
fn edit_requires_collection_when_alias_name_is_ambiguous() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["collection", "add", "mac"])
        .assert()
        .success();
    cmd(&home)
        .args(["add", "openit", "xdg-open"])
        .assert()
        .success();
    cmd(&home)
        .args(["add", "openit", "open", "--collection", "mac"])
        .assert()
        .success();

    cmd(&home)
        .args(["edit", "openit", "open -a Finder"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "alias is ambiguous; pass --collection",
        ));
}

#[test]
fn select_prints_matching_alias_command_for_shell_wrapper() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success();
    cmd(&home).args(["add", "ll", "ls -lah"]).assert().success();

    cmd(&home)
        .args(["select", "--print-command", "--first", "ll"])
        .assert()
        .success()
        .stdout(predicate::eq("ls -lah\n"));
}

#[test]
fn select_runs_matching_alias_command_without_shell_wrapper() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["add", "hi", "printf selected-ok"])
        .assert()
        .success();

    cmd(&home)
        .env("SHELL", "/bin/bash")
        .args(["select", "--run", "--first", "hi"])
        .assert()
        .success()
        .stdout(predicate::eq("selected-ok"));
}

#[test]
fn bash_wrapper_executes_selected_alias_in_current_shell() {
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
            aliaz add hi "printf wrapper-ok" >/dev/null
            aliaz select --run --first hi
            "#,
        )
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "bash failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "wrapper-ok");
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
fn migrate_accepts_help_as_a_nested_command() {
    let home = TempDir::new().expect("temp home");

    cmd(&home)
        .args(["migrate", "help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: aliaz migrate"));
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
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
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
  "username": "testuser",
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
        .stdout(predicate::str::contains("Logged out testuser"));

    assert!(!config_dir.join("config.json").exists());
    assert!(!secret_dir.join("user-1").exists());
}

#[test]
fn uninstall_removes_shell_integration_and_keeps_data() {
    let home = TempDir::new().expect("temp home");
    let config_dir = home.path().join(".config/aliaz");
    let aliases_dir = home.path().join(".config/aliaz");
    let zshrc = home.path().join(".zshrc");
    let binary = copied_binary(&home);

    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("config.json"),
        r#"{
  "sync_url": "https://sync.example",
  "username": "testuser",
  "user_id": "user-1",
  "token": "token-1",
  "latest_version": 7
}
"#,
    )
    .expect("config");

    let add_output = ProcessCommand::new(&binary)
        .args(["add", "gs", "git status"])
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .output()
        .expect("add alias");
    assert!(add_output.status.success(), "add failed");

    let init_output = ProcessCommand::new(&binary)
        .args(["init", "zsh"])
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .output()
        .expect("init zsh");
    assert!(init_output.status.success(), "init failed");

    assert!(aliases_dir.join("aliases.sh").exists());
    assert!(zshrc.exists());

    let uninstall_output = ProcessCommand::new(&binary)
        .args(["uninstall"])
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .output()
        .expect("uninstall");
    assert!(uninstall_output.status.success(), "uninstall failed");

    assert!(!binary.exists());
    assert!(!aliases_dir.join("aliases.sh").exists());
    assert!(!zshrc.exists());
    assert!(config_dir.join("config.json").exists());

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"));
}

#[test]
fn key_prints_the_stored_recovery_phrase() {
    let home = TempDir::new().expect("temp home");
    let config_dir = home.path().join(".config/aliaz");
    let secret_dir = home.path().join(".secrets");
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        config_dir.join("config.json"),
        r#"{
  "sync_url": "https://sync.example",
  "username": "testuser",
  "user_id": "user-1",
  "token": "token-1",
  "latest_version": 7
}
"#,
    )
    .expect("config");
    fs::write(secret_dir.join("user-1"), "recovery phrase").expect("secret");

    cmd(&home)
        .args(["key"])
        .assert()
        .success()
        .stdout(predicate::eq("recovery phrase\n"));
}

#[test]
fn login_uses_file_secret_home_when_configured() {
    let home = TempDir::new().expect("temp home");
    let secret_dir = home.path().join(".aliaz-secrets");
    let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let (sync_url, server) = sync_server_for_login();

    let spaced_phrase = format!("{}  ", phrase.replace(' ', "  "));
    cmd(&home).args(["init", "bash"]).assert().success();
    let integration = home.path().join(".config/aliaz/aliases.sh");
    fs::write(&integration, "# stale integration\n").unwrap();
    let mut login = AssertCommand::cargo_bin("aliaz").expect("binary exists");
    login
        .env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", &secret_dir)
        .args([
            "login",
            "--username",
            "testuser",
            "--password",
            "Testuser123!",
            "--recovery-phrase",
            &spaced_phrase,
            "--collections",
            "shared",
            "--sync-url",
            &sync_url,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logged in testuser"));

    server.join().expect("sync server finished");

    assert!(
        !fs::read_to_string(&integration)
            .unwrap()
            .contains("# stale integration")
    );
    let secret_path = secret_dir.join("user-1");
    assert_eq!(fs::read_to_string(&secret_path).expect("secret"), phrase);

    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&secret_path)
            .expect("secret metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let mut key = AssertCommand::cargo_bin("aliaz").expect("binary exists");
    key.env("HOME", home.path())
        .env("ALIAS_TOOL_HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", &secret_dir)
        .args(["key"])
        .assert()
        .success()
        .stdout(predicate::eq(format!("{phrase}\n")));
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
fn export_and_import_preserve_collections() {
    let source = TempDir::new().expect("temp home");
    let export_path = source.path().join("aliases.json");

    cmd(&source)
        .args(["collection", "add", "dev"])
        .assert()
        .success();
    cmd(&source)
        .args([
            "add",
            "serve",
            "python3 -m http.server",
            "--collection",
            "dev",
        ])
        .assert()
        .success();
    cmd(&source)
        .args([
            "export",
            "--output",
            export_path.to_str().expect("utf8 path"),
        ])
        .assert()
        .success();

    let target = TempDir::new().expect("temp home");
    cmd(&target)
        .args(["import", export_path.to_str().expect("utf8 path")])
        .assert()
        .success();

    cmd(&target)
        .args(["list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "serve\tpython3 -m http.server\tdev\tinactive",
        ));
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
        .stdout(predicate::str::contains("collections: 1"))
        .stdout(predicate::str::contains("active collections: shared"))
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

#[test]
fn doctor_fix_repairs_missing_shell_integration() {
    let home = TempDir::new().expect("temp home");
    let aliases_path = home.path().join(".config/aliaz/aliases.sh");
    let zshrc = home.path().join(".zshrc");

    cmd(&home)
        .args(["add", "gs", "git status"])
        .assert()
        .success();

    cmd(&home)
        .env("SHELL", "/bin/zsh")
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("repaired zsh integration"));

    assert!(aliases_path.exists());
    assert!(zshrc.exists());
    assert!(
        fs::read_to_string(&aliases_path)
            .expect("aliases")
            .contains("alias gs='git status'")
    );
    assert!(
        fs::read_to_string(&zshrc)
            .expect("zshrc")
            .contains(r#"source "$HOME/.config/aliaz/aliases.sh""#)
    );

    cmd(&home)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gs\tgit status"));
}

fn shell_refresh_removes_owned_aliases(shell: &str) {
    if ProcessCommand::new(shell)
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("not checked: {shell} is not installed");
        return;
    }
    let home = TempDir::new().unwrap();
    for args in [
        vec!["add", "gs", "printf shared"],
        vec!["collection", "add", "work"],
        vec!["add", "gs", "printf work", "--collection", "work"],
        vec!["add", "gone", "printf stale", "--collection", "work"],
        vec![
            "add",
            "overridden",
            "printf managed",
            "--collection",
            "work",
        ],
        vec!["collection", "activate", "work"],
        vec!["init", shell],
    ] {
        cmd(&home).args(args).assert().success();
    }
    let (path, script) = if shell == "fish" {
        (
            home.path().join(".config/fish/conf.d/aliaz.fish"),
            r#"
source "$ALIAZ_INTEGRATION"
alias unrelated 'printf own'
alias overridden 'printf custom'
aliaz collection deactivate work >/dev/null; or exit 1
functions -q gone; and exit 2
eval gs
source "$ALIAZ_INTEGRATION"
eval overridden
eval unrelated
aliaz rm gs --collection shared >/dev/null; or exit 3
functions -q gs; and exit 4
exit 0
"#,
        )
    } else {
        (
            home.path().join(".config/aliaz/aliases.sh"),
            r#"
set -e
if [ -n "${BASH_VERSION:-}" ]; then shopt -s expand_aliases; fi
source "$ALIAZ_INTEGRATION"
alias unrelated='printf own'
alias overridden='printf custom'
aliaz collection deactivate work >/dev/null
if alias gone >/dev/null 2>&1; then exit 2; fi
eval gs
source "$ALIAZ_INTEGRATION"
eval overridden
eval unrelated
aliaz rm gs --collection shared >/dev/null
if alias gs >/dev/null 2>&1; then exit 4; fi
"#,
        )
    };
    let mut process = ProcessCommand::new(shell);
    match shell {
        "bash" => {
            process.args(["--noprofile", "--norc", "-c"]);
        }
        "zsh" => {
            process.args(["-f", "-c"]);
        }
        _ => {
            process.args(["--no-config", "-c"]);
        }
    }
    let output = process
        .arg(script)
        .env("HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .env_remove("ZDOTDIR")
        .env("ALIAZ_INTEGRATION", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{shell} refresh failed ({}): stdout={:?}, stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "sharedcustomown");
}

#[test]
fn bash_refresh_removes_deleted_and_deactivated_aliases_preserving_user_overrides() {
    shell_refresh_removes_owned_aliases("bash");
}
#[test]
fn zsh_refresh_removes_deleted_and_deactivated_aliases_preserving_user_overrides() {
    shell_refresh_removes_owned_aliases("zsh");
}
#[test]
fn fish_refresh_removes_deleted_and_deactivated_aliases_preserving_user_overrides() {
    shell_refresh_removes_owned_aliases("fish");
}

#[test]
fn init_repairs_commented_source_and_respects_zdotdir() {
    let home = TempDir::new().unwrap();
    let zdir = home.path().join("zsh-config");
    fs::create_dir_all(&zdir).unwrap();
    fs::write(
        zdir.join(".zshrc"),
        "# source \"$HOME/.config/aliaz/aliases.sh\"\n",
    )
    .unwrap();
    cmd(&home)
        .args(["init", "zsh"])
        .env("ZDOTDIR", &zdir)
        .assert()
        .success();
    let contents = fs::read_to_string(zdir.join(".zshrc")).unwrap();
    assert!(
        contents
            .lines()
            .any(|line| line == "source \"$HOME/.config/aliaz/aliases.sh\"")
    );
    assert!(!home.path().join(".zshrc").exists());
}

#[test]
fn bash_init_configures_existing_login_profile_and_uninstall_preserves_its_contents() {
    let home = TempDir::new().unwrap();
    fs::write(
        home.path().join(".profile"),
        "export MY_LOGIN_SETTING=kept\n",
    )
    .unwrap();
    cmd(&home).args(["init", "bash"]).assert().success();
    assert!(!home.path().join(".bash_profile").exists());
    let profile = fs::read_to_string(home.path().join(".profile")).unwrap();
    assert!(profile.contains("source \"$HOME/.config/aliaz/aliases.sh\""));
    assert!(!profile.contains("source .bashrc"));
    assert!(profile.contains("if [ -n \"${BASH_VERSION:-}\" ]; then source"));
    let posix = ProcessCommand::new("sh")
        .args([
            "-c",
            ". \"$HOME/.profile\"; test \"$MY_LOGIN_SETTING\" = kept",
        ])
        .env("HOME", home.path())
        .env_remove("BASH_VERSION")
        .output()
        .unwrap();
    assert!(
        posix.status.success(),
        "POSIX login failed: {}",
        String::from_utf8_lossy(&posix.stderr)
    );
    assert!(posix.stderr.is_empty());
    let binary = copied_binary(&home);
    let output = ProcessCommand::new(&binary)
        .arg("uninstall")
        .env("HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .env_remove("ZDOTDIR")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(home.path().join(".profile")).unwrap(),
        "export MY_LOGIN_SETTING=kept\n"
    );
}

#[test]
fn failed_login_decryption_preserves_previous_phrase_and_config() {
    let home = TempDir::new().unwrap();
    let config_path = home.path().join(".config/aliaz/config.json");
    let secret_path = home.path().join(".secrets/user-1");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::create_dir_all(secret_path.parent().unwrap()).unwrap();
    let original_config = r#"{"sync_url":"https://previous.invalid","username":"testuser","user_id":"user-1","token":"previous-fixture","latest_version":9}"#;
    let original_phrase =
        "legal winner thank year wave sausage worth useful legal winner thank yellow";
    fs::write(&config_path, original_config).unwrap();
    fs::write(&secret_path, original_phrase).unwrap();
    let (url, server) = sync_server_for_login_with_pull(r#"{"latest_version":1,"records":[{"id":"remote-alias","record_type":"alias","encrypted_blob":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","version":1}]}"#.to_owned());
    cmd(&home).args([
        "login", "--username", "testuser", "--password", "Testuser123!",
        "--recovery-phrase", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        "--collections", "shared", "--sync-url", &url,
    ]).assert().failure().stderr(predicate::str::contains("failed to decrypt record"));
    server.join().unwrap();
    assert_eq!(fs::read_to_string(config_path).unwrap(), original_config);
    assert_eq!(fs::read_to_string(secret_path).unwrap(), original_phrase);
}

#[test]
fn recovery_commands_do_not_open_a_database_with_a_newer_schema() {
    let home = TempDir::new().unwrap();
    let database = home.path().join("aliases.sqlite3");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("PRAGMA user_version = 999; CREATE TABLE future_aliases (value TEXT); INSERT INTO future_aliases VALUES ('retained');").unwrap();
    drop(conn);
    let original = fs::read(&database).unwrap();
    let (url, server) = sync_server_for_login();
    cmd(&home)
        .arg("update")
        .env("ALIAZ_RELEASE_BASE_URL", url)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to download"))
        .stderr(predicate::str::contains("newer aliaz").not());
    server.join().unwrap();
    assert_eq!(fs::read(&database).unwrap(), original);
    let binary = copied_binary(&home);
    let output = ProcessCommand::new(&binary)
        .arg("uninstall")
        .env("HOME", home.path())
        .env("ALIAZ_DATA_HOME", home.path())
        .env("ALIAZ_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("ALIAZ_SECRET_HOME", home.path().join(".secrets"))
        .env_remove("ZDOTDIR")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(fs::read(database).unwrap(), original);
}

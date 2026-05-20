use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn installer_prompt_functions() -> String {
    let installer = fs::read_to_string(format!("{}/install.sh", env!("CARGO_MANIFEST_DIR")))
        .expect("read install.sh");
    let start = installer.find("say() {").expect("find say function");
    let end = installer
        .find("\ndownload() {")
        .expect("find download function");

    installer[start..end]
        .replace("> /dev/tty", ">> \"$ALIAZ_TEST_TTY_OUT\"")
        .replace("< /dev/tty", "< \"$ALIAZ_TEST_TTY_IN\"")
}

#[test]
fn sync_menu_prompts_are_not_captured_as_the_selected_mode() {
    let temp = TempDir::new().expect("tempdir");
    let tty_in = temp.path().join("tty.in");
    let tty_out = temp.path().join("tty.out");
    let script_path = temp.path().join("menu-choice-test.sh");

    fs::write(&tty_in, "\n").expect("write tty input");
    fs::write(&tty_out, "").expect("write tty output");

    let script = format!(
        r#"set -eu
ALIAZ_TEST_TTY_IN="$1"
ALIAZ_TEST_TTY_OUT="$2"
export ALIAZ_TEST_TTY_IN ALIAZ_TEST_TTY_OUT

{functions}

choice="$(menu_choice "aliaz install: set up encrypted sync" 1 skip register login)"
printf 'choice=<%s>\n' "$choice"
printf 'tty=<'
cat "$ALIAZ_TEST_TTY_OUT"
printf '>\n'
"#,
        functions = installer_prompt_functions()
    );
    fs::write(&script_path, script).expect("write test script");

    let output = Command::new("sh")
        .arg(&script_path)
        .arg(&tty_in)
        .arg(&tty_out)
        .output()
        .expect("run shell test");

    assert!(
        output.status.success(),
        "shell test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(
        stdout.starts_with("choice=<skip>\n"),
        "menu text leaked into captured choice:\n{}",
        stdout
    );
    assert!(
        stdout.contains("tty=<aliaz install: set up encrypted sync\n  1) skip\n  2) register\n  3) login\naliaz install: choice [1]: >"),
        "menu prompt was not written to tty output:\n{}",
        stdout
    );
}

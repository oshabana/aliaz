# Aliaz

Aliaz is a CLI for managing shell aliases from a local SQLite-backed source of
truth. Add aliases once, generate shell-safe alias files for zsh, bash, or fish,
and optionally sync encrypted aliases across machines.

## Installation

Install the latest Aliaz release:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | sh
```

The installer downloads the matching release binary for your platform and
installs it to `~/.local/bin`. During interactive installs, it can configure
zsh, bash, fish, or multiple shells. For zsh and bash, it updates the startup
file once; for fish, it writes the managed `conf.d` file.

Configure shells non-interactively:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_INSTALL_SHELLS="zsh bash" sh
```

Skip shell setup:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_INSTALL_SHELLS=skip sh
```

Install a specific version:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_VERSION=v0.1.1 sh
```

Install to a different directory:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_INSTALL_DIR=/usr/local/bin sh
```

Confirm the binary is available:

```sh
aliaz --help
```

The installer can also start sync setup:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_INSTALL_SYNC=login ALIAZ_SYNC_USERNAME=ada sh
```

To build from source, install Rust first if `cargo` is not already available:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install Aliaz from this repository:

```sh
cargo install --path .
```

For local development, build and run without installing:

```sh
cargo run -- --help
cargo run -- add gs "git status"
```

## Quick Start

Add an alias:

```sh
aliaz add gs "git status"
gs
```

List aliases:

```sh
aliaz list
```

Generate aliases for your shell:

```sh
aliaz generate zsh
aliaz generate bash
aliaz generate fish
```

If you skipped shell setup during installation, install it later:

```sh
aliaz init zsh
```

After shell integration is active, mutating commands such as `add`, `edit`,
`rm`, `migrate`, `import`, and `sync` refresh aliases automatically in the
current shell.

## Commands

### Add an alias

```sh
aliaz add <name> <command>
```

Example:

```sh
aliaz add ll "ls -lah"
```

Alias names may contain ASCII letters, numbers, `_`, `-`, and `.`.

### List aliases

```sh
aliaz list
```

Output is tab-separated:

```text
gs	git status
ll	ls -lah
```

### Edit an alias

```sh
aliaz edit <name> <command>
```

Example:

```sh
aliaz edit gs "git status --short"
```

### Delete an alias

```sh
aliaz rm <name>
```

`delete` is also accepted as an alias for `rm`:

```sh
aliaz delete gs
```

### Generate shell aliases

```sh
aliaz generate <shell>
```

Supported shells:

```text
zsh
bash
fish
```

`generate` prints alias definitions to stdout. It does not write files.

### Install shell integration

```sh
aliaz init <shell>
```

For zsh and bash, `init` writes the managed alias file and adds the startup
`source` line once. For fish, `init` writes the managed file used by fish
automatically.

Restart the shell or open a new session after running it manually. After that,
Aliaz refreshes aliases automatically when you change them.

## Migrating Existing Aliases

Import aliases from a zsh-style alias file:

```sh
aliaz migrate --from ~/.zshrc
```

If `--from` is omitted, Aliaz reads from your default `.zshrc`:

```sh
aliaz migrate
```

Only lines that start with `alias ` are imported. Commented aliases and shell
functions are ignored.

## Import and Export

Export aliases as JSON:

```sh
aliaz export --output aliases.json
```

Without `--output`, the JSON is printed to stdout:

```sh
aliaz export
```

Import an exported file:

```sh
aliaz import aliases.json
```

Imports upsert aliases by name, so existing aliases with the same name are
updated.

## Sync

Sync is optional. Local alias management works without an account.

Register a new sync account:

```sh
aliaz register --username ada
```

Aliaz prompts for a password, creates a recovery phrase, stores the recovery
phrase in the OS credential store, and prints the phrase once. Save it somewhere
safe. Aliaz cannot recover encrypted aliases without it.

Log in on another machine:

```sh
aliaz login --username ada
```

Aliaz prompts for the password and recovery phrase.

Run sync:

```sh
aliaz sync
```

Log out of local sync state:

```sh
aliaz logout
```

For the privacy and threat model behind sync, see
[Security and Privacy Model](docs/security.md).

Use a custom sync server:

```sh
aliaz register --username ada --sync-url https://sync.example.com
aliaz login --username ada --sync-url https://sync.example.com
```

For non-interactive setup, pass secrets as options:

```sh
aliaz register --username ada --password "$ALIAZ_PASSWORD"
aliaz login --username ada --password "$ALIAZ_PASSWORD" --recovery-phrase "$ALIAZ_RECOVERY_PHRASE"
```

Prefer interactive prompts on shared machines so secrets are not saved in shell
history.

## Status and Doctor

Check local state:

```sh
aliaz status
```

This reports the number of active aliases, pending sync records, and sync
configuration.

Check local setup:

```sh
aliaz doctor
```

This verifies the database, shell integration files, sync configuration, and
secret storage.

## Storage

Aliaz stores aliases in a local SQLite database under the operating system's
standard data directory. Sync configuration is stored under the standard config
directory. Recovery phrases are stored in the OS credential store.

For tests and isolated runs, these environment variables override storage:

```sh
ALIAZ_DATA_HOME=/tmp/aliaz-data
ALIAZ_CONFIG_HOME=/tmp/aliaz-config
```

`ALIAS_TOOL_HOME` is also supported as a legacy data directory override.

## Troubleshooting

If an alias does not appear in your shell, run:

```sh
aliaz list
aliaz init zsh
aliaz doctor
```

Replace `zsh` with your shell. For zsh and bash, confirm the startup file has
the Aliaz shell integration block.

If sync fails because it is not configured, run `aliaz register` for a new
account or `aliaz login` for an existing one.

If sync reports a missing recovery phrase, log in again with the saved recovery
phrase.

## Development

Run tests:

```sh
cargo test
```

Run the CLI from source:

```sh
cargo run -- <command>
```

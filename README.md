<div align="center">
  <a href="https://aliaz.dev">
    <img src="site/assets/aliaz-icon-180.png" alt="Aliaz" width="88" height="88">
  </a>
  <h1>Aliaz</h1>
  <p>
    <strong>Open-source alias management for the CLI.</strong>
  </p>
  <p>
    Local-first shell aliases, generated safely for zsh, bash, and fish, with
    optional encrypted sync between machines.
  </p>
  <p>
    <a href="https://aliaz.dev"><strong>Official website</strong></a>
    ·
    <a href="https://aliaz.dev/install/">Install</a>
    ·
    <a href="https://aliaz.dev/docs/">Docs</a>
    ·
    <a href="https://aliaz.dev/sync/">Sync</a>
  </p>
  <p>
    <a href="https://github.com/oshabana/aliaz/actions/workflows/release.yml"><img alt="Release" src="https://img.shields.io/github/actions/workflow/status/oshabana/aliaz/release.yml?label=release"></a>
    <a href="https://github.com/oshabana/aliaz/blob/main/LICENSE"><img alt="License" src="https://img.shields.io/github/license/oshabana/aliaz"></a>
    <a href="https://aliaz.dev"><img alt="Website" src="https://img.shields.io/badge/website-aliaz.dev-2563eb"></a>
  </p>
</div>

---

```sh
$ aliaz add gs "git status"
Added gs to shared

$ aliaz init zsh
Wrote ~/.config/aliaz/aliases.sh
Configured ~/.zshrc

$ aliaz sync
Synced encrypted aliases
```

## What Aliaz Does

| Need | How Aliaz handles it |
| --- | --- |
| One alias source | Stores aliases in a local SQLite-backed source of truth. |
| Shell-safe output | Generates alias files for zsh, bash, and fish. |
| Existing aliases | Imports zsh-style alias files into the local database. |
| Recovery across machines | Optionally syncs encrypted alias records after account setup. |
| Inspectable state | Reports local setup, sync state, and shell integration health. |

## Installation

Install the latest Aliaz release:

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | sh
```

The installer downloads the matching release binary for your platform and
installs it to `~/.local/bin`. During interactive installs, it can configure
zsh, bash, fish, or multiple shells through numbered menus. Sync setup also
uses a menu instead of typed `login` / `register` prompts. For zsh and bash, it
updates the startup file once; for fish, it writes the managed `conf.d` file.

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
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_VERSION=v0.2.0 sh
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

The demo username `testuser` is not a provisioned account on the hosted service.

```sh
curl -fsSL https://raw.githubusercontent.com/oshabana/aliaz/main/install.sh | ALIAZ_INSTALL_SYNC=login ALIAZ_SYNC_USERNAME=testuser sh
```

Keep an installed binary current:

```sh
aliaz update
```

`update` downloads the latest release for the current platform and replaces the
installed binary in place.

Remove Aliaz from the current machine:

```sh
aliaz uninstall
```

`uninstall` removes the shell integration and the installed binary, while
keeping your aliases database and sync settings.

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

## Find and reuse a command

Save a command without quoting it for the shell:

```sh
aliaz save
```

Aliaz opens your editor, then asks for a name, an optional description, tags,
and a collection. Quotes, pipelines, and multiple lines stay in the command.
Set `VISUAL` or `EDITOR` to choose your editor. To save your previous command
without typing it again, enable shell integration and run `aliaz save --last`.
It checks the last 50 available history entries and skips Aliaz management
commands. It does not enable or change shell history recording.

Install integration for the shell you use, then open a new shell:

```sh
aliaz init zsh
# Or: aliaz init bash / aliaz init fish
```

Press **Ctrl-X, then Ctrl-A** to find a command. Search by name, command,
description, tags, or collection. Enter opens the full command for review. Confirm to place it
in your shell's editable command line. It runs only when you press Enter on
that line. Escape cancels and keeps the line and cursor you had before.
Favorites appear first, followed by recently selected commands.

Bash 3.2 supports aliases, review, copying, and explicit running. Editable-line
insertion needs newer Bash; on Bash 3.2 Aliaz explains the copy fallback and
leaves the insertion shortcut unbound.

Typing `aliaz` or `aliaz select` also opens the picker. With zsh integration,
it queues the selection at the next prompt. With fish integration, it replaces
the editable command line. In bash, use Ctrl-X then Ctrl-A for insertion; typing `aliaz`
prints the selected command for copying. Without integration, the picker also
prints the command. `aliaz` without a terminal prints help.

To run a selection immediately, choose that action explicitly:

```sh
aliaz select --run
```

With integration, this runs in the current shell, so `cd` and environment
changes persist. Without integration, it runs in a child shell.

### Choose a keyboard shortcut

Aliaz leaves Ctrl-R and Up unchanged. It uses Ctrl-X then Ctrl-A only when that
sequence is unbound. If the key is already assigned, Aliaz explains that and keeps the
existing binding. Choose an unused key before loading integration:

```sh
# bash or zsh: Alt-X
export ALIAZ_PICKER_KEY='\ex'

# fish: Alt-X
set -gx ALIAZ_PICKER_KEY \ex
```

Use `ALIAZ_PICKER_KEY=off` to disable automatic binding (`set -gx
ALIAZ_PICKER_KEY off` in fish). Open a new shell after changing this setting.
Existing bindings are never replaced automatically. Terminal and desktop
shortcuts may also reserve a key; choose another sequence if the default does
not reach your shell.

### Save from a script

Existing CLI commands remain available:

```sh
aliaz add gs 'git status' --description 'Check the working tree' --tags git,status
aliaz list
aliaz select --first --print-command 'working tree'
```

`list` keeps its tab-separated name and command output. `select --first
--print-command` prints the selected command followed by one newline, and never
runs it. `select --first` now reviews or prints a command; add `--run` to scripts
that intentionally executed a selection before this release.

## Commands

### Add an alias

```sh
aliaz add <name> <command> [--collection <collection>] [--description <text>] [--tags <tags>] [--favorite] [--replace]
```

Example:

```sh
aliaz add ll "ls -lah"
```

Alias names may contain ASCII letters, numbers, `_`, `-`, and `.`, but cannot
start with `-`. The name `aliaz` and the prefix `__aliaz_` are reserved for shell integration.

### List aliases

```sh
aliaz list [--all] [--collection <collection>]
```

Default output is tab-separated effective aliases from active collections:

```text
gs	git status
ll	ls -lah
```

Use `--all` to include collection and active state columns.

### Find a saved command

```sh
aliaz select [search text]
aliaz select --run [search text]
```

The default action reviews the selection. Use the shell shortcut to insert it
for editing. `--run` runs the selection explicitly. Only effective aliases from
active collections appear, so a higher-priority collection's command wins when
names overlap. `list --all` still shows the underlying collections.

### Add purpose and favorites

```sh
aliaz describe gs --description 'Check the working tree' --tags git,status
aliaz describe gs
aliaz favorite gs
aliaz favorite gs --off
aliaz list --details
```

`describe` without changes shows the command and its details. An empty
`--description ''` or `--tags ''` clears that field. Use `--collection` when a
name exists in more than one collection. Favorites sync with command details
when the sync service supports them.
Until that support is installed, details stay saved locally. Recent selections
stay on this computer. Canceled selections do not change recent ordering.

Adding a different command under an existing name requires `add --replace`.
Aliaz shows the old and new command. `edit` is also an explicit replacement and
shows both commands before saving.

### Edit an alias

```sh
aliaz edit <name> <command> [--collection <collection>]
```

Example:

```sh
aliaz edit gs "git status --short"
```

### Delete an alias

```sh
aliaz rm <name> [--collection <collection>]
```

`delete` is also accepted as an alias for `rm`:

```sh
aliaz delete gs
```

### Collections

Aliases live in collections. The `shared` collection is created automatically,
is always active, and is the default for `add`, `edit`, `rm`, and `list`.
Collection activation is local to each computer; sync shares collection
definitions and aliases, but not which collections are active on a device.

```sh
aliaz collection add mac
aliaz add pbcopy-path "pwd | pbcopy" --collection mac
aliaz collection activate mac
aliaz collection list
aliaz list --all
```

If multiple active collections contain the same alias name, the later active
collection wins. `shared` has the lowest priority, and `collection activate`
appends collections after it.

```sh
aliaz collection deactivate mac
aliaz collection move pbcopy-path --from mac --to shared
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
functions are ignored. The source file is never changed.

Preview additions, replacements, and skipped entries without applying them:

```sh
aliaz migrate --from ~/.zshrc --preview
aliaz migrate --from ~/.zshrc --preview --replace
```

Existing names are skipped unless `--replace` is supplied. Interactive imports
ask before applying the displayed preview. `--yes` applies without that prompt.
Noninteractive imports apply without prompting, keeping the script path usable;
use `--preview` for a dry run and `--replace` to permit replacements.

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

Preview first, then apply when the changes are right:

```sh
aliaz import aliases.json --preview --replace
aliaz import aliases.json --replace --yes
```

Imports match collection and name. Existing entries are skipped unless
`--replace` is supplied. Interactive imports confirm the preview unless `--yes`
is supplied; noninteractive imports apply without prompting. A failed import
leaves the command library unchanged.

Version 3 exports include descriptions, tags, and favorites. Version 2 exports
preserve collections. Version 1 exports default to `shared`. Imports accept all
three versions; older exports preserve any command details already saved here.
Older Aliaz releases cannot import version 3 exports. Recent selections and undo
history stay on this computer and are not included in an export.

## Recover a changed command

```sh
aliaz history
aliaz undo
aliaz undo 3
```

`undo` restores the most recent change. A number restores that specific change,
including its description, tags, favorite, and collection. A single import is
one change. Restoring a deletion keeps the command's identity. Undo leaves
unrelated commands alone and refuses to overwrite a newer change to the same
command. Inspect `history`, then undo that newer change first. Recovery changes
are included in the next sync; undo history itself stays local.

Inspect commands retained after sync conflicts:

```sh
aliaz conflicts
aliaz restore <backup-id>
```

New backups record the original command, collection, and details. Restoring
without destination options revives the original command identity. Supplying
`--collection` or `--name` copies the backup to the selected destination instead.
A replacement requires `--replace` and shows the old and new command. Older backups did not
record reliable collection identity or command details. For those, supply an
explicit destination with `--collection <collection> --name <name>`; Aliaz never
guesses from the displayed label. The backup remains available after restoring.

## Sync

Sync is optional. Local alias management works without an account.

The `testuser` examples use the standard development account name. Seeded
passwords apply only to the local instance described under Test accounts.

Register a new sync account:

```sh
aliaz register --username testuser
```

Omit `--username` to be prompted for it. Aliaz prompts for a password, creates a
recovery phrase, stores the recovery phrase in the OS credential store, and
prints the phrase once. Save it somewhere safe. Aliaz cannot recover encrypted
aliases without it.

On headless Linux systems without an OS credential service, Aliaz falls back to
a private file-backed secret store under the Aliaz config directory. Set
`ALIAZ_SECRET_HOME` to choose that directory explicitly, and keep that
environment variable set for future sync commands if you use a custom path.

Log in on another machine:

```sh
aliaz login --username testuser
```

Aliaz prompts for the password and recovery phrase (and the username if you omit
`--username`), pulls collections, and keeps only `shared` active unless you
choose more collections interactively or pass them non-interactively:

```sh
aliaz login --username testuser --collections mac,development
```

Run sync:

```sh
aliaz sync
```

Print the stored recovery phrase for the signed-in sync account:

```sh
aliaz key
```

Log out of local sync state:

```sh
aliaz logout
```

For the privacy and threat model behind sync, see
[Security and Privacy Model](docs/security.md).

Use a custom sync server:

```sh
aliaz register --username testuser --sync-url https://sync.example.com
aliaz login --username testuser --sync-url https://sync.example.com
```

For non-interactive setup, pass secrets as options:

```sh
aliaz register --username testuser --password "$ALIAZ_PASSWORD" --collections mac
aliaz login --username testuser --password "$ALIAZ_PASSWORD" --recovery-phrase "$ALIAZ_RECOVERY_PHRASE" --collections mac
```

Prefer interactive prompts on shared machines so secrets are not saved in shell
history.

## Status and Doctor

Check local state:

```sh
aliaz status
```

This reports the number of effective active aliases, collections, active
collections, pending sync records, and sync configuration.

Check local setup:

```sh
aliaz doctor
```

This verifies the database, shell integration files, sync configuration, and
secret storage.

If shell integration is missing or broken, run:

```sh
aliaz doctor --fix
```

`--fix` refreshes the managed shell files and startup hooks without touching
your aliases or sync data.

## Storage

Aliaz stores aliases, collections, and each device's active collection
selection in a local SQLite database under the operating system's standard data
directory. Sync configuration is stored under the standard config directory.
Recovery phrases are stored in the OS credential store when available. If that
store is unavailable, Aliaz uses a private file-backed secret store under the
standard config directory.

For tests and isolated runs, these environment variables override storage:

```sh
ALIAZ_DATA_HOME=/tmp/aliaz-data
ALIAZ_CONFIG_HOME=/tmp/aliaz-config
ALIAZ_SECRET_HOME=/tmp/aliaz-secrets
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

If secret storage fails on headless Linux with a DBus or Secret Service error,
install a Secret Service provider or set `ALIAZ_SECRET_HOME` to a private
directory before running `aliaz login`.

## Development

### Test accounts

Aliaz logs in by username only, so development login names omit the email domain.
The dev seed provisions the default admin and test accounts in isolated local D1
storage. They are available only after the seed succeeds. Sync does not use email
verification or MFA. The hosted sync service is not seeded.

| Account | Username | Password |
| --- | --- | --- |
| admin | adminuser | Adminuser123! |
| test | testuser | Testuser123! |

The account labels do not grant different sync permissions.

Install the repository dependencies and apply the existing migrations to the
isolated local database, then seed it:

```sh
pnpm install
pnpm exec wrangler d1 migrations apply aliaz-sync --local --persist-to .wrangler/test-accounts
NODE_ENV=development CONFIRM_LOCAL_TEST_ACCOUNTS=aliaz pnpm seed:test-accounts
```

The seed always uses `--local` and `.wrangler/test-accounts`. It refuses
production mode and existing matching accounts that were not created by the
seed. Rerunning refreshes only the seeded passwords and preserves vault records.
It does not use the default `.wrangler/state` database or any remote database.

Serve the same isolated state through a local Worker on an allocated port:

```sh
ports run aliaz -- sh -c 'pnpm exec wrangler dev --local --persist-to .wrangler/test-accounts --port "$PORT"'
```

The login endpoint is `http://127.0.0.1:${PORT}/v1/login`. For CLI login, pass
that local origin explicitly with `--sync-url`; omitting it selects the hosted
service. Use isolated `ALIAZ_DATA_HOME`, `ALIAZ_CONFIG_HOME`, and
`ALIAZ_SECRET_HOME` directories. CLI login also needs a development recovery
phrase for encrypted aliases; the account seed does not provision recovery
phrases or encrypted alias data.

Run the Rust tests and sync Worker checks:

```sh
cargo test --all-targets
npm run test:worker
```

Worker checks require Node.js 24 or later. CI pins Node.js 24; local checks for
this release ran on Node.js 26. Final verification also checks Rust formatting
and lint warnings. `.verifyq.json` schedules all four checks together.

Run the CLI from source:

```sh
cargo run -- <command>
```

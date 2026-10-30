use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bip39::{Language, Mnemonic};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use dialoguer::{Confirm, Editor, FuzzySelect, Input, Select, theme::SimpleTheme};
use flate2::read::GzDecoder;
use hkdf::Hkdf;
use keyring::Entry;
use rand::RngCore;
use reqwest::blocking::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Cursor, IsTerminal, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tar::Archive;
use uuid::Uuid;

const DEFAULT_SYNC_URL: &str = "https://aliaz-sync.still-silence-6a39.workers.dev";
const KEYRING_SERVICE: &str = "dev.aliaz.cli";
const DEFAULT_COLLECTION_NAME: &str = "shared";
const DEFAULT_COLLECTION_ID: &str = "collection:shared";
const SCHEMA_VERSION: i64 = 3;

mod library;
mod shell_library;
use library::Metadata;

#[derive(Parser)]
#[command(name = "aliaz")]
#[command(about = "Find, save, and reuse your commands")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Add {
        name: String,
        command: String,
        #[arg(long, default_value = DEFAULT_COLLECTION_NAME)]
        collection: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long)]
        favorite: bool,
        /// Replace an existing command after reviewing it.
        #[arg(long)]
        replace: bool,
    },
    /// Save a command without shell quoting. Opens an editor for multiline commands.
    Save {
        #[arg(long, conflicts_with = "command")]
        last: bool,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Show or change a command's description and tags.
    Describe {
        name: String,
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    Favorite {
        name: String,
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        off: bool,
    },
    /// Inspect changes, then undo one by its number.
    History,
    Undo {
        id: Option<i64>,
    },
    /// Inspect saved conflict commands.
    Conflicts,
    Restore {
        id: String,
        /// Required for older backups whose collection was not recorded.
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        replace: bool,
    },
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        details: bool,
    },
    /// Find a command to review. Execution requires --run.
    Select {
        /// Print the selected command instead of running it.
        #[arg(long, hide = true)]
        print_command: bool,
        #[arg(long, hide = true, conflicts_with_all = ["run", "print_command"])]
        print0: bool,
        /// Run the selected command explicitly.
        #[arg(long, conflicts_with = "print_command")]
        run: bool,
        /// Pick the first matching alias without opening the interactive list.
        #[arg(long, hide = true)]
        first: bool,
        /// Initial search text.
        query: Vec<String>,
    },
    #[command(alias = "delete")]
    Rm {
        name: String,
        #[arg(long)]
        collection: Option<String>,
    },
    Edit {
        name: String,
        command: String,
        #[arg(long)]
        collection: Option<String>,
    },
    Collection {
        #[command(subcommand)]
        command: CollectionCommand,
    },
    Migrate {
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        preview: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        replace: bool,
    },
    Init {
        shell: Shell,
    },
    Generate {
        shell: Shell,
    },
    Update,
    Uninstall,
    Export {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Import {
        path: PathBuf,
        #[arg(long)]
        preview: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        replace: bool,
    },
    Register {
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        collections: Option<String>,
        #[arg(long, default_value = DEFAULT_SYNC_URL)]
        sync_url: String,
    },
    Login {
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        recovery_phrase: Option<String>,
        #[arg(long)]
        collections: Option<String>,
        #[arg(long, default_value = DEFAULT_SYNC_URL)]
        sync_url: String,
    },
    Key,
    Logout,
    Sync,
    Status,
    Doctor {
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Subcommand)]
enum CollectionCommand {
    Add {
        name: String,
    },
    List,
    Activate {
        names: Vec<String>,
    },
    Deactivate {
        name: String,
    },
    Move {
        alias: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
}

#[derive(Clone, ValueEnum)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Alias {
    id: String,
    collection_id: String,
    collection_name: String,
    name: String,
    command: String,
    #[serde(default)]
    metadata: Metadata,
    deleted: bool,
    dirty: bool,
    sync_version: i64,
    updated_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct Collection {
    id: String,
    name: String,
    deleted: bool,
    dirty: bool,
    sync_version: i64,
    updated_at: i64,
    active: bool,
    priority: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportFile {
    version: u8,
    #[serde(default)]
    collections: Vec<ExportCollection>,
    aliases: Vec<ExportAlias>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportCollection {
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportAlias {
    name: String,
    command: String,
    #[serde(default = "default_collection_name")]
    collection: String,
    #[serde(default, flatten)]
    metadata: Metadata,
}

#[derive(Debug, Serialize, Deserialize)]
struct SyncConfig {
    sync_url: String,
    username: String,
    user_id: String,
    token: String,
    latest_version: i64,
}

#[derive(Debug, Deserialize)]
struct StoredSyncConfig {
    sync_url: String,
    username: String,
    user_id: String,
    token: String,
    latest_version: i64,
    recovery_phrase: Option<String>,
}

#[derive(Debug, Serialize)]
struct AccountRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Deserialize)]
struct AccountResponse {
    user_id: String,
    token: String,
    latest_version: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct AliasPayload {
    #[serde(default = "default_collection_id")]
    collection_id: String,
    #[serde(default = "default_collection_name")]
    collection_name: String,
    name: String,
    command: String,
    deleted: bool,
    updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CollectionPayload {
    name: String,
    deleted: bool,
    updated_at: i64,
}

fn default_collection_id() -> String {
    DEFAULT_COLLECTION_ID.to_owned()
}

fn default_collection_name() -> String {
    DEFAULT_COLLECTION_NAME.to_owned()
}

#[derive(Debug, Deserialize)]
struct PullResponse {
    latest_version: i64,
    records: Vec<RemoteRecord>,
}

#[derive(Debug, Serialize)]
struct PushRequest<'a> {
    records: &'a [UploadRecord],
}

#[derive(Debug, Deserialize)]
struct PushResponse {
    records: Vec<PushedRecord>,
}

#[derive(Debug, Deserialize)]
struct PushedRecord {
    id: String,
    version: i64,
}

#[derive(Debug, Deserialize)]
struct RemoteRecord {
    id: String,
    record_type: String,
    encrypted_blob: String,
    version: i64,
}

#[derive(Debug, Serialize)]
struct UploadRecord {
    id: String,
    record_type: String,
    encrypted_blob: String,
    updated_at: i64,
}

fn main() -> Result<()> {
    if print_migrate_help_if_requested()? {
        return Ok(());
    }

    let mut cli = Cli::parse();
    if cli.command.is_none() {
        if !io::stderr().is_terminal() {
            Cli::command().print_help()?;
            println!();
            return Ok(());
        }
        cli.command = Some(Commands::Select {
            print_command: false,
            print0: false,
            run: false,
            first: false,
            query: vec![],
        });
    }
    let command = cli.command.expect("default command supplied");
    match &command {
        Commands::Update => {
            update_release_binary(
                &std::env::current_exe().context("failed to locate current executable")?,
            )?;
            println!("Updated to latest release");
        }
        Commands::Uninstall => {
            let binary_path =
                std::env::current_exe().context("failed to locate current executable")?;
            let removed = uninstall_shell_integration()?;
            let binary_removed = remove_binary_best_effort(&binary_path)?;

            if removed {
                println!("Removed shell integration");
            } else {
                println!("Shell integration was not installed");
            }

            if binary_removed {
                println!("Removed {}", binary_path.display());
            } else {
                println!("Could not remove {} automatically", binary_path.display());
                println!("Delete it manually after closing any shell using aliaz");
            }

            println!("Kept your aliases database and sync settings");
        }
        _ => {}
    }
    if matches!(command, Commands::Update | Commands::Uninstall) {
        return Ok(());
    }
    let db_path = database_path()?;
    let store = Store::open(db_path)?;

    let history = matches!(
        &command,
        Commands::Add { .. }
            | Commands::Save { .. }
            | Commands::Describe { .. }
            | Commands::Favorite { .. }
            | Commands::Edit { .. }
            | Commands::Rm { .. }
            | Commands::Import { .. }
            | Commands::Migrate { .. }
            | Commands::Restore { .. }
            | Commands::Collection {
                command: CollectionCommand::Move { .. }
            }
    );
    store.record_change(history, || run_command(&store, command))
}

fn run_command(store: &Store, command: Commands) -> Result<()> {
    match command {
        Commands::Add {
            name,
            command,
            collection,
            description,
            tags,
            favorite,
            replace,
        } => {
            store.check_replacement(&collection, &name, &command, replace)?;
            store.upsert_in_collection(&collection, &name, &command)?;
            if description.is_some() || tags.is_some() || favorite {
                store.change_metadata(
                    &collection,
                    &name,
                    description,
                    tags,
                    favorite.then_some(true),
                )?;
            }
            println!("Added {name} to {collection}. Undo: aliaz undo");
        }
        Commands::Save {
            last,
            command,
            name,
            collection,
        } => library::save(store, last, command, name, collection)?,
        Commands::Describe {
            name,
            collection,
            description,
            tags,
        } => {
            let scope = store.resolve_alias_scope(collection.as_deref(), &name)?;
            if description.is_none() && tags.is_none() {
                let alias = store
                    .find_alias_by_collection_and_name(&scope.id, &name)?
                    .unwrap();
                let metadata = store.metadata(&alias.id)?;
                println!(
                    "{} [{}]\n{}\nDescription: {}\nTags: {}\nFavorite: {}",
                    alias.name,
                    scope.name,
                    alias.command,
                    metadata.description,
                    metadata.tags.join(", "),
                    metadata.favorite
                );
            } else {
                store.change_metadata(&scope.name, &name, description, tags, None)?;
                println!("Updated {name}. Undo: aliaz undo");
            }
        }
        Commands::Favorite {
            name,
            collection,
            off,
        } => {
            let scope = store.resolve_alias_scope(collection.as_deref(), &name)?;
            store.change_metadata(&scope.name, &name, None, None, Some(!off))?;
            println!("Updated favorite for {name}. Undo: aliaz undo");
        }
        Commands::History => store.print_history()?,
        Commands::Undo { id } => store.undo(id)?,
        Commands::Conflicts => store.print_conflicts()?,
        Commands::Restore {
            id,
            collection,
            name,
            replace,
        } => store.restore_conflict(&id, collection.as_deref(), name.as_deref(), replace)?,
        Commands::List {
            all,
            collection,
            details,
        } => {
            if details {
                for alias in store.list_all(collection.as_deref())? {
                    let metadata = store.metadata(&alias.id)?;
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        alias.name,
                        alias.command,
                        alias.collection_name,
                        metadata.description,
                        metadata.tags.join(","),
                        if metadata.favorite { "favorite" } else { "" }
                    );
                }
                return Ok(());
            }
            if all {
                let active_collections = store
                    .collections()?
                    .into_iter()
                    .filter(|collection| collection.active)
                    .map(|collection| collection.name)
                    .collect::<HashSet<_>>();
                for alias in store.list_all(collection.as_deref())? {
                    let status = if active_collections.contains(&alias.collection_name) {
                        "active"
                    } else {
                        "inactive"
                    };
                    println!(
                        "{}\t{}\t{}\t{}",
                        alias.name, alias.command, alias.collection_name, status
                    );
                }
            } else if let Some(collection) = collection {
                for alias in store.list_all(Some(&collection))? {
                    println!("{}\t{}", alias.name, alias.command);
                }
            } else {
                for alias in store.list_effective()? {
                    println!("{}\t{}", alias.name, alias.command);
                }
            }
        }
        Commands::Select {
            print_command,
            print0,
            run,
            first,
            query,
        } => {
            select_alias(store, &query, print_command, print0, first, run)?;
        }
        Commands::Rm { name, collection } => {
            let collection_name = store.delete_alias(collection.as_deref(), &name)?;
            println!("Deleted {name} from {collection_name}. Undo: aliaz undo");
        }
        Commands::Edit {
            name,
            command,
            collection,
        } => {
            let scope = store.resolve_alias_scope(collection.as_deref(), &name)?;
            let old = store
                .find_alias_by_collection_and_name(&scope.id, &name)?
                .ok_or_else(|| anyhow!("alias not found: {name}"))?;
            eprintln!(
                "Replace {name} [{}]\nOld command:\n{}\nNew command:\n{command}",
                scope.name, old.command
            );
            let collection_name = store.update_alias(collection.as_deref(), &name, &command)?;
            println!("Updated {name} in {collection_name}. Undo: aliaz undo");
        }
        Commands::Collection { command } => match command {
            CollectionCommand::Add { name } => {
                store.create_collection(&name)?;
                println!("Created collection {name}");
            }
            CollectionCommand::List => {
                for collection in store.collections()? {
                    let status = if collection.active {
                        "active"
                    } else {
                        "inactive"
                    };
                    println!("{}\t{}", collection.name, status);
                }
            }
            CollectionCommand::Activate { names } => {
                if names.is_empty() {
                    bail!("at least one collection name is required");
                }
                let activated = store.activate_collections(&names)?;
                if activated.is_empty() {
                    println!("No collections changed");
                } else {
                    for name in activated {
                        println!("Activated {name}");
                    }
                }
            }
            CollectionCommand::Deactivate { name } => {
                if store.deactivate_collection(&name)? {
                    println!("Deactivated {name}");
                } else {
                    println!("{name} is already inactive");
                }
            }
            CollectionCommand::Move { alias, from, to } => {
                store.move_alias(&alias, &from, &to)?;
                println!("Moved {alias} from {from} to {to}");
            }
        },
        Commands::Migrate {
            from,
            preview,
            yes,
            replace,
        } => {
            let path = from.unwrap_or_else(default_zshrc_path);
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let aliases = parse_aliases(&contents)?;
            let export = ExportFile {
                version: 3,
                collections: vec![],
                aliases: aliases
                    .into_iter()
                    .map(|alias| ExportAlias {
                        name: alias.name,
                        command: alias.command,
                        collection: alias.collection_name,
                        metadata: Metadata::default(),
                    })
                    .collect(),
            };
            library::import_with_preview(store, &export, preview, yes, replace)?;
        }
        Commands::Init { shell } => {
            let aliases = store.list_effective()?;
            let path = write_shell_integration(&shell, &aliases)?;
            match shell {
                Shell::Zsh | Shell::Bash => {
                    let startup_path = configure_startup_file(&shell, &path)?;
                    println!("Wrote {}", path.display());
                    println!("Configured {}", startup_path.display());
                }
                Shell::Fish => {
                    println!("Wrote {}", path.display());
                }
            }
        }
        Commands::Generate { shell } => {
            for line in shell_alias_lines(&shell, &store.list_effective()?) {
                println!("{line}");
            }
        }
        Commands::Update | Commands::Uninstall => {
            unreachable!("handled before opening the database")
        }
        Commands::Export { output } => {
            let aliases = store.list_all(None)?;
            let collections = store.collections()?;
            let export = ExportFile {
                version: 3,
                collections: collections
                    .iter()
                    .map(|collection| ExportCollection {
                        name: collection.name.clone(),
                    })
                    .collect(),
                aliases: aliases
                    .iter()
                    .map(|alias| {
                        Ok(ExportAlias {
                            name: alias.name.clone(),
                            command: alias.command.clone(),
                            collection: alias.collection_name.clone(),
                            metadata: store.metadata(&alias.id)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            };
            let json = serde_json::to_string_pretty(&export)?;
            let count = export.aliases.len();
            if let Some(path) = output {
                fs::write(&path, format!("{json}\n"))
                    .with_context(|| format!("failed to write {}", path.display()))?;
                println!("Exported {count} aliases to {}", path.display());
            } else {
                println!("{json}");
            }
        }
        Commands::Import {
            path,
            preview,
            yes,
            replace,
        } => {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let export: ExportFile = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            if !matches!(export.version, 1..=3) {
                bail!("unsupported export version: {}", export.version);
            }
            library::import_with_preview(store, &export, preview, yes, replace)?;
        }
        Commands::Register {
            username,
            password,
            collections,
            sync_url,
        } => {
            let username = prompt_username_if_missing(username)?;
            let password = prompt_secret_if_missing(password, "Password: ")?;
            let phrase = Mnemonic::generate_in(Language::English, 24)?.to_string();
            let response = account_request(&sync_url, "register", &username, &password)?;
            store_recovery_phrase(&response.user_id, &phrase)?;
            save_config(&SyncConfig {
                sync_url,
                username: username.clone(),
                user_id: response.user_id.clone(),
                token: response.token,
                latest_version: response.latest_version,
            })?;
            let collections = parse_collection_csv(collections.as_deref())?;
            create_and_activate_collections(store, &collections)?;
            println!("Registered {username}");
            println!("Recovery phrase: {phrase}");
            println!(
                "Store this phrase safely. Aliaz cannot recover encrypted aliases without it."
            );
        }
        Commands::Login {
            username,
            password,
            recovery_phrase,
            collections,
            sync_url,
        } => {
            let username = prompt_username_if_missing(username)?;
            let password = prompt_secret_if_missing(password, "Password: ")?;
            let recovery_phrase = prompt_secret_if_missing(recovery_phrase, "Recovery phrase: ")?;
            let recovery_phrase =
                Mnemonic::parse_in_normalized(Language::English, &recovery_phrase)
                    .context("invalid recovery phrase")?
                    .to_string();
            let response = account_request(&sync_url, "login", &username, &password)?;
            let mut config = login_config(&sync_url, &username, response);
            let result = sync_aliases(store, &mut config, &recovery_phrase)?;
            store_recovery_phrase(&config.user_id, &recovery_phrase)?;
            save_config(&config)?;
            let collections = match collections {
                Some(value) => parse_collection_csv(Some(&value))?,
                None => prompt_for_collection_selection(store)?,
            };
            store.activate_collections(&collections)?;
            for shell in shells_to_repair()? {
                write_shell_integration(&shell, &store.list_effective()?)?;
            }
            println!("Logged in {username}");
            println!(
                "Synced: pulled {}, pushed {}, latest version {}",
                result.pulled, result.pushed, config.latest_version
            );
        }
        Commands::Key => {
            let config = load_config()?.ok_or_else(|| {
                anyhow!("sync is not configured; run aliaz register or aliaz login first")
            })?;
            let recovery_phrase = load_recovery_phrase(&config.user_id)?;
            println!("{recovery_phrase}");
        }
        Commands::Logout => {
            if let Some(config) = load_config()? {
                remove_recovery_phrase(&config.user_id)?;
                remove_config()?;
                println!("Logged out {}", config.username);
            } else {
                println!("Sync was not configured");
            }
        }
        Commands::Sync => {
            let mut config = load_config()?.ok_or_else(|| {
                anyhow!("sync is not configured; run aliaz register or aliaz login first")
            })?;
            let recovery_phrase = load_recovery_phrase(&config.user_id)?;
            let result = sync_aliases(store, &mut config, &recovery_phrase)?;
            save_config(&config)?;
            println!(
                "Synced: pulled {}, pushed {}, latest version {}",
                result.pulled, result.pushed, config.latest_version
            );
        }
        Commands::Status => {
            let status = store.status()?;
            println!("aliases: {}", status.aliases);
            println!("collections: {}", status.collections);
            println!(
                "active collections: {}",
                status.active_collections.join(", ")
            );
            println!("pending sync records: {}", status.pending);
            if let Some(config) = load_config()? {
                println!("sync: configured for {}", config.username);
                println!("sync url: {}", config.sync_url);
                println!("latest sync version: {}", config.latest_version);
            } else {
                println!("sync: not configured");
            }
        }
        Commands::Doctor { fix } => {
            if fix {
                let aliases = store.list_effective()?;
                for shell in shells_to_repair()? {
                    for message in repair_shell_integration(&shell, &aliases)? {
                        println!("{message}");
                    }
                }
            }
            println!("database: ok");
            report_integration_status()?;
            if let Some(config) = load_config()? {
                println!("sync config: ok");
                if recovery_phrase_available(&config.user_id) {
                    println!("secret storage: ok");
                } else {
                    println!("secret storage: missing");
                }
            } else {
                println!("sync config: missing");
            }
        }
    }
    Ok(())
}

fn print_migrate_help_if_requested() -> Result<bool> {
    let mut args = std::env::args_os();
    let _binary_name = args.next();
    match (args.next(), args.next(), args.next()) {
        (Some(command), Some(help), None) if command == "migrate" && help == "help" => {
            let mut cli = Cli::command();
            let migrate = cli
                .find_subcommand_mut("migrate")
                .expect("migrate subcommand exists");
            let help = format!("{}", migrate.render_help()).replacen(
                "Usage: migrate",
                "Usage: aliaz migrate",
                1,
            );
            println!("{help}");
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn database_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("ALIAZ_DATA_HOME") {
        return Ok(PathBuf::from(home).join("aliases.sqlite3"));
    }

    if let Some(home) = std::env::var_os("ALIAS_TOOL_HOME") {
        return Ok(PathBuf::from(home).join("aliases.sqlite3"));
    }

    let data_dir = dirs::data_dir().ok_or_else(|| anyhow!("could not locate data directory"))?;
    Ok(data_dir.join("aliaz").join("aliases.sqlite3"))
}

fn default_zshrc_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zshrc")
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("could not locate home directory"))
}

fn release_base_url() -> String {
    std::env::var("ALIAZ_RELEASE_BASE_URL")
        .unwrap_or_else(|_| "https://github.com/oshabana/aliaz/releases/latest/download".to_owned())
}

fn release_target() -> Result<String> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        (os, arch) => bail!("unsupported platform for update: {os} {arch}"),
    };
    Ok(target.to_owned())
}

fn download_text(url: &str) -> Result<String> {
    Ok(reqwest::blocking::get(url)?
        .error_for_status()
        .with_context(|| format!("failed to download {url}"))?
        .text()?)
}

fn download_bytes(url: &str) -> Result<Vec<u8>> {
    Ok(reqwest::blocking::get(url)?
        .error_for_status()
        .with_context(|| format!("failed to download {url}"))?
        .bytes()?
        .to_vec())
}

fn checksum_for_asset(checksums: &str, asset_name: &str) -> Result<String> {
    for line in checksums.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let checksum = parts.next();
        let asset = parts.next();
        if let (Some(checksum), Some(asset)) = (checksum, asset)
            && asset.trim_start_matches('*') == asset_name
        {
            return Ok(checksum.to_owned());
        }
    }

    bail!("checksum for {asset_name} was not found")
}

fn verify_checksum(bytes: &[u8], expected_checksum: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected_checksum {
        bail!("downloaded archive checksum did not match");
    }
    Ok(())
}

fn extract_release_binary(archive_bytes: &[u8], output_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let cursor = Cursor::new(archive_bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = Archive::new(gz);
    archive
        .unpack(output_dir)
        .with_context(|| format!("failed to unpack {}", output_dir.display()))?;

    let binary_path = output_dir.join("aliaz");
    if binary_path.exists() {
        Ok(binary_path)
    } else {
        bail!("release archive did not contain aliaz")
    }
}

fn install_updated_binary(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to install updated binary to {}",
            destination.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(destination, permissions)?;
    }

    Ok(())
}

fn update_release_binary(destination: &Path) -> Result<()> {
    let base_url = release_base_url();
    update_release_binary_from_base_url(destination, &base_url)
}

fn select_alias(
    store: &Store,
    query: &[String],
    print_command: bool,
    print0: bool,
    first: bool,
    run: bool,
) -> Result<()> {
    let aliases = store.picker_aliases()?;
    if aliases.is_empty() {
        bail!("No saved commands. Save one with aliaz save or aliaz add NAME COMMAND");
    }
    let query = query.join(" ");
    let selected_index = if first {
        Some(
            first_matching_alias(&aliases, &query)
                .ok_or_else(|| anyhow!("no aliases matched: {query}"))?,
        )
    } else {
        if !io::stderr().is_terminal() {
            bail!(
                "select requires an interactive terminal; use --first --print-command for scripts"
            );
        }
        interactively_select_alias(&aliases, &query)?
    };
    let Some(index) = selected_index else {
        return Ok(());
    };
    let alias = &aliases[index];
    store.record_use(&alias.id)?;
    if print0 {
        io::stdout().write_all(alias.command.as_bytes())?;
        io::stdout().write_all(&[0])?;
        return Ok(());
    }
    if print_command {
        println!("{}", alias.command);
        return Ok(());
    }
    if run {
        return run_selected_command(&alias.command);
    }
    println!("{}", alias.command);
    eprintln!(
        "Command ready to copy. Use your shell's Aliaz shortcut to insert it for editing, or select --run to run it."
    );
    Ok(())
}

fn first_matching_alias(aliases: &[Alias], query: &str) -> Option<usize> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let words = query.to_ascii_lowercase();
    aliases.iter().position(|alias| {
        words
            .split_whitespace()
            .all(|word| alias_search_text(alias).contains(word))
    })
}

fn interactively_select_alias(aliases: &[Alias], query: &str) -> Result<Option<usize>> {
    let items = aliases.iter().map(alias_select_line).collect::<Vec<_>>();
    let selected = FuzzySelect::with_theme(&SimpleTheme)
        .with_prompt("Find command (Enter: review, Esc: cancel)")
        .items(&items)
        .with_initial_text(query)
        .default(0)
        .interact_opt()
        .context("failed to read command selection")?;
    if let Some(index) = selected {
        let alias = &aliases[index];
        eprintln!(
            "{} [{}]\n{}",
            alias.name,
            alias.collection_name,
            display_preview(&alias.command)
        );
        if !alias.metadata.description.is_empty() {
            eprintln!("{}", display_preview(&alias.metadata.description));
        }
        if !alias.metadata.tags.is_empty() {
            eprintln!("Tags: {}", display_line(&alias.metadata.tags.join(", ")));
        }
        if !Confirm::new()
            .with_prompt("Use this command?")
            .default(true)
            .interact()?
        {
            return Ok(None);
        }
    }
    Ok(selected)
}

fn alias_select_line(alias: &Alias) -> String {
    format!(
        "{}{} [{}]  {}  {}  {}",
        if alias.metadata.favorite { "* " } else { "" },
        alias.name,
        alias.collection_name,
        display_line(&alias.command),
        display_line(&alias.metadata.description),
        display_line(&alias.metadata.tags.join(" "))
    )
}

fn display_preview(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn display_line(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn alias_search_text(alias: &Alias) -> String {
    format!(
        "{} {} {} {} {}",
        alias.name,
        alias.command,
        alias.collection_name,
        alias.metadata.description,
        alias.metadata.tags.join(" ")
    )
    .to_ascii_lowercase()
}

fn run_selected_command(command: &str) -> Result<()> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let status = std::process::Command::new(shell)
        .arg("-lc")
        .arg(command)
        .status()
        .context("failed to run selected alias")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn update_release_binary_from_base_url(destination: &Path, base_url: &str) -> Result<()> {
    let target = release_target()?;
    let archive_name = format!("aliaz-{target}.tar.gz");
    let checksums_url = format!("{base_url}/checksums.txt");
    let archive_url = format!("{base_url}/{archive_name}");

    let checksums = download_text(&checksums_url)?;
    let expected_checksum = checksum_for_asset(&checksums, &archive_name)?;
    let archive_bytes = download_bytes(&archive_url)?;
    verify_checksum(&archive_bytes, &expected_checksum)?;

    let temp_dir = std::env::temp_dir().join(format!(
        "aliaz-update-{}-{}",
        std::process::id(),
        now_unix()?
    ));
    let release_dir = temp_dir.join("release");
    let binary_path = extract_release_binary(&archive_bytes, &release_dir)?;
    install_updated_binary(&binary_path, destination)?;

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

struct Store {
    conn: Connection,
}

struct StoreStatus {
    aliases: i64,
    collections: i64,
    active_collections: Vec<String>,
    pending: i64,
}

struct SyncResult {
    pulled: usize,
    pushed: usize,
}

impl Store {
    fn atomic<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let point = format!("aliaz_{}", Uuid::new_v4().simple());
        self.conn.execute_batch(&format!("SAVEPOINT {point}"))?;
        match operation() {
            Ok(value) => {
                self.conn.execute_batch(&format!("RELEASE {point}"))?;
                Ok(value)
            }
            Err(error) => {
                self.conn
                    .execute_batch(&format!("ROLLBACK TO {point}; RELEASE {point}"))?;
                Err(error)
            }
        }
    }

    fn import(&self, export: &ExportFile) -> Result<()> {
        self.atomic(|| {
            if export.version >= 2 {
                for collection in &export.collections {
                    self.create_collection(&collection.name)?;
                }
            }
            for alias in &export.aliases {
                self.upsert_in_collection(&alias.collection, &alias.name, &alias.command)?;
                if export.version >= 3 {
                    self.change_metadata(
                        &alias.collection,
                        &alias.name,
                        Some(alias.metadata.description.clone()),
                        Some(alias.metadata.tags.clone()),
                        Some(alias.metadata.favorite),
                    )?;
                }
            }
            Ok(())
        })
    }

    fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(path)?;
        let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema_version > SCHEMA_VERSION {
            bail!(
                "this alias database was written by a newer aliaz (schema v{schema_version}, this build understands v{SCHEMA_VERSION}); run `aliaz update`"
            );
        }
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS collections (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,
                dirty INTEGER NOT NULL DEFAULT 0,
                sync_version INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS active_collections (
                collection_id TEXT PRIMARY KEY,
                priority INTEGER NOT NULL,
                FOREIGN KEY (collection_id) REFERENCES collections(id)
            );

            CREATE TABLE IF NOT EXISTS aliases (
                id TEXT PRIMARY KEY,
                collection_id TEXT NOT NULL,
                name TEXT NOT NULL,
                command TEXT NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,
                dirty INTEGER NOT NULL DEFAULT 0,
                sync_version INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE(collection_id, name),
                FOREIGN KEY (collection_id) REFERENCES collections(id)
            );

            CREATE TABLE IF NOT EXISTS conflict_backups (
                id TEXT PRIMARY KEY,
                alias_name TEXT NOT NULL,
                command TEXT NOT NULL,
                remote_version INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            ",
        )?;
        ensure_shared_collection(&conn)?;
        migrate_alias_table(&conn)?;
        migrate_alias_collections(&conn)?;
        ensure_shared_collection(&conn)?;
        library::migrate(&conn)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;

        Ok(Self { conn })
    }

    fn create_collection(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        if name == DEFAULT_COLLECTION_NAME {
            ensure_shared_collection(&self.conn)?;
            return Ok(());
        }
        let now = now_unix()?;
        self.conn.execute(
            "
            INSERT INTO collections (id, name, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, 0, 1, 0, ?3, ?3)
            ON CONFLICT(name) DO UPDATE SET
                deleted = 0,
                dirty = 1,
                updated_at = excluded.updated_at
            ",
            params![collection_id_for_name(name), name, now],
        )?;
        Ok(())
    }

    fn collections(&self) -> Result<Vec<Collection>> {
        let mut statement = self.conn.prepare(
            "
            SELECT c.id, c.name, c.deleted, c.dirty, c.sync_version, c.updated_at,
                   CASE WHEN ac.collection_id IS NULL THEN 0 ELSE 1 END AS active,
                   COALESCE(ac.priority, -1) AS priority
            FROM collections c
            LEFT JOIN active_collections ac ON ac.collection_id = c.id
            WHERE c.deleted = 0
            ORDER BY
                CASE WHEN c.name = ?1 THEN 0 ELSE 1 END,
                CASE WHEN ac.collection_id IS NULL THEN 1 ELSE 0 END,
                ac.priority ASC,
                c.name ASC
            ",
        )?;
        Ok(statement
            .query_map([DEFAULT_COLLECTION_NAME], collection_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn collection_by_name(&self, name: &str) -> Result<Collection> {
        validate_name(name)?;
        let collection = self
            .conn
            .query_row(
                "
                SELECT c.id, c.name, c.deleted, c.dirty, c.sync_version, c.updated_at,
                       CASE WHEN ac.collection_id IS NULL THEN 0 ELSE 1 END AS active,
                       COALESCE(ac.priority, -1) AS priority
                FROM collections c
                LEFT JOIN active_collections ac ON ac.collection_id = c.id
                WHERE c.name = ?1 AND c.deleted = 0
                ",
                [name],
                collection_from_row,
            )
            .optional()?;
        collection.ok_or_else(|| anyhow!("collection not found: {name}"))
    }

    fn upsert_in_collection(&self, collection_name: &str, name: &str, command: &str) -> Result<()> {
        validate_alias_name(name)?;
        validate_command(command)?;
        let collection = self.collection_by_name(collection_name)?;
        let now = now_unix()?;
        self.conn.execute(
            "
            INSERT INTO aliases (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 0, 1, 0, ?5, ?5)
            ON CONFLICT(collection_id, name) DO UPDATE SET
                command = excluded.command,
                deleted = 0,
                dirty = 1,
                updated_at = excluded.updated_at
            ",
            params![Uuid::new_v4().to_string(), collection.id, name, command, now],
        )?;
        Ok(())
    }

    fn list_effective(&self) -> Result<Vec<Alias>> {
        let mut statement = self.conn.prepare(
            "
            SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                   a.sync_version, a.updated_at
            FROM aliases a
            JOIN collections c ON c.id = a.collection_id
            JOIN active_collections ac ON ac.collection_id = c.id
            WHERE a.deleted = 0 AND c.deleted = 0
              AND NOT EXISTS (
                SELECT 1
                FROM aliases newer
                JOIN active_collections newer_ac ON newer_ac.collection_id = newer.collection_id
                JOIN collections newer_c ON newer_c.id = newer.collection_id
                WHERE newer.name = a.name
                  AND newer.deleted = 0
                  AND newer_c.deleted = 0
                  AND newer_ac.priority > ac.priority
              )
            ORDER BY a.name ASC
            ",
        )?;
        Ok(statement
            .query_map([], alias_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn list_all(&self, collection_name: Option<&str>) -> Result<Vec<Alias>> {
        if let Some(collection_name) = collection_name {
            let collection = self.collection_by_name(collection_name)?;
            let mut statement = self.conn.prepare(
                "
                SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                       a.sync_version, a.updated_at
                FROM aliases a
                JOIN collections c ON c.id = a.collection_id
                WHERE a.deleted = 0 AND c.deleted = 0 AND a.collection_id = ?1
                ORDER BY a.name ASC
                ",
            )?;
            return Ok(statement
                .query_map([collection.id], alias_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?);
        }

        let mut statement = self.conn.prepare(
            "
            SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                   a.sync_version, a.updated_at
            FROM aliases a
            JOIN collections c ON c.id = a.collection_id
            WHERE a.deleted = 0 AND c.deleted = 0
            ORDER BY c.name ASC, a.name ASC
            ",
        )?;
        Ok(statement
            .query_map([], alias_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn activate_collections(&self, names: &[String]) -> Result<Vec<String>> {
        self.atomic(|| {
            let mut activated = Vec::new();
            let mut priority = self.next_collection_priority()?;
            for name in names {
                validate_name(name)?;
                if name == DEFAULT_COLLECTION_NAME {
                    continue;
                }
                let collection = self.collection_by_name(name)?;
                if collection.active {
                    continue;
                }
                self.conn.execute(
                    "INSERT INTO active_collections (collection_id, priority) VALUES (?1, ?2)",
                    params![collection.id, priority],
                )?;
                activated.push(collection.name);
                priority += 1;
            }
            Ok(activated)
        })
    }

    fn deactivate_collection(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        if name == DEFAULT_COLLECTION_NAME {
            bail!("shared is always active");
        }
        let collection = self.collection_by_name(name)?;
        let changed = self.conn.execute(
            "DELETE FROM active_collections WHERE collection_id = ?1",
            [collection.id],
        )?;
        Ok(changed > 0)
    }

    fn next_collection_priority(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(priority), 0) + 1 FROM active_collections",
            [],
            |row| row.get(0),
        )?)
    }

    fn update_alias(
        &self,
        collection_name: Option<&str>,
        name: &str,
        command: &str,
    ) -> Result<String> {
        validate_alias_name(name)?;
        validate_command(command)?;
        let collection = self.resolve_alias_scope(collection_name, name)?;
        let changed = self.conn.execute(
            "
            UPDATE aliases
            SET command = ?3, deleted = 0, dirty = 1, updated_at = ?4
            WHERE collection_id = ?1 AND name = ?2 AND deleted = 0
            ",
            params![collection.id, name, command, now_unix()?],
        )?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(collection.name)
    }

    fn delete_alias(&self, collection_name: Option<&str>, name: &str) -> Result<String> {
        validate_name(name)?;
        let collection = self.resolve_alias_scope(collection_name, name)?;
        let changed = self.conn.execute(
            "
            UPDATE aliases
            SET deleted = 1, dirty = 1, updated_at = ?3
            WHERE collection_id = ?1 AND name = ?2 AND deleted = 0
            ",
            params![collection.id, name, now_unix()?],
        )?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(collection.name)
    }

    fn move_alias(&self, name: &str, from: &str, to: &str) -> Result<()> {
        validate_alias_name(name)?;
        let from_collection = self.collection_by_name(from)?;
        let to_collection = self.collection_by_name(to)?;
        let target_exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM aliases WHERE collection_id = ?1 AND name = ?2 AND deleted = 0",
            params![to_collection.id, name],
            |row| row.get(0),
        )?;
        if target_exists > 0 {
            bail!("alias already exists in target collection: {name}");
        }
        self.atomic(|| {
            self.vacate_alias_slot(&to_collection.id, name)?;
            let now = now_unix()?;
            let changed = self.conn.execute(
                "
            UPDATE aliases
            SET collection_id = ?1, dirty = 1, updated_at = ?4
            WHERE collection_id = ?2 AND name = ?3 AND deleted = 0
            ",
                params![to_collection.id, from_collection.id, name, now],
            )?;
            if changed == 0 {
                bail!("alias not found: {name}");
            }
            Ok(())
        })
    }

    fn resolve_alias_scope(
        &self,
        collection_name: Option<&str>,
        alias_name: &str,
    ) -> Result<Collection> {
        if let Some(collection_name) = collection_name {
            return self.collection_by_name(collection_name);
        }

        let mut statement = self.conn.prepare(
            "
            SELECT c.id, c.name, c.deleted, c.dirty, c.sync_version, c.updated_at,
                   CASE WHEN ac.collection_id IS NULL THEN 0 ELSE 1 END AS active,
                   COALESCE(ac.priority, -1) AS priority
            FROM aliases a
            JOIN collections c ON c.id = a.collection_id
            LEFT JOIN active_collections ac ON ac.collection_id = c.id
            WHERE a.name = ?1 AND a.deleted = 0 AND c.deleted = 0
            ",
        )?;
        let collections = statement
            .query_map([alias_name], collection_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match collections.len() {
            0 => bail!("alias not found: {alias_name}"),
            1 => Ok(collections.into_iter().next().expect("one collection")),
            _ => bail!("alias is ambiguous; pass --collection"),
        }
    }

    fn pending_collections(&self) -> Result<Vec<Collection>> {
        let mut statement = self.conn.prepare(
            "
            SELECT c.id, c.name, c.deleted, c.dirty, c.sync_version, c.updated_at,
                   CASE WHEN ac.collection_id IS NULL THEN 0 ELSE 1 END AS active,
                   COALESCE(ac.priority, -1) AS priority
            FROM collections c
            LEFT JOIN active_collections ac ON ac.collection_id = c.id
            WHERE c.dirty = 1
            ORDER BY c.name ASC
            ",
        )?;
        Ok(statement
            .query_map([], collection_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn pending_aliases(&self) -> Result<Vec<Alias>> {
        let mut statement = self.conn.prepare(
            "
            SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                   a.sync_version, a.updated_at
            FROM aliases a
            JOIN collections c ON c.id = a.collection_id
            WHERE a.dirty = 1
            ORDER BY c.name ASC, a.name ASC
            ",
        )?;
        Ok(statement
            .query_map([], alias_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn find_alias_by_collection_and_name(
        &self,
        collection_id: &str,
        name: &str,
    ) -> Result<Option<Alias>> {
        Ok(self
            .conn
            .query_row(
                "
                SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                       a.sync_version, a.updated_at
                FROM aliases a
                JOIN collections c ON c.id = a.collection_id
                WHERE a.collection_id = ?1 AND a.name = ?2
                ",
                params![collection_id, name],
                alias_from_row,
            )
            .optional()?)
    }

    fn apply_remote_collection(
        &self,
        record: &RemoteRecord,
        payload: &CollectionPayload,
    ) -> Result<bool> {
        validate_name(&payload.name)?;
        let deleted = payload.deleted && payload.name != DEFAULT_COLLECTION_NAME;
        let known_version: Option<i64> = self
            .conn
            .query_row(
                "SELECT sync_version FROM collections WHERE name = ?1",
                [&payload.name],
                |row| row.get(0),
            )
            .optional()?;
        if known_version.is_some_and(|version| version >= record.version) {
            return Ok(false);
        }
        if let Ok(local) = self.collection_by_name(&payload.name)
            && local.dirty
            && local.updated_at > payload.updated_at
        {
            return Ok(false);
        }
        self.conn.execute(
            "
            INSERT INTO collections (id, name, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)
            ON CONFLICT(name) DO UPDATE SET
                deleted = excluded.deleted,
                dirty = 0,
                sync_version = excluded.sync_version,
                updated_at = excluded.updated_at
            ",
            params![
                record.id,
                payload.name,
                deleted as i64,
                record.version,
                now_unix()?,
                payload.updated_at
            ],
        )?;
        ensure_shared_collection(&self.conn)?;
        Ok(true)
    }

    fn alias_by_id(&self, id: &str) -> Result<Option<Alias>> {
        Ok(self
            .conn
            .query_row(
                "SELECT a.id, a.collection_id, c.name, a.name, a.command, a.deleted, a.dirty,
                    a.sync_version, a.updated_at FROM aliases a
             JOIN collections c ON c.id = a.collection_id WHERE a.id = ?1",
                [id],
                alias_from_row,
            )
            .optional()?)
    }

    fn backup_alias(&self, local: &Alias, version: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conflict_backups (id, alias_name, command, remote_version, created_at, snapshot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::new_v4().to_string(),
                format!("{}/{}", local.collection_name, local.name),
                local.command,
                version,
                now_unix()?,
                { let mut snapshot = local.clone(); snapshot.metadata = self.metadata(&local.id)?; serde_json::to_string(&snapshot)? }
            ],
        )?;
        Ok(())
    }

    fn vacate_alias_slot(&self, collection_id: &str, name: &str) -> Result<()> {
        if let Some(local) = self.find_alias_by_collection_and_name(collection_id, name)? {
            if !local.deleted {
                bail!("alias already exists in target collection: {name}");
            }
            // Keep the tombstone's sync identity while freeing the unique name slot.
            self.conn.execute(
                "UPDATE aliases SET name = ?2, dirty = 1 WHERE id = ?1",
                params![local.id, format!("__aliaz_deleted_{}", local.id)],
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn apply_remote_alias(&self, record: &RemoteRecord, payload: &AliasPayload) -> Result<bool> {
        self.apply_remote_alias_with_metadata(record, payload, None)
    }

    fn apply_remote_alias_with_metadata(
        &self,
        record: &RemoteRecord,
        payload: &AliasPayload,
        incoming_metadata: Option<&Metadata>,
    ) -> Result<bool> {
        validate_name(&payload.name)?;
        validate_command(&payload.command)?;
        validate_name(&payload.collection_name)?;
        self.atomic(|| {
            self.ensure_remote_collection(&payload.collection_id, &payload.collection_name)?;
            let collection = self.collection_by_name(&payload.collection_name)?;
            let existing = self.alias_by_id(&record.id)?;
            if let Some(local) = &existing
                && (local.sync_version >= record.version || (local.dirty && local.updated_at > payload.updated_at)) {
                return Ok(false);
            }
            let target = self.find_alias_by_collection_and_name(&collection.id, &payload.name)?;
            if let Some(local) = target.as_ref().filter(|local| local.id != record.id) {
                if !local.deleted && (local.updated_at > payload.updated_at ||
                    (local.updated_at == payload.updated_at && local.id > record.id)) {
                    // Publish a tombstone for the losing remote identity so devices converge.
                    self.conn.execute(
                        "INSERT INTO conflict_backups (id, alias_name, command, remote_version, created_at, snapshot, metadata_pending)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![Uuid::new_v4().to_string(), format!("{}/{}", payload.collection_name, payload.name),
                                payload.command, record.version, now_unix()?, serde_json::to_string(&Alias {
                                    id: record.id.clone(), collection_id: collection.id.clone(), collection_name: collection.name.clone(),
                                    name: payload.name.clone(), command: payload.command.clone(), metadata: incoming_metadata.cloned().unwrap_or(self.metadata(&record.id)?),
                                    deleted: false, dirty: false, sync_version: record.version, updated_at: payload.updated_at,
                                })?, incoming_metadata.is_none() && !self.has_metadata(&record.id)?])?;
                    if let Some(existing) = &existing
                        && !existing.deleted { self.backup_alias(existing, record.version)?; }
                    self.conn.execute(
                        "INSERT INTO aliases (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?6, ?7)
                         ON CONFLICT(id) DO UPDATE SET name = excluded.name, deleted = 1, dirty = 1,
                             sync_version = excluded.sync_version, updated_at = excluded.updated_at",
                        params![record.id, collection.id, format!("__aliaz_deleted_{}", record.id),
                                payload.command, record.version, now_unix()?, payload.updated_at])?;
                    return Ok(true);
                }
                if !local.deleted { self.backup_alias(local, record.version)?; }
                self.conn.execute(
                    "UPDATE aliases SET deleted = 1, dirty = 1, name = ?2 WHERE id = ?1",
                    params![local.id, format!("__aliaz_deleted_{}", local.id)])?;
            }
            if let Some(local) = &existing
                && local.dirty { self.backup_alias(local, record.version)?; }
            self.conn.execute(
                "INSERT INTO aliases (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET collection_id = excluded.collection_id,
                    name = excluded.name, command = excluded.command, deleted = excluded.deleted,
                    dirty = 0, sync_version = excluded.sync_version, updated_at = excluded.updated_at",
                params![record.id, collection.id, payload.name, payload.command, payload.deleted as i64,
                        record.version, now_unix()?, payload.updated_at])?;
            Ok(true)
        })
    }

    fn ensure_remote_collection(&self, id: &str, name: &str) -> Result<()> {
        validate_name(name)?;
        if self.collection_by_name(name).is_ok() {
            return Ok(());
        }
        let now = now_unix()?;
        self.conn.execute(
            "
            INSERT INTO collections (id, name, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, 0, 0, 0, ?3, ?3)
            ON CONFLICT(name) DO UPDATE SET deleted = 0
            ",
            params![id, name, now],
        )?;
        Ok(())
    }

    fn mark_collection_synced(&self, snapshot: &Collection, version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE collections SET
             dirty = CASE WHEN name = ?3 AND deleted = ?4 AND updated_at = ?5 AND sync_version = ?6
                          THEN 0 ELSE dirty END,
             sync_version = MAX(sync_version, ?2) WHERE id = ?1",
            params![
                snapshot.id,
                version,
                snapshot.name,
                snapshot.deleted as i64,
                snapshot.updated_at,
                snapshot.sync_version
            ],
        )?;
        Ok(())
    }

    fn mark_alias_synced(&self, snapshot: &Alias, version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE aliases SET
             dirty = CASE WHEN collection_id = ?3 AND name = ?4 AND command = ?5 AND deleted = ?6
                          AND updated_at = ?7 AND sync_version = ?8 THEN 0 ELSE dirty END,
             sync_version = MAX(sync_version, ?2) WHERE id = ?1",
            params![
                snapshot.id,
                version,
                snapshot.collection_id,
                snapshot.name,
                snapshot.command,
                snapshot.deleted as i64,
                snapshot.updated_at,
                snapshot.sync_version
            ],
        )?;
        Ok(())
    }

    fn status(&self) -> Result<StoreStatus> {
        let aliases = self.list_effective()?.len() as i64;
        let collections = self.conn.query_row(
            "SELECT COUNT(*) FROM collections WHERE deleted = 0",
            [],
            |row| row.get(0),
        )?;
        let pending = self.conn.query_row(
            "
            SELECT
                (SELECT COUNT(*) FROM collections WHERE dirty = 1) +
                (SELECT COUNT(*) FROM aliases WHERE dirty = 1) +
                (SELECT COUNT(*) FROM alias_metadata WHERE dirty = 1)
            ",
            [],
            |row| row.get(0),
        )?;
        let active_collections = self
            .collections()?
            .into_iter()
            .filter(|collection| collection.active)
            .map(|collection| collection.name)
            .collect();
        Ok(StoreStatus {
            aliases,
            collections,
            active_collections,
            pending,
        })
    }
}

fn alias_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Alias> {
    Ok(Alias {
        id: row.get(0)?,
        collection_id: row.get(1)?,
        collection_name: row.get(2)?,
        name: row.get(3)?,
        command: row.get(4)?,
        metadata: Metadata::default(),
        deleted: row.get::<_, i64>(5)? == 1,
        dirty: row.get::<_, i64>(6)? == 1,
        sync_version: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn collection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: row.get(0)?,
        name: row.get(1)?,
        deleted: row.get::<_, i64>(2)? == 1,
        dirty: row.get::<_, i64>(3)? == 1,
        sync_version: row.get(4)?,
        updated_at: row.get(5)?,
        active: row.get::<_, i64>(6)? == 1,
        priority: row.get(7)?,
    })
}

fn collection_id_for_name(name: &str) -> String {
    if name == DEFAULT_COLLECTION_NAME {
        DEFAULT_COLLECTION_ID.to_owned()
    } else {
        format!("collection:{name}")
    }
}

fn ensure_shared_collection(conn: &Connection) -> Result<()> {
    let now = now_unix()?;
    conn.execute(
        "
        INSERT INTO collections (id, name, deleted, dirty, sync_version, created_at, updated_at)
        VALUES (?1, ?2, 0, 0, 0, ?3, ?3)
        ON CONFLICT(id) DO UPDATE SET deleted = 0, name = excluded.name
        ",
        params![DEFAULT_COLLECTION_ID, DEFAULT_COLLECTION_NAME, now],
    )?;
    conn.execute(
        "
        INSERT INTO active_collections (collection_id, priority)
        VALUES (?1, 0)
        ON CONFLICT(collection_id) DO UPDATE SET priority = 0
        ",
        params![DEFAULT_COLLECTION_ID],
    )?;
    Ok(())
}

fn migrate_alias_table(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "aliases")?;
    if !columns.contains("id") {
        conn.execute("ALTER TABLE aliases ADD COLUMN id TEXT", [])?;
    }
    if !columns.contains("created_at") {
        conn.execute(
            "ALTER TABLE aliases ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("updated_at") {
        conn.execute(
            "ALTER TABLE aliases ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("deleted") {
        conn.execute(
            "ALTER TABLE aliases ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("dirty") {
        conn.execute(
            "ALTER TABLE aliases ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("sync_version") {
        conn.execute(
            "ALTER TABLE aliases ADD COLUMN sync_version INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    let names = {
        let mut statement = conn.prepare("SELECT name FROM aliases WHERE id IS NULL OR id = ''")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for name in names {
        conn.execute(
            "UPDATE aliases SET id = ?2 WHERE name = ?1",
            params![name, Uuid::new_v4().to_string()],
        )?;
    }
    conn.execute(
        "UPDATE aliases SET created_at = ?1 WHERE created_at IS NULL OR created_at = 0 OR typeof(created_at) = 'text'",
        params![now_unix()?],
    )?;
    conn.execute(
        "UPDATE aliases SET updated_at = ?1 WHERE updated_at IS NULL OR updated_at = 0 OR typeof(updated_at) = 'text'",
        params![now_unix()?],
    )?;

    Ok(())
}

fn migrate_alias_collections(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "aliases")?;
    if columns.contains("collection_id") {
        return Ok(());
    }

    conn.execute_batch(
        "
        CREATE TABLE aliases_new (
            id TEXT PRIMARY KEY,
            collection_id TEXT NOT NULL,
            name TEXT NOT NULL,
            command TEXT NOT NULL,
            deleted INTEGER NOT NULL DEFAULT 0,
            dirty INTEGER NOT NULL DEFAULT 0,
            sync_version INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(collection_id, name),
            FOREIGN KEY (collection_id) REFERENCES collections(id)
        );
        ",
    )?;
    conn.execute(
        "
        INSERT INTO aliases_new (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
        SELECT id, ?1, name, command, deleted, dirty, sync_version, created_at, updated_at
        FROM aliases
        ",
        params![DEFAULT_COLLECTION_ID],
    )?;
    conn.execute_batch(
        "
        DROP TABLE aliases;
        ALTER TABLE aliases_new RENAME TO aliases;
        ",
    )?;
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(columns)
}

fn parse_aliases(contents: &str) -> Result<Vec<Alias>> {
    let mut aliases = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("alias ") {
            continue;
        }

        let entries = shlex::split(trimmed.trim_start_matches("alias ").trim())
            .ok_or_else(|| anyhow!("failed to parse alias line: {trimmed}"))?;
        for entry in entries {
            if let Some((name, command)) = entry.split_once('=') {
                validate_name(name)?;
                aliases.push(Alias {
                    id: Uuid::new_v4().to_string(),
                    collection_id: DEFAULT_COLLECTION_ID.to_owned(),
                    collection_name: DEFAULT_COLLECTION_NAME.to_owned(),
                    name: name.to_owned(),
                    command: command.to_owned(),
                    metadata: Metadata::default(),
                    deleted: false,
                    dirty: true,
                    sync_version: 0,
                    updated_at: now_unix()?,
                });
            }
        }
    }

    Ok(aliases)
}

fn validate_command(command: &str) -> Result<()> {
    if command.contains('\0') {
        bail!("command contains a null character and cannot be used by a shell");
    }
    Ok(())
}

fn validate_alias_name(name: &str) -> Result<()> {
    validate_name(name)?;
    if name.starts_with('-') || name == "aliaz" || name.starts_with("__aliaz_") {
        bail!("alias name cannot start with '-' or '__aliaz_' or be 'aliaz': {name}");
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
    if !valid {
        bail!("invalid alias name: {name}");
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn config_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("ALIAZ_CONFIG_HOME") {
        return Ok(PathBuf::from(home));
    }

    dirs::config_dir().ok_or_else(|| anyhow!("could not locate config directory"))
}

fn shell_config_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("ALIAZ_CONFIG_HOME") {
        return Ok(PathBuf::from(home));
    }

    if let Some(home) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(home));
    }

    Ok(home_dir()?.join(".config"))
}

fn config_path() -> Result<PathBuf> {
    Ok(config_home()?.join("aliaz").join("config.json"))
}

fn prompt_secret_if_missing(value: Option<String>, prompt: &str) -> Result<String> {
    match value {
        Some(value) => Ok(value),
        None => rpassword::prompt_password(prompt).context("failed to read secret from terminal"),
    }
}

fn prompt_username_if_missing(value: Option<String>) -> Result<String> {
    if let Some(value) = value {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    if !io::stdin().is_terminal() {
        bail!("username is required; pass --username <name>");
    }
    loop {
        print!("Username: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            bail!("username is required; pass --username <name>");
        }
        let name = answer.trim();
        if !name.is_empty() {
            return Ok(name.to_owned());
        }
        println!("Username cannot be empty.");
    }
}

fn parse_collection_csv(value: Option<&str>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for raw in value.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        validate_name(name)?;
        if name != DEFAULT_COLLECTION_NAME {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn create_and_activate_collections(store: &Store, names: &[String]) -> Result<()> {
    for name in names {
        store.create_collection(name)?;
    }
    store.activate_collections(names)?;
    Ok(())
}

fn prompt_for_collection_selection(store: &Store) -> Result<Vec<String>> {
    if !io::stdin().is_terminal() {
        return Ok(Vec::new());
    }

    let names = store
        .collections()?
        .into_iter()
        .filter(|collection| collection.name != DEFAULT_COLLECTION_NAME)
        .map(|collection| collection.name)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Ok(Vec::new());
    }

    println!("Available collections: {}", names.join(", "));
    print!("Activate collections for this computer (comma-separated, blank for shared only): ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    parse_collection_csv(Some(answer.trim()))
}

fn load_config() -> Result<Option<SyncConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let stored: StoredSyncConfig = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let config = SyncConfig {
        sync_url: stored.sync_url,
        username: stored.username,
        user_id: stored.user_id,
        token: stored.token,
        latest_version: stored.latest_version,
    };
    if let Some(recovery_phrase) = stored.recovery_phrase {
        store_recovery_phrase(&config.user_id, &recovery_phrase)?;
        save_config(&config)?;
    }
    Ok(Some(config))
}

fn save_config(config: &SyncConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(config)?),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn remove_config() -> Result<()> {
    let path = config_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn store_recovery_phrase(user_id: &str, recovery_phrase: &str) -> Result<()> {
    if let Some(secret_home) = configured_file_secret_home() {
        store_file_recovery_phrase(&secret_home, user_id, recovery_phrase)?;
        return Ok(());
    }

    match Entry::new(KEYRING_SERVICE, user_id).and_then(|entry| entry.set_password(recovery_phrase))
    {
        Ok(()) => Ok(()),
        Err(keyring_error) => {
            let secret_home = default_file_secret_home()?;
            store_file_recovery_phrase(&secret_home, user_id, recovery_phrase).with_context(
                || {
                    format!(
                        "failed to store recovery phrase in OS credential store ({keyring_error}); file fallback also failed at {}",
                        secret_home.display()
                    )
                },
            )?;
            eprintln!(
                "aliaz: OS credential store unavailable; stored recovery phrase in file secret store at {}",
                secret_home.display()
            );
            Ok(())
        }
    }
}

fn load_recovery_phrase(user_id: &str) -> Result<String> {
    if let Some(secret_home) = configured_file_secret_home() {
        return load_file_recovery_phrase(&secret_home, user_id);
    }

    match Entry::new(KEYRING_SERVICE, user_id).and_then(|entry| entry.get_password()) {
        Ok(recovery_phrase) => Ok(recovery_phrase),
        Err(keyring_error) => {
            let secret_home = default_file_secret_home()?;
            let path = file_secret_path(&secret_home, user_id);
            if path.exists() {
                return load_file_recovery_phrase(&secret_home, user_id);
            }

            Err(keyring_error).with_context(|| os_secret_store_hint("recovery phrase is missing"))
        }
    }
}

fn remove_recovery_phrase(user_id: &str) -> Result<()> {
    if let Some(secret_home) = configured_file_secret_home() {
        remove_file_recovery_phrase(&secret_home, user_id)?;
    }

    let _ = Entry::new(KEYRING_SERVICE, user_id).and_then(|entry| entry.delete_credential());
    remove_file_recovery_phrase(&default_file_secret_home()?, user_id)?;
    Ok(())
}

fn recovery_phrase_available(user_id: &str) -> bool {
    load_recovery_phrase(user_id).is_ok()
}

fn shell_name(shell: &Shell) -> &'static str {
    match shell {
        Shell::Zsh => "zsh",
        Shell::Bash => "bash",
        Shell::Fish => "fish",
    }
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn remove_empty_directory(path: &Path) -> Result<()> {
    match fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                fs::remove_dir(path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn remove_binary_best_effort(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Ok(false),
    }
}

fn shell_integration_path(shell: &Shell) -> Result<PathBuf> {
    Ok(match shell {
        Shell::Zsh | Shell::Bash => shell_config_home()?.join("aliaz").join("aliases.sh"),
        Shell::Fish => shell_config_home()?
            .join("fish")
            .join("conf.d")
            .join("aliaz.fish"),
    })
}

fn startup_path(shell: &Shell) -> Result<PathBuf> {
    Ok(match shell {
        Shell::Zsh => std::env::var_os("ZDOTDIR")
            .map(PathBuf::from)
            .unwrap_or(home_dir()?)
            .join(".zshrc"),
        Shell::Bash => home_dir()?.join(".bashrc"),
        Shell::Fish => bail!("fish does not use a zsh/bash startup file"),
    })
}

fn uninstall_shell_integration() -> Result<bool> {
    let mut removed_any = false;

    let zsh_path = shell_integration_path(&Shell::Zsh)?;
    removed_any |= remove_file_if_exists(&zsh_path)?;
    if let Some(parent) = zsh_path.parent() {
        remove_empty_directory(parent)?;
    }

    let fish_path = shell_integration_path(&Shell::Fish)?;
    removed_any |= remove_file_if_exists(&fish_path)?;
    if let Some(parent) = fish_path.parent() {
        remove_empty_directory(parent)?;
    }

    removed_any |= uninstall_startup_file(&Shell::Zsh)?;
    removed_any |= uninstall_startup_file(&Shell::Bash)?;

    Ok(removed_any)
}

fn uninstall_startup_file(shell: &Shell) -> Result<bool> {
    let mut paths = vec![startup_path(shell)?];
    if matches!(shell, Shell::Bash) {
        for profile in [".bash_profile", ".bash_login", ".profile"] {
            paths.push(home_dir()?.join(profile));
        }
    }
    let mut removed = false;
    for path in paths {
        removed |= uninstall_startup_path(shell, &path)?;
    }
    Ok(removed)
}

fn uninstall_startup_path(shell: &Shell, startup_path: &Path) -> Result<bool> {
    let integration_path = shell_integration_path(shell)?;
    let source_line = startup_source_line(startup_path, &integration_path)?;
    let comment = "# Aliaz shell integration";

    let existing = match fs::read_to_string(startup_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", startup_path.display()));
        }
    };

    if !existing.contains(&source_line) {
        return Ok(false);
    }

    let mut lines = existing.lines().peekable();
    let mut kept = Vec::new();
    let mut removed = false;

    while let Some(line) = lines.next() {
        if line == comment && matches!(lines.peek(), Some(next) if *next == source_line) {
            lines.next();
            removed = true;
            continue;
        }

        if line == source_line {
            removed = true;
            continue;
        }

        kept.push(line);
    }

    if !removed {
        return Ok(false);
    }

    if kept.is_empty() {
        remove_file_if_exists(startup_path)?;
        return Ok(true);
    }

    let mut contents = kept.join("\n");
    contents.push('\n');
    fs::write(startup_path, contents)
        .with_context(|| format!("failed to write {}", startup_path.display()))?;
    Ok(true)
}

fn shell_from_env() -> Option<Shell> {
    let shell = std::env::var_os("SHELL")?;
    let shell = Path::new(&shell).file_name()?.to_str()?;
    match shell {
        "zsh" => Some(Shell::Zsh),
        "bash" => Some(Shell::Bash),
        "fish" => Some(Shell::Fish),
        _ => None,
    }
}

fn shells_to_repair() -> Result<Vec<Shell>> {
    let mut shells = Vec::new();

    for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
        let integration_path = shell_integration_path(&shell)?;
        let startup_exists = match shell {
            Shell::Zsh | Shell::Bash => startup_path(&shell)?.exists(),
            Shell::Fish => false,
        };

        if integration_path.exists() || startup_exists {
            shells.push(shell);
        }
    }

    if shells.is_empty() {
        shells.push(shell_from_env().unwrap_or(Shell::Zsh));
    }

    Ok(shells)
}

fn repair_shell_integration(shell: &Shell, aliases: &[Alias]) -> Result<Vec<String>> {
    let mut messages = Vec::new();
    let path = write_shell_integration(shell, aliases)?;
    messages.push(format!(
        "repaired {} integration at {}",
        shell_name(shell),
        path.display()
    ));

    if matches!(shell, Shell::Zsh | Shell::Bash) {
        let startup_path = configure_startup_file(shell, &path)?;
        messages.push(format!("configured {}", startup_path.display()));
    }

    Ok(messages)
}

fn configured_file_secret_home() -> Option<PathBuf> {
    std::env::var_os("ALIAZ_SECRET_HOME")
        .or_else(|| std::env::var_os("ALIAZ_TEST_SECRET_HOME"))
        .map(PathBuf::from)
}

fn default_file_secret_home() -> Result<PathBuf> {
    Ok(config_home()?.join("aliaz").join("secrets"))
}

fn store_file_recovery_phrase(
    secret_home: &Path,
    user_id: &str,
    recovery_phrase: &str,
) -> Result<()> {
    fs::create_dir_all(secret_home)
        .with_context(|| format!("failed to create {}", secret_home.display()))?;
    restrict_secret_dir(secret_home)?;

    let path = file_secret_path(secret_home, user_id);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    restrict_secret_file(&path)?;
    file.write_all(recovery_phrase.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

fn load_file_recovery_phrase(secret_home: &Path, user_id: &str) -> Result<String> {
    let path = file_secret_path(secret_home, user_id);
    fs::read_to_string(&path).with_context(|| {
        format!(
            "recovery phrase is missing from file secret store at {}",
            path.display()
        )
    })
}

fn remove_file_recovery_phrase(secret_home: &Path, user_id: &str) -> Result<()> {
    let path = file_secret_path(secret_home, user_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn file_secret_path(secret_home: &Path, user_id: &str) -> PathBuf {
    secret_home.join(secret_file_name(user_id))
}

#[cfg(unix)]
fn restrict_secret_dir(secret_home: &Path) -> Result<()> {
    fs::set_permissions(secret_home, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to protect {}", secret_home.display()))
}

#[cfg(not(unix))]
fn restrict_secret_dir(_secret_home: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_secret_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_secret_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn os_secret_store_hint(action: &str) -> String {
    format!(
        "{action} in OS credential store. On headless Linux, install a Secret Service provider or set ALIAZ_SECRET_HOME to a private directory for file-backed recovery phrase storage"
    )
}

fn secret_file_name(user_id: &str) -> String {
    user_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_shell_integration(shell: &Shell, aliases: &[Alias]) -> Result<PathBuf> {
    let path = shell_integration_path(shell)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = shell_integration_contents(shell, aliases, &path)?;
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn shell_integration_contents(shell: &Shell, aliases: &[Alias], path: &PathBuf) -> Result<String> {
    let binary = std::env::current_exe().context("failed to locate aliaz binary")?;
    let binary = binary
        .to_str()
        .ok_or_else(|| anyhow!("aliaz binary path is not valid UTF-8"))?;
    let source_command = sh_source_command(path)?;
    let mut lines = match shell {
        Shell::Zsh | Shell::Bash => sh_wrapper_lines(binary, &source_command),
        Shell::Fish => fish_wrapper_lines(binary, path),
    };
    let cleanup = match shell {
        Shell::Bash | Shell::Zsh => {
            "unalias __aliaz_cleanup 2>/dev/null || :; if command -v __aliaz_cleanup >/dev/null 2>&1; then __aliaz_cleanup; fi"
        }
        Shell::Fish => "if functions -q __aliaz_cleanup; __aliaz_cleanup; end",
    };
    lines.insert(1, cleanup.to_owned());
    let mut next_cleanup = Vec::new();
    for (index, alias) in aliases
        .iter()
        .filter(|alias| validate_alias_name(&alias.name).is_ok())
        .enumerate()
    {
        let name = shell_quote(&alias.name);
        let variable = format!("__aliaz_owned_{index}");
        let definition = shell_alias_lines(shell, std::slice::from_ref(alias)).remove(0);
        match shell {
            Shell::Bash | Shell::Zsh => {
                lines.push(format!("unset {variable}"));
                lines.push(format!(
                    "case \" ${{__aliaz_overrides:-}} \" in *\" {} \"*) ;; *)",
                    alias.name
                ));
                lines.push(definition);
                lines.push(format!("{variable}=$(alias -- {name} 2>/dev/null)"));
                lines.push(";; esac".to_owned());
                next_cleanup.push(format!("if [ -n \"${{{variable}:-}}\" ]; then if [ \"$(alias -- {name} 2>/dev/null)\" = \"${{{variable}}}\" ]; then unalias -- {name}; elif alias -- {name} >/dev/null 2>&1; then __aliaz_overrides=\"${{__aliaz_overrides:-}} {}\"; fi; fi", alias.name));
            }
            Shell::Fish => {
                lines.push(format!("set -e {variable}"));
                lines.push(format!("if not contains -- {name} $__aliaz_overrides"));
                lines.push(definition);
                lines.push(format!(
                    "set -g {variable} (functions {name} | string collect)"
                ));
                lines.push("end".to_owned());
                next_cleanup.push(format!("if set -q {variable}; set -l __aliaz_current_definition (functions {name} | string collect); if test \"$__aliaz_current_definition\" = \"${variable}\"; functions -e {name}; else; if functions -q {name}; set -ga __aliaz_overrides {name}; end; end; end"));
            }
        }
    }
    match shell {
        Shell::Bash | Shell::Zsh => {
            lines.push("__aliaz_cleanup() {".to_owned());
            lines.push(":".to_owned());
            lines.extend(next_cleanup);
            lines.push("}".to_owned());
        }
        Shell::Fish => {
            lines.push("function __aliaz_cleanup".to_owned());
            lines.extend(next_cleanup);
            lines.push("end".to_owned());
        }
    }
    let mut contents = lines.join("\n");
    contents.push('\n');
    Ok(contents)
}

fn sh_source_command(path: &PathBuf) -> Result<String> {
    Ok(format!("source {}", sh_source_path_token(path)?))
}

fn sh_source_path_token(path: &PathBuf) -> Result<String> {
    let default_path = home_dir()?.join(".config").join("aliaz").join("aliases.sh");
    if *path == default_path {
        Ok(r#""$HOME/.config/aliaz/aliases.sh""#.to_owned())
    } else {
        Ok(shell_quote(&path.display().to_string()))
    }
}

fn sh_wrapper_lines(binary: &str, source_command: &str) -> Vec<String> {
    shell_library::sh(binary, source_command)
}

fn fish_wrapper_lines(binary: &str, path: &Path) -> Vec<String> {
    shell_library::fish(binary, path)
}

fn configure_startup_file(shell: &Shell, integration_path: &PathBuf) -> Result<PathBuf> {
    let startup_path = startup_path(shell)?;
    configure_startup_path(&startup_path, integration_path)?;
    if matches!(shell, Shell::Bash) {
        let home = home_dir()?;
        let profile = [".bash_profile", ".bash_login", ".profile"]
            .iter()
            .map(|name| home.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| home.join(".bash_profile"));
        configure_startup_path(&profile, integration_path)?;
    }
    Ok(startup_path)
}

fn startup_source_line(startup_path: &Path, integration_path: &PathBuf) -> Result<String> {
    let source = sh_source_command(integration_path)?;
    if startup_path
        .file_name()
        .is_some_and(|name| name == ".profile")
    {
        Ok(format!(
            "if [ -n \"${{BASH_VERSION:-}}\" ]; then {source}; fi"
        ))
    } else {
        Ok(source)
    }
}

fn configure_startup_path(startup_path: &Path, integration_path: &PathBuf) -> Result<()> {
    let source_line = startup_source_line(startup_path, integration_path)?;
    let comment = "# Aliaz shell integration";
    let existing = match fs::read_to_string(startup_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", startup_path.display()));
        }
    };
    if existing.lines().any(|line| line.trim() == source_line) {
        return Ok(());
    }

    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(comment);
    contents.push('\n');
    contents.push_str(&source_line);
    contents.push('\n');
    fs::write(startup_path, contents)
        .with_context(|| format!("failed to write {}", startup_path.display()))?;
    Ok(())
}

fn shell_alias_lines(shell: &Shell, aliases: &[Alias]) -> Vec<String> {
    aliases
        .iter()
        .filter(|alias| validate_alias_name(&alias.name).is_ok())
        .map(|alias| match shell {
            Shell::Zsh | Shell::Bash => {
                format!("alias {}={}", alias.name, shell_quote(&alias.command))
            }
            Shell::Fish => {
                format!("alias {} {}", alias.name, shell_quote(&alias.command))
            }
        })
        .collect()
}

fn account_request(
    sync_url: &str,
    path: &str,
    username: &str,
    password: &str,
) -> Result<AccountResponse> {
    let url = format!("{}/v1/{path}", sync_url.trim_end_matches('/'));
    let response = Client::new()
        .post(url)
        .json(&AccountRequest { username, password })
        .send()?;
    if !response.status().is_success() {
        bail!("server returned {}", response.status());
    }
    Ok(response.json()?)
}

fn login_config(sync_url: &str, username: &str, response: AccountResponse) -> SyncConfig {
    SyncConfig {
        sync_url: sync_url.to_owned(),
        username: username.to_owned(),
        user_id: response.user_id,
        token: response.token,
        latest_version: 0,
    }
}

fn sync_aliases(
    store: &Store,
    config: &mut SyncConfig,
    recovery_phrase: &str,
) -> Result<SyncResult> {
    let client = Client::new();
    let key = derive_key(recovery_phrase)?;
    let pull_url = format!(
        "{}/v1/records?after={}",
        config.sync_url.trim_end_matches('/'),
        config.latest_version
    );
    let pull: PullResponse = client
        .get(pull_url)
        .bearer_auth(&config.token)
        .send()?
        .error_for_status()?
        .json()?;

    let mut pulled = 0;
    let incoming_metadata = pull
        .records
        .iter()
        .filter(|record| record.record_type == "alias_metadata")
        .map(|record| {
            Ok((record, {
                let payload =
                    decrypt_record::<library::MetadataPayload>(&key, &record.encrypted_blob)?;
                if record.id != library::metadata_record_id(&payload.alias_id) {
                    bail!("command details have an invalid identity");
                }
                payload
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    for record in &pull.records {
        if record.record_type == "collection" {
            let payload: CollectionPayload = decrypt_record(&key, &record.encrypted_blob)?;
            if store.apply_remote_collection(record, &payload)? {
                pulled += 1;
            }
        }
    }
    // Keep local details intact until losing local commands have been backed up.
    for record in &pull.records {
        if record.record_type == "alias" {
            let payload: AliasPayload = decrypt_record(&key, &record.encrypted_blob)?;
            let details = incoming_metadata
                .iter()
                .find(|(_, details)| details.alias_id == record.id)
                .map(|(_, details)| &details.metadata);
            if store.apply_remote_alias_with_metadata(record, &payload, details)? {
                pulled += 1;
            }
        }
    }
    for (record, payload) in incoming_metadata {
        if store.apply_remote_metadata(record, &payload)? {
            pulled += 1;
        }
    }
    config.latest_version = pull.latest_version;

    let pending_collections = store.pending_collections()?;
    let pending_aliases = store.pending_aliases()?;
    let pending_metadata = store.pending_metadata()?;
    let mut uploads = Vec::with_capacity(pending_collections.len() + pending_aliases.len());
    for collection in &pending_collections {
        let payload = CollectionPayload {
            name: collection.name.clone(),
            deleted: collection.deleted,
            updated_at: collection.updated_at,
        };
        uploads.push(UploadRecord {
            id: collection.id.clone(),
            record_type: "collection".to_owned(),
            encrypted_blob: encrypt_record(&key, &payload)?,
            updated_at: collection.updated_at,
        });
    }
    for alias in &pending_aliases {
        let payload = AliasPayload {
            collection_id: alias.collection_id.clone(),
            collection_name: alias.collection_name.clone(),
            name: alias.name.clone(),
            command: alias.command.clone(),
            deleted: alias.deleted,
            updated_at: alias.updated_at,
        };
        uploads.push(UploadRecord {
            id: alias.id.clone(),
            record_type: "alias".to_owned(),
            encrypted_blob: encrypt_record(&key, &payload)?,
            updated_at: alias.updated_at,
        });
    }

    for payload in &pending_metadata {
        uploads.push(UploadRecord {
            id: library::metadata_record_id(&payload.alias_id),
            record_type: "alias_metadata".to_owned(),
            encrypted_blob: encrypt_record(&key, payload)?,
            updated_at: payload.updated_at,
        });
    }
    let pushed = uploads.len();
    for chunk in uploads.chunks(500) {
        let push_url = format!("{}/v1/records", config.sync_url.trim_end_matches('/'));
        let response = client
            .post(push_url)
            .bearer_auth(&config.token)
            .json(&PushRequest { records: chunk })
            .send()?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST && !pending_metadata.is_empty() {
            let error: serde_json::Value = response.json().unwrap_or_default();
            if error.get("error").and_then(|value| value.as_str()) == Some("invalid_record_type") {
                bail!("Command details cannot sync yet. Your changes remain saved locally.");
            }
            bail!("sync upload was rejected; your changes remain saved locally");
        }
        let push: PushResponse = response.error_for_status()?.json()?;
        for record in push.records {
            if let Some(snapshot) = pending_collections
                .iter()
                .find(|entry| entry.id == record.id)
            {
                store.mark_collection_synced(snapshot, record.version)?;
            } else if let Some(snapshot) =
                pending_aliases.iter().find(|entry| entry.id == record.id)
            {
                store.mark_alias_synced(snapshot, record.version)?;
            } else if let Some(snapshot) = pending_metadata
                .iter()
                .find(|entry| library::metadata_record_id(&entry.alias_id) == record.id)
            {
                store.mark_metadata_synced(snapshot, record.version)?;
            }
        }
        // Only the pull watermark is safe: other devices may have pushed before our upload.
    }

    Ok(SyncResult { pulled, pushed })
}

fn derive_key(recovery_phrase: &str) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(
        Some(b"aliaz recovery phrase v1"),
        recovery_phrase.as_bytes(),
    );
    let mut key = [0u8; 32];
    hk.expand(b"alias record encryption", &mut key)
        .map_err(|_| anyhow!("failed to derive encryption key"))?;
    Ok(key)
}

fn encrypt_record<T: Serialize>(key: &[u8; 32], payload: &T) -> Result<String> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, serde_json::to_vec(payload)?.as_ref())
        .map_err(|_| anyhow!("failed to encrypt record"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend(ciphertext);
    Ok(BASE64.encode(blob))
}

fn decrypt_record<T: for<'de> Deserialize<'de>>(key: &[u8; 32], encrypted_blob: &str) -> Result<T> {
    let blob = BASE64.decode(encrypted_blob)?;
    if blob.len() < 25 {
        bail!("encrypted record blob is too short");
    }
    let (nonce_bytes, ciphertext) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow!("failed to decrypt record"))?;
    Ok(serde_json::from_slice(&plaintext)?)
}

fn report_integration_status() -> Result<()> {
    let sh_path = shell_config_home()?.join("aliaz").join("aliases.sh");
    let fish_path = shell_config_home()?
        .join("fish")
        .join("conf.d")
        .join("aliaz.fish");
    if sh_path.exists() {
        println!("zsh/bash integration: ok");
    } else {
        println!("zsh/bash integration: missing");
    }
    if fish_path.exists() {
        println!("fish integration: ok");
    } else {
        println!("fish integration: missing");
    }
    Ok(())
}

fn now_unix() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use tar::Builder;

    fn test_store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = Store::open(temp.path().join("aliases.db")).unwrap();
        (temp, store)
    }

    fn remote_alias(
        id: &str,
        collection: &str,
        name: &str,
        version: i64,
    ) -> (RemoteRecord, AliasPayload) {
        (
            RemoteRecord {
                id: id.to_owned(),
                record_type: "alias".to_owned(),
                encrypted_blob: String::new(),
                version,
            },
            AliasPayload {
                collection_id: collection_id_for_name(collection),
                collection_name: collection.to_owned(),
                name: name.to_owned(),
                command: "remote command".to_owned(),
                deleted: false,
                updated_at: 200,
            },
        )
    }

    #[test]
    fn moves_preserve_source_identity_and_destination_tombstone() {
        let (_temp, store) = test_store();
        store.create_collection("work").unwrap();
        store
            .upsert_in_collection("shared", "gs", "source")
            .unwrap();
        let source = store.list_all(Some("shared")).unwrap().remove(0);
        store
            .upsert_in_collection("work", "gs", "deleted destination")
            .unwrap();
        let destination = store.list_all(Some("work")).unwrap().remove(0);
        store.delete_alias(Some("work"), "gs").unwrap();
        store.move_alias("gs", "shared", "work").unwrap();
        let moved = store.list_all(Some("work")).unwrap().remove(0);
        assert_eq!(moved.id, source.id);
        assert_eq!(moved.command, "source");
        let tombstone = store.alias_by_id(&destination.id).unwrap().unwrap();
        assert!(tombstone.deleted && tombstone.dirty);
        assert_ne!(tombstone.name, "gs");
    }

    #[test]
    fn remote_move_updates_by_id_and_replays_without_duplicates() {
        let (_temp, store) = test_store();
        let (mut record, mut payload) = remote_alias("remote-1", "shared", "gs", 1);
        store.apply_remote_alias(&record, &payload).unwrap();
        store.create_collection("work").unwrap();
        store
            .upsert_in_collection("work", "gs", "deleted destination")
            .unwrap();
        let tombstone = store.list_all(Some("work")).unwrap().remove(0);
        store.delete_alias(Some("work"), "gs").unwrap();
        record.version = 2;
        payload.collection_id = collection_id_for_name("work");
        payload.collection_name = "work".to_owned();
        payload.updated_at = 201;
        assert!(store.apply_remote_alias(&record, &payload).unwrap());
        assert!(!store.apply_remote_alias(&record, &payload).unwrap());
        assert!(store.list_all(Some("shared")).unwrap().is_empty());
        assert_eq!(store.list_all(Some("work")).unwrap()[0].id, "remote-1");
        assert!(store.alias_by_id(&tombstone.id).unwrap().unwrap().deleted);
    }

    #[test]
    fn remote_name_collision_has_deterministic_winner_and_backup() {
        let (_temp, store) = test_store();
        let (record, payload) = remote_alias("a", "shared", "gs", 1);
        store.apply_remote_alias(&record, &payload).unwrap();
        let (winner, same_name) = remote_alias("z", "shared", "gs", 2);
        assert!(store.apply_remote_alias(&winner, &same_name).unwrap());
        assert_eq!(store.list_effective().unwrap()[0].id, "z");
        assert!(store.alias_by_id("a").unwrap().unwrap().deleted);
        let backups: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM conflict_backups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(backups, 1);
        assert!(!store.apply_remote_alias(&record, &payload).unwrap());
        assert_eq!(store.list_effective().unwrap()[0].id, "z");
    }

    #[test]
    fn invalid_import_and_activation_roll_back_prior_entries() {
        let (_temp, store) = test_store();
        let export = ExportFile {
            version: 2,
            collections: vec![ExportCollection {
                name: "work".to_owned(),
            }],
            aliases: vec![
                ExportAlias {
                    name: "ok".to_owned(),
                    command: "true".to_owned(),
                    collection: "work".to_owned(),
                    metadata: Metadata::default(),
                },
                ExportAlias {
                    name: "invalid!".to_owned(),
                    command: "false".to_owned(),
                    collection: "work".to_owned(),
                    metadata: Metadata::default(),
                },
            ],
        };
        assert!(store.import(&export).is_err());
        assert!(store.collection_by_name("work").is_err());
        assert!(store.list_all(None).unwrap().is_empty());
        store.create_collection("work").unwrap();
        assert!(
            store
                .activate_collections(&["work".to_owned(), "missing".to_owned()])
                .is_err()
        );
        assert!(!store.collection_by_name("work").unwrap().active);
    }

    #[test]
    fn upload_ack_does_not_clear_edits_made_after_snapshot() {
        let (_temp, store) = test_store();
        store.upsert_in_collection("shared", "gs", "old").unwrap();
        let snapshot = store.pending_aliases().unwrap().remove(0);
        store.upsert_in_collection("shared", "gs", "new").unwrap();
        store.mark_alias_synced(&snapshot, 7).unwrap();
        assert_eq!(store.pending_aliases().unwrap()[0].command, "new");
        let own_upload = RemoteRecord {
            id: snapshot.id.clone(),
            record_type: "alias".to_owned(),
            encrypted_blob: String::new(),
            version: 7,
        };
        let old_payload = AliasPayload {
            collection_id: snapshot.collection_id.clone(),
            collection_name: snapshot.collection_name.clone(),
            name: snapshot.name.clone(),
            command: snapshot.command.clone(),
            deleted: snapshot.deleted,
            updated_at: snapshot.updated_at,
        };
        assert!(!store.apply_remote_alias(&own_upload, &old_payload).unwrap());
        assert_eq!(store.pending_aliases().unwrap()[0].command, "new");
        assert_eq!(store.pending_aliases().unwrap()[0].sync_version, 7);
        store.create_collection("work").unwrap();
        let snapshot = store.pending_collections().unwrap().remove(0);
        store
            .conn
            .execute(
                "UPDATE collections SET deleted = 1 WHERE id = ?1",
                [&snapshot.id],
            )
            .unwrap();
        store.mark_collection_synced(&snapshot, 8).unwrap();
        assert!(!store.pending_collections().unwrap().is_empty());
        let own_collection_upload = RemoteRecord {
            id: snapshot.id.clone(),
            record_type: "collection".to_owned(),
            encrypted_blob: String::new(),
            version: 8,
        };
        let old_collection_payload = CollectionPayload {
            name: snapshot.name.clone(),
            deleted: snapshot.deleted,
            updated_at: snapshot.updated_at,
        };
        assert!(
            !store
                .apply_remote_collection(&own_collection_upload, &old_collection_payload)
                .unwrap()
        );
        assert!(store.pending_collections().unwrap()[0].deleted);
    }

    #[test]
    fn rejects_shell_reserved_aliases_but_keeps_old_records_removable() {
        let (_temp, store) = test_store();
        for name in ["-x", "aliaz", "__aliaz_cleanup", "__aliaz_bin"] {
            assert!(store.upsert_in_collection("shared", name, "true").is_err());
            store.conn.execute("INSERT INTO aliases (id, collection_id, name, command, created_at, updated_at) VALUES (?1, ?2, ?1, 'true', 1, 1)", params![name, DEFAULT_COLLECTION_ID]).unwrap();
            assert!(store.delete_alias(Some("shared"), name).is_ok());
        }
        assert!(
            store
                .upsert_in_collection("shared", "foo-bar", "true")
                .is_ok()
        );
    }

    fn mock_sync_server(
        count: usize,
        mut reply: impl FnMut(usize, &str, serde_json::Value) -> serde_json::Value + Send + 'static,
    ) -> (String, thread::JoinHandle<()>) {
        use std::io::Read;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            for index in 0..count {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "missing fixture request"
                            );
                            thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(e) => panic!("fixture accept failed: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let body = if bytes.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                let response = reply(index, &path, body).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            }
        });
        (url, handle)
    }

    const TEST_PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn sync_keeps_pull_cursor_to_receive_update_between_pull_and_push() {
        let (_temp, store) = test_store();
        store
            .upsert_in_collection("shared", "local", "local command")
            .unwrap();
        let (_, payload) = remote_alias("other-device", "shared", "remote", 11);
        let encrypted = encrypt_record(&derive_key(TEST_PHRASE).unwrap(), &payload).unwrap();
        let (url, server) = mock_sync_server(3, move |index, path, body| match index {
            0 => {
                assert_eq!(path, "/v1/records?after=0");
                serde_json::json!({ "latest_version":10, "records":[] })
            }
            1 => {
                assert_eq!(path, "/v1/records");
                serde_json::json!({ "latest_version":12, "records":[{ "id":body["records"][0]["id"], "version":12 }] })
            }
            _ => {
                assert_eq!(path, "/v1/records?after=10");
                serde_json::json!({ "latest_version":12, "records":[{ "id":"other-device", "record_type":"alias", "encrypted_blob":encrypted, "version":11 }] })
            }
        });
        let mut config = SyncConfig {
            sync_url: url,
            username: "testuser".to_owned(),
            user_id: "fixture".to_owned(),
            token: "fixture-only".to_owned(),
            latest_version: 0,
        };
        sync_aliases(&store, &mut config, TEST_PHRASE).unwrap();
        assert_eq!(config.latest_version, 10);
        sync_aliases(&store, &mut config, TEST_PHRASE).unwrap();
        server.join().unwrap();
        assert_eq!(store.list_effective().unwrap().len(), 2);
    }

    #[test]
    fn sync_chunks_more_than_five_hundred_pending_records() {
        let (_temp, store) = test_store();
        for index in 0..501 {
            store
                .upsert_in_collection("shared", &format!("a{index}"), "true")
                .unwrap();
        }
        let (url, server) = mock_sync_server(3, |index, _, body| {
            if index == 0 {
                return serde_json::json!({ "latest_version":0, "records":[] });
            }
            let records = body["records"].as_array().unwrap();
            assert_eq!(records.len(), if index == 1 { 500 } else { 1 });
            serde_json::json!({ "latest_version":501, "records":records.iter().enumerate().map(|(i, record)| serde_json::json!({ "id":record["id"], "version":i + index * 500 })).collect::<Vec<_>>() })
        });
        let mut config = SyncConfig {
            sync_url: url,
            username: "testuser".to_owned(),
            user_id: "fixture".to_owned(),
            token: "fixture-only".to_owned(),
            latest_version: 0,
        };
        let result = sync_aliases(&store, &mut config, TEST_PHRASE).unwrap();
        server.join().unwrap();
        assert_eq!(result.pushed, 501);
        assert!(store.pending_aliases().unwrap().is_empty());
    }

    #[test]
    fn encryption_round_trips_collection_scoped_alias_payload() {
        let key = derive_key("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let payload = AliasPayload {
            collection_id: DEFAULT_COLLECTION_ID.to_owned(),
            collection_name: DEFAULT_COLLECTION_NAME.to_owned(),
            name: "gs".to_owned(),
            command: "git status".to_owned(),
            deleted: false,
            updated_at: 123,
        };

        let encrypted = encrypt_record(&key, &payload).unwrap();
        assert_ne!(encrypted, serde_json::to_string(&payload).unwrap());
        let decrypted: AliasPayload = decrypt_record(&key, &encrypted).unwrap();

        assert_eq!(decrypted.collection_id, DEFAULT_COLLECTION_ID);
        assert_eq!(decrypted.collection_name, DEFAULT_COLLECTION_NAME);
        assert_eq!(decrypted.name, "gs");
        assert_eq!(decrypted.command, "git status");
        assert!(!decrypted.deleted);
        assert_eq!(decrypted.updated_at, 123);
    }

    #[test]
    fn old_alias_payloads_default_to_shared_collection() {
        let json = r#"{"name":"gs","command":"git status","deleted":false,"updated_at":123}"#;
        let payload: AliasPayload = serde_json::from_str(json).unwrap();

        assert_eq!(payload.collection_id, DEFAULT_COLLECTION_ID);
        assert_eq!(payload.collection_name, DEFAULT_COLLECTION_NAME);
    }

    #[test]
    fn collection_csv_parses_names() {
        assert_eq!(
            parse_collection_csv(Some("mac, development ,arch")).unwrap(),
            vec![
                "mac".to_owned(),
                "development".to_owned(),
                "arch".to_owned()
            ]
        );
        assert!(parse_collection_csv(Some("bad name")).is_err());
        assert!(parse_collection_csv(None).unwrap().is_empty());
    }

    #[test]
    fn login_config_starts_from_zero_to_force_initial_pull() {
        let response = AccountResponse {
            user_id: "user-1".to_owned(),
            token: "token-1".to_owned(),
            latest_version: 42,
        };

        let config = login_config("https://sync.example", "testuser", response);

        assert_eq!(config.latest_version, 0);
    }

    #[test]
    fn sync_config_json_does_not_contain_recovery_phrase() {
        let config = SyncConfig {
            sync_url: "https://sync.example".to_owned(),
            username: "testuser".to_owned(),
            user_id: "user-1".to_owned(),
            token: "token-1".to_owned(),
            latest_version: 7,
        };

        let json = serde_json::to_string(&config).unwrap();

        assert!(!json.contains("recovery_phrase"));
        assert!(!json.contains("abandon"));
    }

    fn release_archive_bytes(contents: &[u8]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_path("aliaz").unwrap();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, "aliaz", contents).unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn spawn_release_server(
        checksums: Vec<u8>,
        archive: Vec<u8>,
        archive_path: String,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (status, body, content_type) = if path == "/checksums.txt" {
                    ("200 OK", checksums.clone(), "text/plain")
                } else if path == archive_path {
                    ("200 OK", archive.clone(), "application/gzip")
                } else {
                    ("404 Not Found", b"not found".to_vec(), "text/plain")
                };

                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });

        (base_url, handle)
    }

    #[test]
    fn update_release_binary_downloads_and_installs_the_latest_release() {
        let temp = tempfile::TempDir::new().unwrap();
        let destination = temp.path().join("aliaz");
        fs::write(&destination, b"old-binary").unwrap();

        let target = release_target().unwrap();
        let archive_name = format!("aliaz-{target}.tar.gz");
        let archive = release_archive_bytes(b"new-binary");
        let checksum = format!("{:x}", Sha256::digest(&archive));
        let checksums = format!("{checksum}  {archive_name}\n").into_bytes();
        let (base_url, handle) =
            spawn_release_server(checksums, archive, format!("/{archive_name}"));

        update_release_binary_from_base_url(&destination, &base_url).unwrap();
        handle.join().unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new-binary");
    }

    #[test]
    fn pull_keeps_losing_local_command_details_together_in_the_conflict_backup() {
        let (_temp, store) = test_store();
        store
            .upsert_in_collection("shared", "same", "local command")
            .unwrap();
        store
            .change_metadata(
                "shared",
                "same",
                Some("local purpose".into()),
                Some(vec!["local".into()]),
                Some(true),
            )
            .unwrap();
        let alias = store.list_effective().unwrap().remove(0);
        store
            .conn
            .execute(
                "UPDATE aliases SET updated_at = 1 WHERE id = ?1",
                [&alias.id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE alias_metadata SET updated_at = 1 WHERE alias_id = ?1",
                [&alias.id],
            )
            .unwrap();
        let key = derive_key(TEST_PHRASE).unwrap();
        let command = AliasPayload {
            collection_id: alias.collection_id.clone(),
            collection_name: "shared".into(),
            name: "same".into(),
            command: "remote command".into(),
            deleted: false,
            updated_at: 10,
        };
        let details = library::MetadataPayload {
            alias_id: alias.id.clone(),
            metadata: Metadata {
                description: "remote purpose".into(),
                tags: vec!["remote".into()],
                favorite: false,
            },
            updated_at: 10,
        };
        let records = serde_json::json!([
            { "id": library::metadata_record_id(&alias.id), "record_type": "alias_metadata", "encrypted_blob": encrypt_record(&key, &details).unwrap(), "version": 2 },
            { "id": alias.id, "record_type": "alias", "encrypted_blob": encrypt_record(&key, &command).unwrap(), "version": 3 }
        ]);
        let (url, server) = mock_sync_server(
            1,
            move |_, _, _| serde_json::json!({ "latest_version": 3, "records": records }),
        );
        let mut config = SyncConfig {
            sync_url: url,
            username: "testuser".into(),
            user_id: "fixture".into(),
            token: "fixture-only".into(),
            latest_version: 0,
        };
        sync_aliases(&store, &mut config, TEST_PHRASE).unwrap();
        server.join().unwrap();
        let snapshot: String = store
            .conn
            .query_row("SELECT snapshot FROM conflict_backups", [], |row| {
                row.get(0)
            })
            .unwrap();
        let snapshot: Alias = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(snapshot.command, "local command");
        assert_eq!(snapshot.metadata.description, "local purpose");
        assert_eq!(snapshot.metadata.tags, vec!["local"]);
        assert!(snapshot.metadata.favorite);
        assert_eq!(store.metadata(&snapshot.id).unwrap(), details.metadata);
    }
}

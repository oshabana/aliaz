use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bip39::{Language, Mnemonic};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use dialoguer::{FuzzySelect, theme::ColorfulTheme};
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
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tar::Archive;
use uuid::Uuid;

const DEFAULT_SYNC_URL: &str = "https://aliaz-sync.still-silence-6a39.workers.dev";
const KEYRING_SERVICE: &str = "dev.aliaz.cli";
const DEFAULT_COLLECTION_NAME: &str = "shared";
const DEFAULT_COLLECTION_ID: &str = "collection:shared";

#[derive(Parser)]
#[command(name = "aliaz")]
#[command(about = "Manage shell aliases from a local SQLite-backed source of truth")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Add {
        name: String,
        command: String,
        #[arg(long, default_value = DEFAULT_COLLECTION_NAME)]
        collection: String,
    },
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Pick an alias from a searchable list and run it.
    Select {
        /// Print the selected command instead of running it.
        #[arg(long, hide = true)]
        print_command: bool,
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
    },
    Register {
        #[arg(long)]
        username: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        collections: Option<String>,
        #[arg(long, default_value = DEFAULT_SYNC_URL)]
        sync_url: String,
    },
    Login {
        #[arg(long)]
        username: String,
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

#[derive(Debug, PartialEq, Eq)]
struct Alias {
    id: String,
    collection_id: String,
    collection_name: String,
    name: String,
    command: String,
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
struct PushRequest {
    records: Vec<UploadRecord>,
}

#[derive(Debug, Deserialize)]
struct PushResponse {
    latest_version: i64,
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

    let cli = Cli::parse();
    let db_path = database_path()?;
    let store = Store::open(db_path)?;

    match cli.command {
        Commands::Add {
            name,
            command,
            collection,
        } => {
            store.upsert_in_collection(&collection, &name, &command)?;
            println!("Added {name} to {collection}");
        }
        Commands::List { all, collection } => {
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
            first,
            query,
        } => {
            select_alias(&store, &query, print_command, first)?;
        }
        Commands::Rm { name, collection } => {
            let collection_name = store.delete_alias(collection.as_deref(), &name)?;
            println!("Deleted {name} from {collection_name}");
        }
        Commands::Edit {
            name,
            command,
            collection,
        } => {
            let collection_name = store.update_alias(collection.as_deref(), &name, &command)?;
            println!("Updated {name} in {collection_name}");
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
        Commands::Migrate { from } => {
            let path = from.unwrap_or_else(default_zshrc_path);
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let aliases = parse_aliases(&contents)?;
            let count = aliases.len();
            for alias in aliases {
                store.upsert_in_collection(DEFAULT_COLLECTION_NAME, &alias.name, &alias.command)?;
            }
            println!("Imported {count} aliases");
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
        Commands::Export { output } => {
            let aliases = store.list_all(None)?;
            let collections = store.collections()?;
            let export = ExportFile {
                version: 2,
                collections: collections
                    .iter()
                    .map(|collection| ExportCollection {
                        name: collection.name.clone(),
                    })
                    .collect(),
                aliases: aliases
                    .iter()
                    .map(|alias| ExportAlias {
                        name: alias.name.clone(),
                        command: alias.command.clone(),
                        collection: alias.collection_name.clone(),
                    })
                    .collect(),
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
        Commands::Import { path } => {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let export: ExportFile = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            if !matches!(export.version, 1 | 2) {
                bail!("unsupported export version: {}", export.version);
            }
            if export.version >= 2 {
                for collection in &export.collections {
                    store.create_collection(&collection.name)?;
                }
            }
            let count = export.aliases.len();
            for alias in export.aliases {
                store.upsert_in_collection(&alias.collection, &alias.name, &alias.command)?;
            }
            println!("Imported {count} aliases");
        }
        Commands::Register {
            username,
            password,
            collections,
            sync_url,
        } => {
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
            create_and_activate_collections(&store, &collections)?;
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
            let password = prompt_secret_if_missing(password, "Password: ")?;
            let recovery_phrase = prompt_secret_if_missing(recovery_phrase, "Recovery phrase: ")?;
            Mnemonic::parse_in_normalized(Language::English, &recovery_phrase)
                .context("invalid recovery phrase")?;
            let response = account_request(&sync_url, "login", &username, &password)?;
            store_recovery_phrase(&response.user_id, &recovery_phrase)?;
            let mut config = login_config(&sync_url, &username, response);
            let _ = sync_aliases(&store, &mut config, &recovery_phrase)?;
            save_config(&config)?;
            let collections = match collections {
                Some(value) => parse_collection_csv(Some(&value))?,
                None => prompt_for_collection_selection(&store)?,
            };
            store.activate_collections(&collections)?;
            println!("Logged in {username}");
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
            let result = sync_aliases(&store, &mut config, &recovery_phrase)?;
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

fn select_alias(store: &Store, query: &[String], print_command: bool, first: bool) -> Result<()> {
    let aliases = store.list_effective()?;
    if aliases.is_empty() {
        bail!("no aliases found");
    }

    let query = query.join(" ");
    let selected_index = if first {
        Some(
            first_matching_alias(&aliases, &query)
                .ok_or_else(|| anyhow!("no aliases matched: {query}"))?,
        )
    } else {
        if !io::stderr().is_terminal() {
            bail!("select requires an interactive terminal");
        }
        interactively_select_alias(&aliases, &query)?
    };

    let Some(selected_index) = selected_index else {
        return Ok(());
    };
    let command = &aliases[selected_index].command;

    if print_command {
        println!("{command}");
        return Ok(());
    }

    run_selected_command(command)
}

fn first_matching_alias(aliases: &[Alias], query: &str) -> Option<usize> {
    if query.trim().is_empty() {
        return Some(0);
    }

    let query = query.to_ascii_lowercase();
    aliases
        .iter()
        .position(|alias| alias_search_text(alias).contains(&query))
}

fn interactively_select_alias(aliases: &[Alias], query: &str) -> Result<Option<usize>> {
    let items = aliases.iter().map(alias_select_line).collect::<Vec<_>>();
    let theme = ColorfulTheme::default();
    FuzzySelect::with_theme(&theme)
        .with_prompt("Select alias")
        .items(&items)
        .with_initial_text(query)
        .default(0)
        .interact_opt()
        .context("failed to read alias selection")
}

fn alias_select_line(alias: &Alias) -> String {
    format!("{}\t{}", alias.name, alias.command)
}

fn alias_search_text(alias: &Alias) -> String {
    format!("{}\t{}", alias.name, alias.command).to_ascii_lowercase()
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
    fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(path)?;
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
        validate_name(name)?;
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
        validate_name(name)?;
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
        validate_name(name)?;
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

    fn apply_remote_alias(&self, record: &RemoteRecord, payload: &AliasPayload) -> Result<bool> {
        validate_name(&payload.name)?;
        validate_name(&payload.collection_name)?;
        self.ensure_remote_collection(&payload.collection_id, &payload.collection_name)?;
        let collection = self.collection_by_name(&payload.collection_name)?;
        if let Some(local) =
            self.find_alias_by_collection_and_name(&collection.id, &payload.name)?
        {
            if local.dirty && local.updated_at > payload.updated_at {
                return Ok(false);
            }
            if local.dirty && local.updated_at <= payload.updated_at {
                self.conn.execute(
                    "INSERT INTO conflict_backups (id, alias_name, command, remote_version, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        Uuid::new_v4().to_string(),
                        format!("{}/{}", local.collection_name, local.name),
                        local.command,
                        record.version,
                        now_unix()?
                    ],
                )?;
            }
        }

        self.conn.execute(
            "
            INSERT INTO aliases (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)
            ON CONFLICT(collection_id, name) DO UPDATE SET
                id = excluded.id,
                command = excluded.command,
                deleted = excluded.deleted,
                dirty = 0,
                sync_version = excluded.sync_version,
                updated_at = excluded.updated_at
            ",
            params![
                record.id,
                collection.id,
                payload.name,
                payload.command,
                payload.deleted as i64,
                record.version,
                now_unix()?,
                payload.updated_at
            ],
        )?;
        Ok(true)
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

    fn mark_collection_synced(&self, id: &str, version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE collections SET dirty = 0, sync_version = ?2 WHERE id = ?1",
            params![id, version],
        )?;
        Ok(())
    }

    fn mark_alias_synced(&self, id: &str, version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE aliases SET dirty = 0, sync_version = ?2 WHERE id = ?1",
            params![id, version],
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
                (SELECT COUNT(*) FROM aliases WHERE dirty = 1)
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
    if let Some(secret_home) = test_secret_home() {
        fs::create_dir_all(&secret_home)
            .with_context(|| format!("failed to create {}", secret_home.display()))?;
        fs::write(secret_home.join(secret_file_name(user_id)), recovery_phrase)
            .context("failed to store test recovery phrase")?;
        return Ok(());
    }

    Entry::new(KEYRING_SERVICE, user_id)?
        .set_password(recovery_phrase)
        .context("failed to store recovery phrase in OS credential store")?;
    Ok(())
}

fn load_recovery_phrase(user_id: &str) -> Result<String> {
    if let Some(secret_home) = test_secret_home() {
        return fs::read_to_string(secret_home.join(secret_file_name(user_id)))
            .context("recovery phrase is missing from test secret store");
    }

    Entry::new(KEYRING_SERVICE, user_id)?
        .get_password()
        .context("recovery phrase is missing from OS credential store")
}

fn remove_recovery_phrase(user_id: &str) -> Result<()> {
    if let Some(secret_home) = test_secret_home() {
        let path = secret_home.join(secret_file_name(user_id));
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
    }

    match Entry::new(KEYRING_SERVICE, user_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(_) => Ok(()),
    }
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
        Shell::Zsh => home_dir()?.join(".zshrc"),
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
    let startup_path = startup_path(shell)?;
    let integration_path = shell_integration_path(shell)?;
    let source_line = sh_source_command(&integration_path)?;
    let comment = "# Aliaz shell integration";

    let existing = match fs::read_to_string(&startup_path) {
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
        remove_file_if_exists(&startup_path)?;
        return Ok(true);
    }

    let mut contents = kept.join("\n");
    contents.push('\n');
    fs::write(&startup_path, contents)
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

fn test_secret_home() -> Option<PathBuf> {
    std::env::var_os("ALIAZ_TEST_SECRET_HOME").map(PathBuf::from)
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
    lines.extend(shell_alias_lines(shell, aliases));
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
    vec![
        "# Managed by Aliaz. Do not edit.".to_owned(),
        format!("__aliaz_bin={}", shell_quote(binary)),
        "aliaz() {".to_owned(),
        "  local __aliaz_status".to_owned(),
        "  local __aliaz_shell".to_owned(),
        "  local __aliaz_selected_command".to_owned(),
        "  if [ \"${1:-}\" = \"select\" ]; then".to_owned(),
        "    __aliaz_selected_command=$(\"$__aliaz_bin\" select --print-command \"${@:2}\")"
            .to_owned(),
        "    __aliaz_status=$?".to_owned(),
        "    if [ $__aliaz_status -eq 0 ] && [ -n \"$__aliaz_selected_command\" ]; then".to_owned(),
        "      eval \"$__aliaz_selected_command\"".to_owned(),
        "      return $?".to_owned(),
        "    fi".to_owned(),
        "    return $__aliaz_status".to_owned(),
        "  fi".to_owned(),
        "  \"$__aliaz_bin\" \"$@\"".to_owned(),
        "  __aliaz_status=$?".to_owned(),
        "  if [ $__aliaz_status -eq 0 ]; then".to_owned(),
        "    case \"${1:-}\" in".to_owned(),
        "      add|edit|rm|delete|collection|migrate|import|sync)".to_owned(),
        "        __aliaz_shell=\"\"".to_owned(),
        "        if [ -n \"${ZSH_VERSION:-}\" ]; then".to_owned(),
        "          __aliaz_shell=\"zsh\"".to_owned(),
        "        elif [ -n \"${BASH_VERSION:-}\" ]; then".to_owned(),
        "          __aliaz_shell=\"bash\"".to_owned(),
        "        fi".to_owned(),
        "        if [ -n \"$__aliaz_shell\" ]; then".to_owned(),
        format!(
            "          \"$__aliaz_bin\" init \"$__aliaz_shell\" >/dev/null && {}",
            source_command
        ),
        "        fi".to_owned(),
        "        ;;".to_owned(),
        "    esac".to_owned(),
        "  fi".to_owned(),
        "  return $__aliaz_status".to_owned(),
        "}".to_owned(),
        "".to_owned(),
    ]
}

fn fish_wrapper_lines(binary: &str, path: &Path) -> Vec<String> {
    vec![
        "# Managed by Aliaz. Do not edit.".to_owned(),
        format!("set -g __aliaz_bin {}", shell_quote(binary)),
        "function aliaz".to_owned(),
        "  if test (count $argv) -gt 0; and test $argv[1] = select".to_owned(),
        "    set -l __aliaz_selected_command (\"$__aliaz_bin\" select --print-command $argv[2..-1])".to_owned(),
        "    set -l __aliaz_status $status".to_owned(),
        "    if test $__aliaz_status -eq 0; and test -n \"$__aliaz_selected_command\"".to_owned(),
        "      eval \"$__aliaz_selected_command\"".to_owned(),
        "      return $status".to_owned(),
        "    end".to_owned(),
        "    return $__aliaz_status".to_owned(),
        "  end".to_owned(),
        "  \"$__aliaz_bin\" $argv".to_owned(),
        "  set -l __aliaz_status $status".to_owned(),
        "  if test $__aliaz_status -eq 0".to_owned(),
        "    switch $argv[1]".to_owned(),
        "      case add edit rm delete collection migrate import sync".to_owned(),
        "        \"$__aliaz_bin\" init fish >/dev/null".to_owned(),
        format!(
            "        source {}",
            shell_quote(&path.display().to_string())
        ),
        "    end".to_owned(),
        "  end".to_owned(),
        "  return $__aliaz_status".to_owned(),
        "end".to_owned(),
        "".to_owned(),
    ]
}

fn configure_startup_file(shell: &Shell, integration_path: &PathBuf) -> Result<PathBuf> {
    let startup_path = startup_path(shell)?;
    let source_line = sh_source_command(integration_path)?;
    let comment = "# Aliaz shell integration";
    let existing = match fs::read_to_string(&startup_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", startup_path.display()));
        }
    };
    if existing.contains(&source_line) {
        return Ok(startup_path);
    }

    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(comment);
    contents.push('\n');
    contents.push_str(&source_line);
    contents.push('\n');
    fs::write(&startup_path, contents)
        .with_context(|| format!("failed to write {}", startup_path.display()))?;
    Ok(startup_path)
}

fn shell_alias_lines(shell: &Shell, aliases: &[Alias]) -> Vec<String> {
    aliases
        .iter()
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
    for record in &pull.records {
        if record.record_type == "collection" {
            let payload: CollectionPayload = decrypt_record(&key, &record.encrypted_blob)?;
            if store.apply_remote_collection(record, &payload)? {
                pulled += 1;
            }
        }
    }
    for record in &pull.records {
        if record.record_type == "alias" {
            let payload: AliasPayload = decrypt_record(&key, &record.encrypted_blob)?;
            if store.apply_remote_alias(record, &payload)? {
                pulled += 1;
            }
        }
    }
    config.latest_version = pull.latest_version;

    let pending_collections = store.pending_collections()?;
    let pending_aliases = store.pending_aliases()?;
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

    let pushed = uploads.len();
    if pushed > 0 {
        let push_url = format!("{}/v1/records", config.sync_url.trim_end_matches('/'));
        let push: PushResponse = client
            .post(push_url)
            .bearer_auth(&config.token)
            .json(&PushRequest { records: uploads })
            .send()?
            .error_for_status()?
            .json()?;
        for record in push.records {
            if pending_collections
                .iter()
                .any(|collection| collection.id == record.id)
            {
                store.mark_collection_synced(&record.id, record.version)?;
            } else {
                store.mark_alias_synced(&record.id, record.version)?;
            }
        }
        config.latest_version = push.latest_version;
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

        let config = login_config("https://sync.example", "ada", response);

        assert_eq!(config.latest_version, 0);
    }

    #[test]
    fn sync_config_json_does_not_contain_recovery_phrase() {
        let config = SyncConfig {
            sync_url: "https://sync.example".to_owned(),
            username: "ada".to_owned(),
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
}

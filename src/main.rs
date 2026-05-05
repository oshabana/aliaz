use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bip39::{Language, Mnemonic};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use clap::{Parser, Subcommand, ValueEnum};
use flate2::read::GzDecoder;
use hkdf::Hkdf;
use keyring::Entry;
use rand::RngCore;
use reqwest::blocking::Client;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tar::Archive;
use uuid::Uuid;

const DEFAULT_SYNC_URL: &str = "https://aliaz-sync.still-silence-6a39.workers.dev";
const KEYRING_SERVICE: &str = "dev.aliaz.cli";

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
    },
    List,
    #[command(alias = "delete")]
    Rm {
        name: String,
    },
    Edit {
        name: String,
        command: String,
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
        #[arg(long, default_value = DEFAULT_SYNC_URL)]
        sync_url: String,
    },
    Key,
    Logout,
    Sync,
    Status,
    Doctor,
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
    name: String,
    command: String,
    deleted: bool,
    dirty: bool,
    sync_version: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportFile {
    version: u8,
    aliases: Vec<ExportAlias>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportAlias {
    name: String,
    command: String,
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
    name: String,
    command: String,
    deleted: bool,
    updated_at: i64,
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
    let cli = Cli::parse();
    let db_path = database_path()?;
    let store = Store::open(db_path)?;

    match cli.command {
        Commands::Add { name, command } => {
            store.upsert(&name, &command)?;
            println!("Added {name}");
        }
        Commands::List => {
            for alias in store.list()? {
                println!("{}\t{}", alias.name, alias.command);
            }
        }
        Commands::Rm { name } => {
            store.delete(&name)?;
            println!("Deleted {name}");
        }
        Commands::Edit { name, command } => {
            store.update(&name, &command)?;
            println!("Updated {name}");
        }
        Commands::Migrate { from } => {
            let path = from.unwrap_or_else(default_zshrc_path);
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let aliases = parse_aliases(&contents)?;
            let count = aliases.len();
            for alias in aliases {
                store.upsert(&alias.name, &alias.command)?;
            }
            println!("Imported {count} aliases");
        }
        Commands::Init { shell } => {
            let aliases = store.list()?;
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
            for line in shell_alias_lines(&shell, &store.list()?) {
                println!("{line}");
            }
        }
        Commands::Update => {
            update_release_binary(
                &std::env::current_exe().context("failed to locate current executable")?,
            )?;
            println!("Updated to latest release");
        }
        Commands::Export { output } => {
            let aliases = store.list()?;
            let export = ExportFile {
                version: 1,
                aliases: aliases
                    .iter()
                    .map(|alias| ExportAlias {
                        name: alias.name.clone(),
                        command: alias.command.clone(),
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
            if export.version != 1 {
                bail!("unsupported export version: {}", export.version);
            }
            let count = export.aliases.len();
            for alias in export.aliases {
                store.upsert(&alias.name, &alias.command)?;
            }
            println!("Imported {count} aliases");
        }
        Commands::Register {
            username,
            password,
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
            sync_url,
        } => {
            let password = prompt_secret_if_missing(password, "Password: ")?;
            let recovery_phrase = prompt_secret_if_missing(recovery_phrase, "Recovery phrase: ")?;
            Mnemonic::parse_in_normalized(Language::English, &recovery_phrase)
                .context("invalid recovery phrase")?;
            let response = account_request(&sync_url, "login", &username, &password)?;
            store_recovery_phrase(&response.user_id, &recovery_phrase)?;
            save_config(&login_config(&sync_url, &username, response))?;
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
            println!("pending sync records: {}", status.pending);
            if let Some(config) = load_config()? {
                println!("sync: configured for {}", config.username);
                println!("sync url: {}", config.sync_url);
                println!("latest sync version: {}", config.latest_version);
            } else {
                println!("sync: not configured");
            }
        }
        Commands::Doctor => {
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
        if let (Some(checksum), Some(asset)) = (checksum, asset) {
            if asset.trim_start_matches('*') == asset_name {
                return Ok(checksum.to_owned());
            }
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
            CREATE TABLE IF NOT EXISTS aliases (
                id TEXT NOT NULL UNIQUE,
                name TEXT PRIMARY KEY,
                command TEXT NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,
                dirty INTEGER NOT NULL DEFAULT 0,
                sync_version INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
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
        migrate_alias_table(&conn)?;

        Ok(Self { conn })
    }

    fn upsert(&self, name: &str, command: &str) -> Result<()> {
        validate_name(name)?;
        let now = now_unix()?;
        self.conn.execute(
            "
            INSERT INTO aliases (id, name, command, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, ?3, 0, 1, 0, ?4, ?4)
            ON CONFLICT(name) DO UPDATE SET
                command = excluded.command,
                deleted = 0,
                dirty = 1,
                updated_at = excluded.updated_at
            ",
            params![Uuid::new_v4().to_string(), name, command, now],
        )?;
        Ok(())
    }

    fn update(&self, name: &str, command: &str) -> Result<()> {
        validate_name(name)?;
        let changed = self.conn.execute(
            "UPDATE aliases SET command = ?2, deleted = 0, dirty = 1, updated_at = ?3 WHERE name = ?1 AND deleted = 0",
            params![name, command, now_unix()?],
        )?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let changed = self.conn.execute(
            "UPDATE aliases SET deleted = 1, dirty = 1, updated_at = ?2 WHERE name = ?1 AND deleted = 0",
            params![name, now_unix()?],
        )?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(())
    }

    fn list(&self) -> Result<Vec<Alias>> {
        self.aliases_where("deleted = 0")
    }

    fn pending(&self) -> Result<Vec<Alias>> {
        self.aliases_where("dirty = 1")
    }

    fn find_by_name(&self, name: &str) -> Result<Option<Alias>> {
        let mut aliases = self.aliases_where_with_param("name = ?1", name)?;
        Ok(aliases.pop())
    }

    fn apply_remote(&self, record: &RemoteRecord, payload: &AliasPayload) -> Result<bool> {
        validate_name(&payload.name)?;
        if let Some(local) = self.find_by_name(&payload.name)? {
            if local.dirty && local.updated_at > payload.updated_at {
                return Ok(false);
            }
            if local.dirty && local.updated_at <= payload.updated_at {
                self.conn.execute(
                    "INSERT INTO conflict_backups (id, alias_name, command, remote_version, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        Uuid::new_v4().to_string(),
                        local.name,
                        local.command,
                        record.version,
                        now_unix()?
                    ],
                )?;
            }
        }

        self.conn.execute(
            "
            INSERT INTO aliases (id, name, command, deleted, dirty, sync_version, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)
            ON CONFLICT(name) DO UPDATE SET
                id = excluded.id,
                command = excluded.command,
                deleted = excluded.deleted,
                dirty = 0,
                sync_version = excluded.sync_version,
                updated_at = excluded.updated_at
            ",
            params![
                record.id,
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

    fn mark_synced(&self, id: &str, version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE aliases SET dirty = 0, sync_version = ?2 WHERE id = ?1",
            params![id, version],
        )?;
        Ok(())
    }

    fn status(&self) -> Result<StoreStatus> {
        let aliases = self.conn.query_row(
            "SELECT COUNT(*) FROM aliases WHERE deleted = 0",
            [],
            |row| row.get(0),
        )?;
        let pending =
            self.conn
                .query_row("SELECT COUNT(*) FROM aliases WHERE dirty = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(StoreStatus { aliases, pending })
    }

    fn aliases_where(&self, clause: &str) -> Result<Vec<Alias>> {
        let sql = format!(
            "SELECT id, name, command, deleted, dirty, sync_version, updated_at FROM aliases WHERE {clause} ORDER BY name ASC"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let aliases = statement
            .query_map([], alias_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(aliases)
    }

    fn aliases_where_with_param(&self, clause: &str, value: &str) -> Result<Vec<Alias>> {
        let sql = format!(
            "SELECT id, name, command, deleted, dirty, sync_version, updated_at FROM aliases WHERE {clause} ORDER BY name ASC"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let aliases = statement
            .query_map([value], alias_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(aliases)
    }
}

fn alias_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Alias> {
    Ok(Alias {
        id: row.get(0)?,
        name: row.get(1)?,
        command: row.get(2)?,
        deleted: row.get::<_, i64>(3)? == 1,
        dirty: row.get::<_, i64>(4)? == 1,
        sync_version: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn migrate_alias_table(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn)?;
    if !columns.contains("id") {
        conn.execute("ALTER TABLE aliases ADD COLUMN id TEXT", [])?;
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
        "UPDATE aliases SET updated_at = ?1 WHERE updated_at IS NULL OR typeof(updated_at) = 'text'",
        params![now_unix()?],
    )?;

    Ok(())
}

fn table_columns(conn: &Connection) -> Result<HashSet<String>> {
    let mut statement = conn.prepare("PRAGMA table_info(aliases)")?;
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
    let path = match shell {
        Shell::Zsh | Shell::Bash => shell_config_home()?.join("aliaz").join("aliases.sh"),
        Shell::Fish => shell_config_home()?
            .join("fish")
            .join("conf.d")
            .join("aliaz.fish"),
    };

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
        "  \"$__aliaz_bin\" \"$@\"".to_owned(),
        "  __aliaz_status=$?".to_owned(),
        "  if [ $__aliaz_status -eq 0 ]; then".to_owned(),
        "    case \"${1:-}\" in".to_owned(),
        "      add|edit|rm|delete|migrate|import|sync)".to_owned(),
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

fn fish_wrapper_lines(binary: &str, path: &PathBuf) -> Vec<String> {
    vec![
        "# Managed by Aliaz. Do not edit.".to_owned(),
        format!("set -g __aliaz_bin {}", shell_quote(binary)),
        "function aliaz".to_owned(),
        "  \"$__aliaz_bin\" $argv".to_owned(),
        "  set -l __aliaz_status $status".to_owned(),
        "  if test $__aliaz_status -eq 0".to_owned(),
        "    switch $argv[1]".to_owned(),
        "      case add edit rm delete migrate import sync".to_owned(),
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
    let startup_path = match shell {
        Shell::Zsh => home_dir()?.join(".zshrc"),
        Shell::Bash => home_dir()?.join(".bashrc"),
        Shell::Fish => bail!("fish does not use a zsh/bash startup file"),
    };
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
        if record.record_type != "alias" {
            continue;
        }
        let payload = decrypt_alias(&key, &record.encrypted_blob)?;
        if store.apply_remote(record, &payload)? {
            pulled += 1;
        }
    }
    config.latest_version = pull.latest_version;

    let pending = store.pending()?;
    let mut uploads = Vec::with_capacity(pending.len());
    for alias in &pending {
        let payload = AliasPayload {
            name: alias.name.clone(),
            command: alias.command.clone(),
            deleted: alias.deleted,
            updated_at: alias.updated_at,
        };
        uploads.push(UploadRecord {
            id: alias.id.clone(),
            record_type: "alias".to_owned(),
            encrypted_blob: encrypt_alias(&key, &payload)?,
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
            store.mark_synced(&record.id, record.version)?;
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

fn encrypt_alias(key: &[u8; 32], payload: &AliasPayload) -> Result<String> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, serde_json::to_vec(payload)?.as_ref())
        .map_err(|_| anyhow!("failed to encrypt alias"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend(ciphertext);
    Ok(BASE64.encode(blob))
}

fn decrypt_alias(key: &[u8; 32], encrypted_blob: &str) -> Result<AliasPayload> {
    let blob = BASE64.decode(encrypted_blob)?;
    if blob.len() < 25 {
        bail!("encrypted alias blob is too short");
    }
    let (nonce_bytes, ciphertext) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow!("failed to decrypt alias"))?;
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
    fn encryption_round_trips_alias_payload() {
        let key = derive_key("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let payload = AliasPayload {
            name: "gs".to_owned(),
            command: "git status".to_owned(),
            deleted: false,
            updated_at: 123,
        };

        let encrypted = encrypt_alias(&key, &payload).unwrap();
        assert_ne!(encrypted, serde_json::to_string(&payload).unwrap());
        let decrypted = decrypt_alias(&key, &encrypted).unwrap();

        assert_eq!(decrypted.name, "gs");
        assert_eq!(decrypted.command, "git status");
        assert!(!decrypted.deleted);
        assert_eq!(decrypted.updated_at, 123);
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

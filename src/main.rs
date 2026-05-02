use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rusqlite::{Connection, params};
use std::fs;
use std::path::PathBuf;

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
}

#[derive(Clone, ValueEnum)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

#[derive(Debug, PartialEq, Eq)]
struct Alias {
    name: String,
    command: String,
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
                    println!(
                        "Wrote {}. Add this line to your {} startup file: source \"$HOME/.config/aliaz/aliases.sh\"",
                        path.display(),
                        shell.name()
                    );
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

struct Store {
    conn: Connection,
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
                name TEXT PRIMARY KEY,
                command TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            ",
        )?;

        Ok(Self { conn })
    }

    fn upsert(&self, name: &str, command: &str) -> Result<()> {
        validate_name(name)?;
        self.conn.execute(
            "
            INSERT INTO aliases (name, command)
            VALUES (?1, ?2)
            ON CONFLICT(name) DO UPDATE SET
                command = excluded.command,
                updated_at = CURRENT_TIMESTAMP
            ",
            params![name, command],
        )?;
        Ok(())
    }

    fn update(&self, name: &str, command: &str) -> Result<()> {
        validate_name(name)?;
        let changed = self.conn.execute(
            "UPDATE aliases SET command = ?2, updated_at = CURRENT_TIMESTAMP WHERE name = ?1",
            params![name, command],
        )?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let changed = self
            .conn
            .execute("DELETE FROM aliases WHERE name = ?1", params![name])?;
        if changed == 0 {
            bail!("alias not found: {name}");
        }
        Ok(())
    }

    fn list(&self) -> Result<Vec<Alias>> {
        let mut statement = self
            .conn
            .prepare("SELECT name, command FROM aliases ORDER BY name ASC")?;
        let aliases = statement
            .query_map([], |row| {
                Ok(Alias {
                    name: row.get(0)?,
                    command: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(aliases)
    }
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
                    name: name.to_owned(),
                    command: command.to_owned(),
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

impl Shell {
    fn name(&self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }
}

fn config_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("ALIAZ_CONFIG_HOME") {
        return Ok(PathBuf::from(home));
    }

    dirs::config_dir().ok_or_else(|| anyhow!("could not locate config directory"))
}

fn write_shell_integration(shell: &Shell, aliases: &[Alias]) -> Result<PathBuf> {
    let path = match shell {
        Shell::Zsh | Shell::Bash => config_home()?.join("aliaz").join("aliases.sh"),
        Shell::Fish => config_home()?
            .join("fish")
            .join("conf.d")
            .join("aliaz.fish"),
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut contents = shell_alias_lines(shell, aliases).join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
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

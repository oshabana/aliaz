use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Metadata {
    pub description: String,
    pub tags: Vec<String>,
    pub favorite: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryEntry {
    before: Option<Alias>,
    after: Alias,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MetadataPayload {
    pub alias_id: String,
    pub metadata: Metadata,
    pub updated_at: i64,
}

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alias_metadata (
            alias_id TEXT PRIMARY KEY, data TEXT NOT NULL, dirty INTEGER NOT NULL DEFAULT 0,
            sync_version INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS alias_usage (alias_id TEXT PRIMARY KEY, last_used INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS alias_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT, changes TEXT NOT NULL,
            created_at INTEGER NOT NULL, undone INTEGER NOT NULL DEFAULT 0);",
    )?;
    if !table_columns(conn, "conflict_backups")?.contains("snapshot") {
        conn.execute("ALTER TABLE conflict_backups ADD COLUMN snapshot TEXT", [])?;
    }
    if !table_columns(conn, "conflict_backups")?.contains("metadata_pending") {
        conn.execute(
            "ALTER TABLE conflict_backups ADD COLUMN metadata_pending INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

impl Store {
    pub(super) fn has_metadata(&self, id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM alias_metadata WHERE alias_id = ?1)",
            [id],
            |row| row.get(0),
        )?)
    }

    pub(super) fn metadata(&self, id: &str) -> Result<Metadata> {
        let data: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM alias_metadata WHERE alias_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| {
            serde_json::from_str(&data).context("saved command details could not be read")
        })
        .unwrap_or_else(|| Ok(Metadata::default()))
    }

    pub(super) fn write_metadata(&self, id: &str, metadata: &Metadata) -> Result<()> {
        self.conn.execute(
            "INSERT INTO alias_metadata (alias_id, data, dirty, updated_at) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(alias_id) DO UPDATE SET data = excluded.data, dirty = 1,
                updated_at = MAX(alias_metadata.updated_at + 1, excluded.updated_at)",
            params![id, serde_json::to_string(metadata)?, now_unix()?],
        )?;
        Ok(())
    }

    pub(super) fn change_metadata(
        &self,
        collection: &str,
        name: &str,
        description: Option<String>,
        tags: Option<Vec<String>>,
        favorite: Option<bool>,
    ) -> Result<()> {
        let scope = self.collection_by_name(collection)?;
        let alias = self
            .find_alias_by_collection_and_name(&scope.id, name)?
            .filter(|alias| !alias.deleted)
            .ok_or_else(|| anyhow!("alias not found: {name}"))?;
        let mut metadata = self.metadata(&alias.id)?;
        if let Some(description) = description {
            metadata.description = description;
        }
        if let Some(tags) = tags {
            metadata.tags = tags
                .into_iter()
                .map(|tag| tag.trim().to_owned())
                .filter(|tag| !tag.is_empty())
                .collect();
            metadata.tags.sort();
            metadata.tags.dedup();
        }
        if let Some(favorite) = favorite {
            metadata.favorite = favorite;
        }
        self.write_metadata(&alias.id, &metadata)
    }

    pub(super) fn check_replacement(
        &self,
        collection: &str,
        name: &str,
        command: &str,
        replace: bool,
    ) -> Result<()> {
        let scope = self.collection_by_name(collection)?;
        if let Some(old) = self
            .find_alias_by_collection_and_name(&scope.id, name)?
            .filter(|alias| !alias.deleted)
        {
            if old.command == command {
                return Ok(());
            }
            eprintln!(
                "Replace {} [{}]\nOld command:\n{}\nNew command:\n{}",
                name, collection, old.command, command
            );
            if !replace {
                bail!(
                    "command already exists; use --replace to keep this replacement, or choose another name"
                );
            }
        }
        Ok(())
    }

    pub(super) fn picker_aliases(&self) -> Result<Vec<Alias>> {
        let mut aliases = self.list_effective()?;
        let mut recent = BTreeMap::new();
        let mut query = self
            .conn
            .prepare("SELECT alias_id, last_used FROM alias_usage")?;
        for row in query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (id, time) = row?;
            recent.insert(id, time);
        }
        for alias in &mut aliases {
            alias.metadata = self.metadata(&alias.id)?;
        }
        aliases.sort_by(|a, b| {
            b.metadata
                .favorite
                .cmp(&a.metadata.favorite)
                .then_with(|| recent.get(&b.id).cmp(&recent.get(&a.id)))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(aliases)
    }

    pub(super) fn record_use(&self, id: &str) -> Result<()> {
        self.conn.execute("INSERT INTO alias_usage (alias_id, last_used) VALUES (?1, ?2)
            ON CONFLICT(alias_id) DO UPDATE SET last_used = MAX(alias_usage.last_used + 1, excluded.last_used)", params![id, now_unix()?])?;
        Ok(())
    }

    fn snapshots(&self) -> Result<BTreeMap<String, Alias>> {
        let mut statement = self.conn.prepare("SELECT a.id, a.collection_id, c.name, a.name, a.command,
            a.deleted, a.dirty, a.sync_version, a.updated_at FROM aliases a JOIN collections c ON c.id = a.collection_id")?;
        let mut aliases = BTreeMap::new();
        for row in statement.query_map([], alias_from_row)? {
            let mut alias = row?;
            alias.metadata = self.metadata(&alias.id)?;
            aliases.insert(alias.id.clone(), alias);
        }
        Ok(aliases)
    }

    pub(super) fn record_change<T>(
        &self,
        enabled: bool,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !enabled {
            return operation();
        }
        self.atomic(|| {
            let before = self.snapshots()?;
            let value = operation()?;
            let after = self.snapshots()?;
            let changes: Vec<HistoryEntry> = after
                .into_iter()
                .filter_map(|(id, after)| {
                    let old = before.get(&id);
                    if old.is_some_and(|before| same_content(before, &after)) {
                        None
                    } else {
                        Some(HistoryEntry {
                            before: old.cloned(),
                            after,
                        })
                    }
                })
                .collect();
            if !changes.is_empty() {
                self.conn.execute(
                    "INSERT INTO alias_history (changes, created_at) VALUES (?1, ?2)",
                    params![serde_json::to_string(&changes)?, now_unix()?],
                )?;
            }
            Ok(value)
        })
    }

    pub(super) fn print_history(&self) -> Result<()> {
        let mut statement = self
            .conn
            .prepare("SELECT id, changes, undone FROM alias_history ORDER BY id DESC")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?;
        let mut count = 0;
        for row in rows {
            let (id, changes, undone) = row?;
            let entries: Vec<HistoryEntry> = serde_json::from_str(&changes)?;
            println!("Change {id}{}", if undone { " (undone)" } else { "" });
            for entry in entries {
                let before = entry.before.as_ref().filter(|alias| !alias.deleted);
                let after = (!entry.after.deleted).then_some(&entry.after);
                println!(
                    "  {} [{}]\n  Old: {}\n  New: {}",
                    entry.after.name,
                    entry.after.collection_name,
                    before
                        .map(|alias| alias.command.as_str())
                        .unwrap_or("(absent)"),
                    after
                        .map(|alias| alias.command.as_str())
                        .unwrap_or("(deleted)")
                );
                println!(
                    "  Details: {} -> {}",
                    serde_json::to_string(&before.map(|alias| &alias.metadata))?,
                    serde_json::to_string(&after.map(|alias| &alias.metadata))?
                );
            }
            count += 1;
        }
        if count == 0 {
            println!("No saved changes yet");
        }
        Ok(())
    }

    pub(super) fn undo(&self, id: Option<i64>) -> Result<()> {
        self.atomic(|| {
            let change: Option<(i64, String)> = if let Some(id) = id {
                self.conn.query_row("SELECT id, changes FROM alias_history WHERE id = ?1 AND undone = 0", [id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?
            } else {
                self.conn.query_row("SELECT id, changes FROM alias_history WHERE undone = 0 ORDER BY id DESC LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?))).optional()?
            };
            let (id, changes) = change.ok_or_else(|| anyhow!("no change available to undo; inspect aliaz history"))?;
            let entries: Vec<HistoryEntry> = serde_json::from_str(&changes)?;
            let current = self.snapshots()?;
            for entry in &entries {
                if !current.get(&entry.after.id).is_some_and(|alias| same_content(alias, &entry.after)) {
                    bail!("{} [{}] changed since change {id}; undo its newer changes first", entry.after.name, entry.after.collection_name);
                }
            }
            let affected = entries.iter().map(|entry| entry.after.id.as_str()).collect::<HashSet<_>>();
            for entry in &entries {
                if let Some(before) = &entry.before
                    && let Some(other) = self.find_alias_by_collection_and_name(&before.collection_id, &before.name)?
                    && other.id != before.id && !other.deleted && !affected.contains(other.id.as_str()) {
                    bail!("cannot restore {}: that name is now used in {}", before.name, before.collection_name);
                }
            }
            // A move and its displaced tombstone can exchange slots in one undo group.
            for entry in &entries {
                self.conn.execute("UPDATE aliases SET name = ?2 WHERE id = ?1", params![entry.after.id, format!("__aliaz_undo_{}", Uuid::new_v4().simple())])?;
            }
            for entry in entries {
                if let Some(before) = entry.before {
                    self.conn.execute("UPDATE collections SET deleted = 0, dirty = 1, updated_at = MAX(updated_at + 1, ?2) WHERE id = ?1 AND deleted = 1", params![before.collection_id, now_unix()?])?;
                    self.vacate_other_tombstone(&before.collection_id, &before.name, &before.id)?;
                    self.conn.execute("UPDATE aliases SET collection_id = ?2, name = ?3, command = ?4,
                        deleted = ?5, dirty = 1, updated_at = MAX(updated_at + 1, ?6) WHERE id = ?1",
                        params![before.id, before.collection_id, before.name, before.command, before.deleted, now_unix()?])?;
                    self.write_metadata(&before.id, &before.metadata)?;
                } else {
                    self.conn.execute("UPDATE aliases SET name = ?3, deleted = 1, dirty = 1, updated_at = MAX(updated_at + 1, ?2) WHERE id = ?1", params![entry.after.id, now_unix()?, format!("__aliaz_deleted_{}", entry.after.id)])?;
                }
            }
            self.conn.execute("UPDATE alias_history SET undone = 1 WHERE id = ?1", [id])?;
            println!("Undid change {id}");
            Ok(())
        })
    }

    fn vacate_other_tombstone(&self, collection: &str, name: &str, id: &str) -> Result<()> {
        if let Some(other) = self.find_alias_by_collection_and_name(collection, name)?
            && other.id != id
        {
            self.vacate_alias_slot(collection, name)?;
        }
        Ok(())
    }

    pub(super) fn print_conflicts(&self) -> Result<()> {
        let mut statement = self.conn.prepare("SELECT id, alias_name, command, snapshot FROM conflict_backups ORDER BY created_at DESC, id")?;
        let mut count = 0;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (id, label, command, snapshot) = row?;
            println!("{id}\t{label}\n{command}");
            if snapshot.is_none() {
                println!("Collection not recorded: restore requires --collection and --name");
            }
            count += 1;
        }
        if count == 0 {
            println!("No saved conflicts");
        }
        Ok(())
    }

    pub(super) fn restore_conflict(
        &self,
        id: &str,
        collection: Option<&str>,
        name: Option<&str>,
        replace: bool,
    ) -> Result<()> {
        let copy = collection.is_some() || name.is_some();
        let (command, snapshot): (String, Option<String>) = self
            .conn
            .query_row(
                "SELECT command, snapshot FROM conflict_backups WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("conflict not found: {id}"))?;
        let snapshot: Option<Alias> = snapshot
            .map(|value| serde_json::from_str(&value))
            .transpose()?;
        let collection = collection
            .or_else(|| {
                snapshot
                    .as_ref()
                    .map(|alias| alias.collection_name.as_str())
            })
            .ok_or_else(|| {
                anyhow!("older backup has no collection identity; pass --collection and --name")
            })?;
        let name = name
            .or_else(|| snapshot.as_ref().map(|alias| alias.name.as_str()))
            .ok_or_else(|| {
                anyhow!("older backup has no alias identity; pass --collection and --name")
            })?;
        validate_alias_name(name)?;
        validate_command(&command)?;
        self.atomic(|| {
            self.check_replacement(collection, name, &command, replace)?;
            if let Some(snapshot) = &snapshot {
                if !copy {
                    let scope = self.collection_by_name(collection)?;
                    if scope.id != snapshot.collection_id { bail!("original collection identity changed; choose --collection and --name to copy this backup"); }
                    if let Some(target) = self.find_alias_by_collection_and_name(&scope.id, name)?
                        && target.id != snapshot.id {
                            if !target.deleted {
                                if !replace { bail!("another command uses this name; use --replace to restore the original command identity"); }
                                self.backup_alias(&target, target.sync_version)?;
                            }
                            self.conn.execute("UPDATE aliases SET name = ?2, deleted = 1, dirty = 1, updated_at = MAX(updated_at + 1, ?3) WHERE id = ?1",
                                params![target.id, format!("__aliaz_deleted_{}", target.id), now_unix()?])?;
                    }
                    if let Some(current) = self.alias_by_id(&snapshot.id)?.filter(|alias| !alias.deleted)
                        && (current.collection_id != snapshot.collection_id || current.name != snapshot.name) {
                        if !replace { bail!("original command was moved; use --replace to restore it to its saved collection"); }
                        self.backup_alias(&current, current.sync_version)?;
                    }
                    self.conn.execute("INSERT INTO aliases (id, collection_id, name, command, deleted, dirty, sync_version, created_at, updated_at)
                        VALUES (?1, ?2, ?3, ?4, 0, 1, 0, ?5, ?5) ON CONFLICT(id) DO UPDATE SET
                        collection_id = excluded.collection_id, name = excluded.name, command = excluded.command,
                        deleted = 0, dirty = 1, updated_at = MAX(aliases.updated_at + 1, excluded.updated_at)",
                        params![snapshot.id, snapshot.collection_id, name, command, now_unix()?])?;
                } else { self.upsert_in_collection(collection, name, &command)?; }
                self.change_metadata(collection, name, Some(snapshot.metadata.description.clone()), Some(snapshot.metadata.tags.clone()), Some(snapshot.metadata.favorite))?;
            } else { self.upsert_in_collection(collection, name, &command)?; }
            Ok(())
        })?;
        println!("Restored {name} in {collection}. Undo: aliaz undo");
        Ok(())
    }

    pub(super) fn pending_metadata(&self) -> Result<Vec<MetadataPayload>> {
        let mut statement = self.conn.prepare("SELECT alias_id, data, updated_at FROM alias_metadata WHERE dirty = 1 ORDER BY alias_id")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (alias_id, data, updated_at) = row?;
            Ok(MetadataPayload {
                alias_id,
                metadata: serde_json::from_str(&data)?,
                updated_at,
            })
        })
        .collect()
    }

    pub(super) fn apply_remote_metadata(
        &self,
        record: &RemoteRecord,
        payload: &MetadataPayload,
    ) -> Result<bool> {
        if record.id != metadata_record_id(&payload.alias_id) {
            bail!("command details have an invalid identity");
        }
        let changed = self.conn.execute(
            "INSERT INTO alias_metadata (alias_id, data, dirty, sync_version, updated_at)
            VALUES (?1, ?2, 0, ?3, ?4) ON CONFLICT(alias_id) DO UPDATE SET data = excluded.data,
                dirty = 0, sync_version = excluded.sync_version, updated_at = excluded.updated_at
            WHERE alias_metadata.sync_version < excluded.sync_version
              AND (alias_metadata.dirty = 0 OR alias_metadata.updated_at <= excluded.updated_at)",
            params![
                payload.alias_id,
                serde_json::to_string(&payload.metadata)?,
                record.version,
                payload.updated_at
            ],
        )?;
        let pending = {
            let mut statement = self.conn.prepare("SELECT id, snapshot FROM conflict_backups WHERE metadata_pending = 1 AND snapshot IS NOT NULL")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, snapshot) in pending {
            let mut snapshot: Alias = serde_json::from_str(&snapshot)?;
            if snapshot.id == payload.alias_id {
                snapshot.metadata = payload.metadata.clone();
                self.conn.execute(
                    "UPDATE conflict_backups SET snapshot = ?2, metadata_pending = 0 WHERE id = ?1",
                    params![id, serde_json::to_string(&snapshot)?],
                )?;
            }
        }
        Ok(changed > 0)
    }

    pub(super) fn mark_metadata_synced(
        &self,
        snapshot: &MetadataPayload,
        version: i64,
    ) -> Result<()> {
        self.conn.execute("UPDATE alias_metadata SET dirty = CASE WHEN data = ?2 AND updated_at = ?3 THEN 0 ELSE dirty END,
            sync_version = MAX(sync_version, ?4) WHERE alias_id = ?1", params![snapshot.alias_id, serde_json::to_string(&snapshot.metadata)?, snapshot.updated_at, version])?;
        Ok(())
    }
}

pub(super) fn metadata_record_id(alias_id: &str) -> String {
    format!("metadata:{:x}", Sha256::digest(alias_id.as_bytes()))
}

fn same_content(a: &Alias, b: &Alias) -> bool {
    a.id == b.id
        && a.collection_id == b.collection_id
        && a.name == b.name
        && a.command == b.command
        && a.deleted == b.deleted
        && a.metadata == b.metadata
}

pub(super) fn save(
    store: &Store,
    last: bool,
    command: Option<String>,
    name: Option<String>,
    collection: Option<String>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("save needs an interactive terminal; use aliaz add NAME COMMAND for scripts");
    }
    let initial = if last {
        std::env::var("ALIAZ_LAST_COMMAND")
            .context("last command is unavailable; run aliaz init for your shell, then reload it")?
    } else {
        command.unwrap_or_default()
    };
    if last && initial.trim().is_empty() {
        bail!("no previous command is available");
    }
    let command = if last {
        eprintln!("Command to save:\n{initial}");
        if Confirm::new()
            .with_prompt("Edit command before saving?")
            .default(false)
            .interact()?
        {
            Editor::new()
                .trim_newlines(false)
                .edit(&initial)?
                .ok_or_else(|| anyhow!("save cancelled"))?
        } else {
            initial
        }
    } else {
        Editor::new()
            .trim_newlines(false)
            .edit(&initial)?
            .ok_or_else(|| anyhow!("save cancelled"))?
    };
    if command.trim().is_empty() {
        bail!("command is empty; nothing saved");
    }
    let name = match name {
        Some(name) => name,
        None => Input::<String>::new().with_prompt("Name").interact_text()?,
    };
    validate_alias_name(&name)?;
    let description = Input::<String>::new()
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;
    let tags = Input::<String>::new()
        .with_prompt("Tags (comma-separated, optional)")
        .allow_empty(true)
        .interact_text()?;
    let collection = match collection {
        Some(collection) => collection,
        None => {
            let names = store
                .collections()?
                .into_iter()
                .map(|collection| collection.name)
                .collect::<Vec<_>>();
            let index = Select::new()
                .with_prompt("Collection")
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };
    let scope = store.collection_by_name(&collection)?;
    if let Some(old) = store
        .find_alias_by_collection_and_name(&scope.id, &name)?
        .filter(|alias| !alias.deleted)
    {
        eprintln!(
            "Replace {name} [{collection}]\nOld command:\n{}\nNew command:\n{command}",
            old.command
        );
        if !Confirm::new()
            .with_prompt("Replace saved command?")
            .default(false)
            .interact()?
        {
            return Ok(());
        }
    }
    store.upsert_in_collection(&collection, &name, &command)?;
    store.change_metadata(
        &collection,
        &name,
        Some(description),
        Some(tags.split(',').map(str::to_owned).collect()),
        None,
    )?;
    println!("Saved {name} in {collection}. Undo: aliaz undo");
    Ok(())
}

pub(super) fn import_with_preview(
    store: &Store,
    export: &ExportFile,
    preview: bool,
    yes: bool,
    replace: bool,
) -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    // Validate the entire file before displaying an applicable preview.
    for collection in &export.collections {
        validate_name(&collection.name)?;
    }
    let mut seen = HashSet::new();
    for alias in &export.aliases {
        validate_alias_name(&alias.name)?;
        validate_command(&alias.command)?;
        validate_name(&alias.collection)?;
        if !seen.insert((&alias.collection, &alias.name)) {
            bail!(
                "duplicate command in import: {} [{}]",
                alias.name,
                alias.collection
            );
        }
    }
    let existing = store.list_all(None)?;
    let mut additions = 0;
    let mut replacements = 0;
    let mut skipped = 0;
    for alias in &export.aliases {
        let old = existing
            .iter()
            .find(|old| old.name == alias.name && old.collection_name == alias.collection);
        match old {
            None => {
                additions += 1;
                eprintln!(
                    "Add {} [{}]\n{}",
                    alias.name, alias.collection, alias.command
                );
            }
            Some(old)
                if old.command == alias.command
                    && (export.version < 3 || store.metadata(&old.id)? == alias.metadata) =>
            {
                skipped += 1;
            }
            Some(old) if replace => {
                replacements += 1;
                eprintln!(
                    "Replace {} [{}]\nOld command:\n{}\nNew command:\n{}",
                    alias.name, alias.collection, old.command, alias.command
                );
                if export.version >= 3 {
                    eprintln!(
                        "Old details: {}\nNew details: {}",
                        serde_json::to_string(&store.metadata(&old.id)?)?,
                        serde_json::to_string(&alias.metadata)?
                    );
                }
            }
            Some(_) => {
                skipped += 1;
                eprintln!(
                    "Skip {} [{}] (already saved; --replace permits replacement)",
                    alias.name, alias.collection
                );
            }
        }
    }
    eprintln!("Preview: {additions} additions, {replacements} replacements, {skipped} skipped");
    if preview {
        return Ok(());
    }
    if interactive
        && !yes
        && !Confirm::new()
            .with_prompt("Apply this import?")
            .default(false)
            .interact()?
    {
        return Ok(());
    }
    let apply = ExportFile {
        version: export.version,
        collections: export
            .collections
            .iter()
            .map(|collection| ExportCollection {
                name: collection.name.clone(),
            })
            .collect(),
        aliases: export
            .aliases
            .iter()
            .filter(|alias| {
                replace
                    || !existing.iter().any(|old| {
                        old.name == alias.name && old.collection_name == alias.collection
                    })
            })
            .map(|alias| ExportAlias {
                name: alias.name.clone(),
                command: alias.command.clone(),
                collection: alias.collection.clone(),
                metadata: alias.metadata.clone(),
            })
            .collect(),
    };
    store.import(&apply)?;
    println!(
        "Imported {} aliases. Undo: aliaz undo",
        additions + replacements
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = Store::open(temp.path().join("aliases.db")).unwrap();
        (temp, store)
    }

    #[test]
    fn schema_two_upgrades_without_erasing_aliases_or_legacy_conflicts() {
        let (temp, store) = store();
        store
            .upsert_in_collection("shared", "old", "printf old")
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE alias_metadata; DROP TABLE alias_usage; DROP TABLE alias_history;
            PRAGMA user_version=2;",
            )
            .unwrap();
        store.conn.execute("INSERT INTO conflict_backups (id, alias_name, command, remote_version, created_at) VALUES ('legacy', 'ambiguous', 'saved conflict', 1, 1)", []).unwrap();
        drop(store);
        let store = Store::open(temp.path().join("aliases.db")).unwrap();
        assert_eq!(
            store.picker_aliases().unwrap()[0].metadata,
            Metadata::default()
        );
        assert!(store.restore_conflict("legacy", None, None, false).is_err());
        store
            .restore_conflict("legacy", Some("shared"), Some("recovered"), false)
            .unwrap();
        assert_eq!(store.list_effective().unwrap().len(), 2);
    }

    #[test]
    fn metadata_is_independent_of_old_command_payloads_and_upload_acknowledgements() {
        let (_temp, store) = store();
        let payload = MetadataPayload {
            alias_id: "fixture-id".into(),
            metadata: Metadata {
                description: "Purpose".into(),
                tags: vec!["tag".into()],
                favorite: true,
            },
            updated_at: 1,
        };
        let record = RemoteRecord {
            id: metadata_record_id("fixture-id"),
            record_type: "alias_metadata".into(),
            encrypted_blob: String::new(),
            version: 2,
        };
        // Details can arrive before the command and remain available afterwards.
        store.apply_remote_metadata(&record, &payload).unwrap();
        let alias_record = RemoteRecord {
            id: "fixture-id".into(),
            record_type: "alias".into(),
            encrypted_blob: String::new(),
            version: 3,
        };
        let command: AliasPayload = serde_json::from_str(
            r#"{"name":"old","command":"old client edit","deleted":false,"updated_at":2}"#,
        )
        .unwrap();
        store.apply_remote_alias(&alias_record, &command).unwrap();
        assert_eq!(store.metadata("fixture-id").unwrap(), payload.metadata);
        store
            .change_metadata("shared", "old", Some("first".into()), None, None)
            .unwrap();
        let snapshot = store.pending_metadata().unwrap().remove(0);
        store
            .change_metadata("shared", "old", Some("second".into()), None, None)
            .unwrap();
        store.mark_metadata_synced(&snapshot, 4).unwrap();
        assert_eq!(
            store.pending_metadata().unwrap()[0].metadata.description,
            "second"
        );
    }

    #[test]
    fn failed_group_has_no_partial_aliases_or_history() {
        let (_temp, store) = store();
        let result = store.record_change(true, || {
            store.upsert_in_collection("shared", "good", "true")?;
            store.upsert_in_collection("shared", "invalid!", "false")
        });
        assert!(result.is_err());
        assert!(store.list_all(None).unwrap().is_empty());
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM alias_history", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn scoped_conflicts_and_delete_undo_retain_details_identity_and_dirty_state() {
        let (_temp, store) = store();
        store.create_collection("work").unwrap();
        store
            .upsert_in_collection("shared", "same", "shared command")
            .unwrap();
        store
            .upsert_in_collection("work", "same", "work command")
            .unwrap();
        store
            .change_metadata(
                "work",
                "same",
                Some("work purpose".into()),
                Some(vec!["work".into()]),
                Some(true),
            )
            .unwrap();
        let scope = store.collection_by_name("work").unwrap();
        let alias = store
            .find_alias_by_collection_and_name(&scope.id, "same")
            .unwrap()
            .unwrap();
        store.backup_alias(&alias, 5).unwrap();
        let id: String = store
            .conn
            .query_row("SELECT id FROM conflict_backups", [], |row| row.get(0))
            .unwrap();
        store
            .record_change(true, || store.delete_alias(Some("work"), "same"))
            .unwrap();
        store.undo(None).unwrap();
        let restored = store
            .find_alias_by_collection_and_name(&scope.id, "same")
            .unwrap()
            .unwrap();
        assert_eq!(restored.id, alias.id);
        assert!(restored.dirty);
        assert!(store.metadata(&restored.id).unwrap().favorite);
        store.update_alias(Some("work"), "same", "changed").unwrap();
        store.restore_conflict(&id, None, None, true).unwrap();
        assert_eq!(
            store
                .find_alias_by_collection_and_name(&scope.id, "same")
                .unwrap()
                .unwrap()
                .command,
            "work command"
        );
        assert_eq!(
            store.metadata(&alias.id).unwrap().description,
            "work purpose"
        );
        assert_eq!(
            store.list_all(Some("shared")).unwrap()[0].command,
            "shared command"
        );
    }

    #[test]
    fn grouped_undo_restores_a_move_and_its_displaced_destination_tombstone() {
        let (_temp, store) = store();
        store.create_collection("work").unwrap();
        store
            .upsert_in_collection("shared", "same", "source")
            .unwrap();
        store
            .upsert_in_collection("work", "same", "deleted destination")
            .unwrap();
        let work = store.collection_by_name("work").unwrap();
        let source = store.list_all(Some("shared")).unwrap().remove(0);
        let destination = store.list_all(Some("work")).unwrap().remove(0);
        store.delete_alias(Some("work"), "same").unwrap();
        store
            .record_change(true, || store.move_alias("same", "shared", "work"))
            .unwrap();
        store.undo(None).unwrap();
        let restored_source = store.alias_by_id(&source.id).unwrap().unwrap();
        let restored_destination = store.alias_by_id(&destination.id).unwrap().unwrap();
        assert_eq!(restored_source.collection_id, DEFAULT_COLLECTION_ID);
        assert_eq!(restored_source.name, "same");
        assert!(!restored_source.deleted);
        assert_eq!(restored_destination.collection_id, work.id);
        assert_eq!(restored_destination.name, "same");
        assert!(restored_destination.deleted);
    }

    #[test]
    fn restoring_a_collision_revives_the_saved_identity_and_preserves_the_replaced_identity() {
        let (_temp, store) = store();
        store
            .upsert_in_collection("shared", "same", "losing local")
            .unwrap();
        let original = store.list_effective().unwrap().remove(0);
        store
            .change_metadata(
                "shared",
                "same",
                Some("original purpose".into()),
                Some(vec!["original".into()]),
                Some(true),
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE aliases SET updated_at = 1 WHERE id = ?1",
                [&original.id],
            )
            .unwrap();
        let record = RemoteRecord {
            id: "winning-identity".into(),
            record_type: "alias".into(),
            encrypted_blob: String::new(),
            version: 2,
        };
        let payload = AliasPayload {
            collection_id: DEFAULT_COLLECTION_ID.into(),
            collection_name: "shared".into(),
            name: "same".into(),
            command: "remote winner".into(),
            deleted: false,
            updated_at: 2,
        };
        store.apply_remote_alias(&record, &payload).unwrap();
        let backup: String = store
            .conn
            .query_row("SELECT id FROM conflict_backups", [], |row| row.get(0))
            .unwrap();
        store
            .record_change(true, || store.restore_conflict(&backup, None, None, true))
            .unwrap();
        let restored = store.list_effective().unwrap().remove(0);
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.command, "losing local");
        assert_eq!(
            store.metadata(&original.id).unwrap().description,
            "original purpose"
        );
        assert!(
            store
                .alias_by_id("winning-identity")
                .unwrap()
                .unwrap()
                .deleted
        );
        assert!(
            store
                .alias_by_id("winning-identity")
                .unwrap()
                .unwrap()
                .dirty
        );
        // Recovery is one undo group, including the displaced winner.
        store.undo(None).unwrap();
        assert_eq!(store.list_effective().unwrap()[0].id, "winning-identity");
    }

    #[test]
    fn incoming_conflict_details_can_arrive_in_a_later_pull_without_changing_local_backups() {
        let (_temp, store) = store();
        store
            .upsert_in_collection("shared", "same", "newer local")
            .unwrap();
        store
            .conn
            .execute("UPDATE aliases SET updated_at = 100", [])
            .unwrap();
        let record = RemoteRecord {
            id: "losing-remote".into(),
            record_type: "alias".into(),
            encrypted_blob: String::new(),
            version: 1,
        };
        let payload = AliasPayload {
            collection_id: DEFAULT_COLLECTION_ID.into(),
            collection_name: "shared".into(),
            name: "same".into(),
            command: "older remote".into(),
            deleted: false,
            updated_at: 1,
        };
        store.apply_remote_alias(&record, &payload).unwrap();
        let metadata = MetadataPayload {
            alias_id: record.id.clone(),
            metadata: Metadata {
                description: "remote purpose".into(),
                tags: vec!["remote".into()],
                favorite: true,
            },
            updated_at: 1,
        };
        let details = RemoteRecord {
            id: metadata_record_id(&record.id),
            record_type: "alias_metadata".into(),
            encrypted_blob: String::new(),
            version: 2,
        };
        store.apply_remote_metadata(&details, &metadata).unwrap();
        let snapshot: String = store
            .conn
            .query_row("SELECT snapshot FROM conflict_backups", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Alias>(&snapshot).unwrap().metadata,
            metadata.metadata
        );
    }

    #[test]
    fn a_known_incoming_conflict_snapshot_is_not_rewritten_by_a_later_details_edit() {
        let (_temp, store) = store();
        store
            .upsert_in_collection("shared", "same", "newer local")
            .unwrap();
        store
            .conn
            .execute("UPDATE aliases SET updated_at = 100", [])
            .unwrap();
        let metadata = MetadataPayload {
            alias_id: "losing-remote".into(),
            metadata: Metadata {
                description: "original remote purpose".into(),
                tags: vec!["original".into()],
                favorite: true,
            },
            updated_at: 1,
        };
        let details = RemoteRecord {
            id: metadata_record_id(&metadata.alias_id),
            record_type: "alias_metadata".into(),
            encrypted_blob: String::new(),
            version: 1,
        };
        store.apply_remote_metadata(&details, &metadata).unwrap();
        let record = RemoteRecord {
            id: metadata.alias_id.clone(),
            record_type: "alias".into(),
            encrypted_blob: String::new(),
            version: 2,
        };
        let payload = AliasPayload {
            collection_id: DEFAULT_COLLECTION_ID.into(),
            collection_name: "shared".into(),
            name: "same".into(),
            command: "older remote".into(),
            deleted: false,
            updated_at: 2,
        };
        // This pull contains only a command; its unchanged details are below the cursor.
        store.apply_remote_alias(&record, &payload).unwrap();
        let changed = MetadataPayload {
            alias_id: metadata.alias_id.clone(),
            metadata: Metadata {
                description: "later remote purpose".into(),
                tags: vec![],
                favorite: false,
            },
            updated_at: 3,
        };
        let next = RemoteRecord {
            version: 3,
            ..details
        };
        store.apply_remote_metadata(&next, &changed).unwrap();
        let snapshot: String = store
            .conn
            .query_row("SELECT snapshot FROM conflict_backups", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Alias>(&snapshot).unwrap().metadata,
            metadata.metadata
        );
        assert_eq!(
            store.metadata(&metadata.alias_id).unwrap(),
            changed.metadata
        );
    }
}

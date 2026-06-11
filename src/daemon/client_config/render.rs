//! The one MCP-config renderer — format-switched on `ConfigSchema`, IDE-agnostic.
//!
//! Replaces the old per-format `write_uniform_mcp_servers` / `write_toml_mcp_servers`
//! and their purge siblings. Reads the existing config (JSON, JSONC, or TOML),
//! splices our managed server entries in/out under `schema.key_path`, and writes
//! atomically via `io`. Four invariants the real editor schemas force:
//!   1. token substitution is recursive over the whole `entry_template` tree;
//!   2. JSONC reads tolerate comments + trailing commas;
//!   3. `key_path` is walked-and-created, merging beside the user's siblings;
//!   4. purge matches the endpoint anywhere in an entry, not a fixed `url` field.
//!
//! CC-16: all write entrypoints serialize per canonical path via `WRITE_LOCKS`.
//! CC-17: `render_exact` reads+parses once, removes our entries, inserts servers, writes once.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use jsonc_parser::common::Ranged as _;
use serde_json::{Map, Value};
use toml_edit::DocumentMut;

use super::io::{atomic_write_json, atomic_write_jsonc_str, atomic_write_toml_str};
use super::types::{ConfigFormat, ConfigSchema};
use super::types::{ResolvedClient, Scope};
use super::ClientError;

// ── CC-16: per-path writer serialization ───────────────────────────────────

static WRITE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// Stable lock key for `path` that does not depend on the file existing yet.
/// Full `canonicalize` only works once the file is present, so a writer creating
/// the file and one updating it afterwards would key on different entries and
/// momentarily bypass the per-path serialization (CC-16). Canonicalize the
/// parent dir (which normally exists) and re-attach the file name so both writers
/// land on the same entry; fall back to a lexically-absolute path, then literal.
fn lock_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(canonical_parent) = parent.canonicalize() {
            return canonical_parent.join(name);
        }
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Obtain (or create) the per-path lock for `path`, keyed via [`lock_key`].
/// Returns a cloned `Arc` so the registry lock is released before any file I/O.
fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    let canonical = lock_key(path);
    let registry = WRITE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    // Hold the registry lock only long enough to clone the Arc out.
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        map.entry(canonical)
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

fn url_for(port: u16, key: &str, server: &str) -> String {
    format!("http://127.0.0.1:{port}/{key}/{server}/mcp")
}

/// Does `url` point at our daemon for this key? Matches any port so that
/// changing `daemon.port` doesn't orphan entries written under the old port.
/// The per-client `key` segment is already globally unique — it is sufficient
/// to identify ownership without the port.
fn url_is_ours(url: &str, _port: u16, key: &str) -> bool {
    // Accept http://127.0.0.1:<any-port>/<key>/<server>/mcp
    let Some(after_host) = url.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    // Skip past the port digits.
    let after_port = after_host.trim_start_matches(|c: char| c.is_ascii_digit());
    let expected_prefix = format!("/{key}/");
    let Some(rest) = after_port.strip_prefix(expected_prefix.as_str()) else {
        return false;
    };
    let Some(server_part) = rest.strip_suffix("/mcp") else {
        return false;
    };
    !server_part.is_empty() && !server_part.contains('/')
}

/// Render the entry template for one server, substituting the fixed token vocab
/// recursively into every string at any depth (rule 1). A token may live inside
/// a nested array — e.g. `["mcp-remote", "<url-token>"]`.
fn render_entry(template: &Value, port: u16, key: &str, server: &str) -> Value {
    let url = url_for(port, key, server);
    let port_s = port.to_string();
    substitute(template, &url, &port_s, key, server)
}

#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "${url} etc. are our placeholder tokens, not Rust format args"
)]
fn substitute(v: &Value, url: &str, port: &str, key: &str, name: &str) -> Value {
    match v {
        Value::String(s) => Value::String(
            s.replace("${url}", url)
                .replace("${port}", port)
                .replace("${key}", key)
                .replace("${name}", name),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| (k.clone(), substitute(val, url, port, key, name)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|val| substitute(val, url, port, key, name))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Does this entry (rendered or user-authored) reference our endpoint anywhere
/// in its serialized form (rule 4)? Walks every nested string.
fn entry_is_ours(entry: &Value, port: u16, key: &str) -> bool {
    any_string(entry, &|s| url_is_ours(s, port, key))
}

fn entry_targets_server(entry: &Value, port: u16, key: &str, server: &str) -> bool {
    let target = url_for(port, key, server);
    any_string(entry, &|s| s == target)
}

fn any_string(v: &Value, pred: &dyn Fn(&str) -> bool) -> bool {
    match v {
        Value::String(s) => pred(s),
        Value::Object(m) => m.values().any(|val| any_string(val, pred)),
        Value::Array(a) => a.iter().any(|val| any_string(val, pred)),
        _ => false,
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Make the client's config file hold *exactly* our `servers` set.
///
/// Purges every entry of ours, then renders the given set, preserving the
/// user's own entries. This is what callers want after a managed-set change
/// (toggle, cascade) — `render_servers` alone merges and would leave removed
/// servers behind.
pub fn render_mcp(
    client: &ResolvedClient,
    servers: &[String],
    port: u16,
) -> Result<(), ClientError> {
    render_scope(client, &Scope::Global, servers, port)
}

/// Make one `(client, scope)`'s MCP config file hold exactly our `servers` set.
///
/// Global writes the user config in place. A repo scope writes
/// `scope_dir/project_config_rel`; when the last entry is removed and the file
/// is left holding no managed entries and no user content, the ours-created file
/// (and any dirs we made up to the repo root) are deleted so an empty repo isn't
/// littered (§5). A client with no repo config path for this scope is skipped.
pub fn render_scope(
    client: &ResolvedClient,
    scope: &Scope,
    servers: &[String],
    port: u16,
) -> Result<(), ClientError> {
    let Some(path) = client.config_target(scope) else {
        // Global always resolves; None means a repo scope on a client lacking a
        // project_config_rel — a bug for a built-in, expected for custom rows.
        debug_assert!(
            !client.is_builtin,
            "built-in {} has no repo config path for {scope:?}",
            client.id
        );
        tracing::trace!(client = %client.id, "render_scope: no config target; skipping");
        return Ok(());
    };
    // CC-17/CC-20: render_exact does the read-parse-remove-insert-write in one
    // pass and also returns whether the file had our entries before (for CC-20
    // cleanup logic).
    let had_our_entries = render_exact(&client.schema, &path, &client.mcp_key, servers, port)?;
    if let Scope::Repo(repo_dir) = scope {
        let we_own_file = had_our_entries || !servers.is_empty();
        if we_own_file {
            cleanup_empty_repo_file(&client.schema, &path, repo_dir)?;
        }
    }
    Ok(())
}

/// Make the file under `path` hold exactly the `servers` set for our key.
///
/// Preserves user entries. Reads and writes at most once (CC-17). Returns
/// `true` when the file held at least one of our entries before (CC-20).
/// Rejects an empty `schema.key_path` to prevent root-level mangling.
pub fn render_servers(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    servers: &[String],
    port: u16,
) -> Result<(), ClientError> {
    render_exact(schema, path, key, servers, port)?;
    Ok(())
}

/// Drop every entry under the schema's `key_path` whose endpoint is ours.
/// Missing file is a no-op.
pub fn purge_mcp(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    port: u16,
) -> Result<(), ClientError> {
    render_exact(schema, path, key, &[], port)?;
    Ok(())
}

/// Drop one server's entry under the schema's `key_path`. Missing file is a no-op.
pub fn purge_one(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    server: &str,
    port: u16,
) -> Result<(), ClientError> {
    // purge_one targets a single server, not all ours — use purge_where directly.
    // CC-16: acquire the per-path lock around the read-modify-write.
    let lock = path_lock(path);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    purge_where(schema, path, &|entry| {
        entry_targets_server(entry, port, key, server)
    })
}

/// CC-17: single read-parse-remove-insert-write pass for a given `servers` set.
///
/// Removes all entries belonging to our key, then inserts `servers`. User entries
/// are preserved. Returns `true` when the file held at least one of our entries
/// before the operation.
///
/// CC-16: serializes writers per canonical path via `path_lock`.
fn render_exact(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    servers: &[String],
    port: u16,
) -> Result<bool, ClientError> {
    if schema.key_path.is_empty() {
        return Err(ClientError::EmptyKeyPath);
    }

    let entries: Vec<(String, Value)> = servers
        .iter()
        .map(|s| {
            (
                s.clone(),
                render_entry(&schema.entry_template, port, key, s),
            )
        })
        .collect();

    // CC-16: hold the per-path lock across the entire read-modify-write.
    let lock = path_lock(path);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match schema.format {
        ConfigFormat::Toml => render_exact_toml(path, &schema.key_path, key, port, &entries),
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            render_exact_json(path, schema.format, &schema.key_path, key, port, &entries)
        }
    }
}

fn render_exact_json(
    path: &Path,
    format: ConfigFormat,
    key_path: &[String],
    key: &str,
    port: u16,
    entries: &[(String, Value)],
) -> Result<bool, ClientError> {
    // Skip creating a scaffold for an empty entry set when the file doesn't
    // already hold our key_path — avoids a no-op rewrite that strips comments.
    let existing = read_json(path, format)?;
    if entries.is_empty() {
        let has_path = existing
            .as_ref()
            .is_some_and(|r| descend_existing_json_ref(r, key_path).is_some());
        if !has_path {
            return Ok(false);
        }
    }
    let original = existing.clone();
    let mut root = existing.unwrap_or_default();
    let had_ours = descend_existing_json(&mut root, key_path)
        .is_some_and(|map| map.values().any(|e| entry_is_ours(e, port, key)));
    {
        let map = descend_create(&mut root, key_path);
        map.retain(|_n, e| !entry_is_ours(e, port, key));
        for (name, entry) in entries {
            map.insert(name.clone(), entry.clone());
        }
    }
    // Skip the rewrite when nothing actually changed — preserves the user's
    // exact bytes/formatting on a no-op purge/render (CC-3).
    if original.as_ref() == Some(&root) {
        return Ok(had_ours);
    }
    // CC-1: for JSONC files that already have the key_path, use the splice
    // path to preserve comments and formatting outside the managed subtree.
    // Falls back to full serialize on any splice failure.
    if format == ConfigFormat::Jsonc {
        if let Some(spliced) = try_splice_jsonc(path, key_path, &root) {
            atomic_write_jsonc_str(path, &spliced)?;
            return Ok(had_ours);
        }
    }
    atomic_write_json(path, &Value::Object(root))?;
    Ok(had_ours)
}

/// CC-1: attempt to splice the updated `key_path` subtree into the original JSONC
/// file text, preserving all bytes outside that subtree (comments, sibling keys,
/// indentation). Returns `None` on any failure so the caller can fall back.
///
/// Strategy: parse the original file to an AST to obtain the byte `Range` of the
/// object at `key_path`. Serialize the new value for that subtree as JSON (using
/// the surrounding indentation). Splice: original[..span.start] + new + original[span.end..].
fn try_splice_jsonc(
    path: &Path,
    key_path: &[String],
    new_root: &Map<String, Value>,
) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // Strip BOM if present, same as read_json.
    let payload = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let text = std::str::from_utf8(payload).ok()?;

    let parse_result = jsonc_parser::parse_to_ast(
        text,
        &jsonc_parser::CollectOptions::default(),
        &jsonc_parser::ParseOptions::default(),
    )
    .ok()?;
    let root_obj = parse_result.value.as_ref()?.as_object()?;

    // Walk key_path through the AST to find the target object.
    let target_obj = descend_ast_object(root_obj, key_path)?;
    let span = target_obj.range();

    // Build the replacement text: serialize the new subtree value from new_root.
    // The subtree sits at `key_path.len()` nesting depth, so seed the serializer
    // at that base depth — otherwise its inner entries and closing brace come out
    // under-indented (the opening brace is fine, it inherits the spliced prefix).
    let new_subtree = descend_existing_json_ref(new_root, key_path)?;
    let indent = detect_indent(text);
    let replacement = serialize_object_indented(new_subtree, indent, key_path.len());

    // Splice: keep everything outside the span byte-for-byte.
    let before = text.get(..span.start)?;
    let after = text.get(span.end..)?;
    Some(format!("{before}{replacement}{after}"))
}

/// Walk the AST `Object` tree following `key_path`, returning the `Object` at the leaf.
fn descend_ast_object<'a>(
    root: &'a jsonc_parser::ast::Object<'a>,
    key_path: &[String],
) -> Option<&'a jsonc_parser::ast::Object<'a>> {
    let mut cur = root;
    for seg in key_path {
        cur = cur.get_object(seg)?;
    }
    Some(cur)
}

/// Detect the predominant indentation unit from the file text.
/// Looks at the first line that starts with whitespace and uses that.
/// Falls back to 2 spaces if no indented line is found.
fn detect_indent(text: &str) -> &str {
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if !trimmed.is_empty() && trimmed != line {
            // The leading whitespace is all ASCII (' '/'\\t'), so every char is
            // one byte. Splitting at char boundaries is always correct here.
            let ws_byte_len = line.len() - trimmed.len();
            // SAFETY: ws_byte_len is the count of ASCII space/tab bytes; those
            // are always valid UTF-8 boundaries.
            if let Some(s) = line.get(..ws_byte_len) {
                return s;
            }
        }
    }
    "  "
}

/// Serialize a JSON object map as a `{…}` block using `indent` per level.
/// This is a minimal pretty-printer: one entry per line, trailing comma on
/// each entry, no trailing comma on the last. Only needs to handle the shapes
/// our entry templates can produce (objects, arrays, strings, numbers, bools, null).
fn serialize_object_indented(map: &Map<String, Value>, indent: &str, base_depth: usize) -> String {
    serialize_value_indented(&Value::Object(map.clone()), indent, base_depth)
}

fn serialize_value_indented(v: &Value, indent: &str, depth: usize) -> String {
    match v {
        Value::Object(m) => {
            if m.is_empty() {
                return "{}".to_owned();
            }
            let inner_indent = indent.repeat(depth + 1);
            let close_indent = indent.repeat(depth);
            let mut out = String::from("{\n");
            let count = m.len();
            for (i, (k, val)) in m.iter().enumerate() {
                let comma = if i + 1 < count { "," } else { "" };
                let key_str = serde_json::to_string(k).unwrap_or_default();
                let val_str = serialize_value_indented(val, indent, depth + 1);
                out.push_str(&inner_indent);
                out.push_str(&key_str);
                out.push_str(": ");
                out.push_str(&val_str);
                out.push_str(comma);
                out.push('\n');
            }
            out.push_str(&close_indent);
            out.push('}');
            out
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_owned()),
    }
}

fn render_exact_toml(
    path: &Path,
    key_path: &[String],
    key: &str,
    port: u16,
    entries: &[(String, Value)],
) -> Result<bool, ClientError> {
    let existing = read_toml_edit(path)?;
    if entries.is_empty() {
        let has_path = existing
            .as_ref()
            .is_some_and(|doc| descend_existing_toml_edit_ref(doc.as_table(), key_path).is_some());
        if !has_path {
            return Ok(false);
        }
    }
    let mut doc = existing.unwrap_or_default();
    let original_str = doc.to_string();
    let had_ours = descend_existing_toml_edit(&mut doc, key_path).is_some_and(|table| {
        table
            .iter()
            .any(|(_, item)| entry_is_ours(&toml_item_to_json(item), port, key))
    });
    {
        let table = descend_create_toml_edit(&mut doc, key_path);
        table.retain(|_n, item| !entry_is_ours(&toml_item_to_json(item), port, key));
        for (name, entry) in entries {
            table.insert(name, json_to_toml_edit_item(entry));
        }
    }
    let new_str = doc.to_string();
    // Skip the rewrite when nothing changed — preserves the user's exact
    // bytes/comments/formatting on a no-op purge/render (CC-3).
    if new_str == original_str {
        return Ok(had_ours);
    }
    atomic_write_toml_str(path, &new_str)?;
    Ok(had_ours)
}

// ── Empty-repo cleanup (repo scope only) ─────────────────────────────────────

/// After a repo-scope render, delete the file if it holds only our (now-empty)
/// scaffold, then prune empty ancestor dirs up to (not incl.) the repo dir.
fn cleanup_empty_repo_file(
    schema: &ConfigSchema,
    path: &Path,
    repo_dir: &Path,
) -> Result<(), ClientError> {
    if !path.exists() || !file_is_scaffold_only(schema, path)? {
        return Ok(());
    }
    fs::remove_file(path)?;
    prune_empty_dirs(path.parent(), repo_dir);
    Ok(())
}

/// True when the file contains nothing but our `key_path` scaffold with an empty
/// leaf — no managed entries, no user keys at any level. Blank/missing = empty.
fn file_is_scaffold_only(schema: &ConfigSchema, path: &Path) -> Result<bool, ClientError> {
    match schema.format {
        ConfigFormat::Toml => Ok(read_toml_edit(path)?
            .as_ref()
            .is_none_or(|doc| toml_doc_is_scaffold_only(doc, &schema.key_path))),
        ConfigFormat::Json | ConfigFormat::Jsonc => Ok(read_json(path, schema.format)?
            .as_ref()
            .is_none_or(|root| map_is_scaffold_only(root, &schema.key_path))),
    }
}

fn map_is_scaffold_only(root: &Map<String, Value>, key_path: &[String]) -> bool {
    let mut cur = root;
    for seg in key_path {
        if cur.is_empty() {
            return true;
        }
        if cur.len() > 1 {
            return false;
        }
        match cur.get(seg).and_then(Value::as_object) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.is_empty()
}

fn toml_doc_is_scaffold_only(doc: &DocumentMut, key_path: &[String]) -> bool {
    // Walk the key_path as nested tables; a missing or multi-key level means
    // the document has user content outside our scaffold.
    let mut cur: &dyn toml_edit::TableLike = doc.as_table();
    for seg in key_path {
        if cur.is_empty() {
            return true;
        }
        if cur.len() > 1 {
            return false;
        }
        match cur.get(seg).and_then(toml_edit::Item::as_table_like) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.is_empty()
}

/// Remove empty ancestor dirs from `start` upward, stopping before `repo_dir`
/// (never deletes the repo itself). Stops at the first non-empty/undeletable dir.
fn prune_empty_dirs(start: Option<&Path>, repo_dir: &Path) {
    let mut cur = start;
    while let Some(dir) = cur {
        if dir == repo_dir {
            break;
        }
        let is_empty = fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none());
        if !is_empty || fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

// ── JSON / JSONC ────────────────────────────────────────────────────────────

fn purge_where(
    schema: &ConfigSchema,
    path: &Path,
    is_target: &dyn Fn(&Value) -> bool,
) -> Result<(), ClientError> {
    match schema.format {
        ConfigFormat::Toml => {
            let Some(mut doc) = read_toml_edit(path)? else {
                return Ok(());
            };
            let removed =
                descend_existing_toml_edit(&mut doc, &schema.key_path).is_some_and(|table| {
                    let before = table.len();
                    table.retain(|_n, item| !is_target(&toml_item_to_json(item)));
                    table.len() != before
                });
            if !removed {
                return Ok(());
            }
            atomic_write_toml_str(path, &doc.to_string())
        }
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            let Some(mut root) = read_json(path, schema.format)? else {
                return Ok(());
            };
            let removed = descend_existing_json(&mut root, &schema.key_path).is_some_and(|map| {
                let before = map.len();
                map.retain(|_n, e| !is_target(e));
                map.len() != before
            });
            if !removed {
                return Ok(());
            }
            atomic_write_json(path, &Value::Object(root))
        }
    }
}

/// Walk `key_path`, creating intermediate objects, and return the server map at
/// the leaf (rule 3). Empty `key_path` writes at the root.
#[expect(
    clippy::expect_used,
    reason = "the line above guarantees the slot is an object; a conditional \
              reborrow in this loop hits an NLL limitation, so expect is the \
              clean infallible descent"
)]
fn descend_create<'a>(
    root: &'a mut Map<String, Value>,
    key_path: &[String],
) -> &'a mut Map<String, Value> {
    let mut cur = root;
    for seg in key_path {
        let slot = cur
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        // A non-object value where we need to descend is malformed by our
        // standards; replace it rather than clobber siblings elsewhere.
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        cur = slot
            .as_object_mut()
            .expect("slot ensured to be an object above");
    }
    cur
}

fn descend_existing_json<'a>(
    root: &'a mut Map<String, Value>,
    key_path: &[String],
) -> Option<&'a mut Map<String, Value>> {
    let mut cur = root;
    for seg in key_path {
        cur = cur.get_mut(seg)?.as_object_mut()?;
    }
    Some(cur)
}

fn descend_existing_json_ref<'a>(
    root: &'a Map<String, Value>,
    key_path: &[String],
) -> Option<&'a Map<String, Value>> {
    let mut cur = root;
    for seg in key_path {
        cur = cur.get(seg)?.as_object()?;
    }
    Some(cur)
}

fn read_json(path: &Path, format: ConfigFormat) -> Result<Option<Map<String, Value>>, ClientError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ClientError::Io(e)),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    // CC-18: strip a leading UTF-8 BOM (EF BB BF) before parsing.
    let payload = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(bytes.as_slice());
    // CC-18: use strict UTF-8 decoding — lossy conversion silently corrupts
    // non-UTF-8 bytes and then writes them back, destroying the file.
    let text = std::str::from_utf8(payload).map_err(|_e| ClientError::ParseJson {
        path: path.to_owned(),
        source: serde::de::Error::custom("config file is not valid UTF-8"),
    })?;
    let cleaned = if format == ConfigFormat::Jsonc {
        strip_jsonc(text)
    } else {
        text.to_owned()
    };
    let value: Value = serde_json::from_str(&cleaned).map_err(|source| ClientError::ParseJson {
        path: path.to_owned(),
        source,
    })?;
    match value {
        Value::Object(m) => Ok(Some(m)),
        _ => Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        ))),
    }
}

/// Strip `//` and `/* */` comments and trailing commas from a JSONC document,
/// leaving string literals untouched (rule 2). Comment-tolerant read path for
/// editors (VS Code, Gemini, Zed) whose config carries comments.
fn strip_jsonc(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    strip_trailing_commas(&out)
}

/// Remove commas that immediately precede `}` or `]` (ignoring whitespace),
/// outside string literals.
fn strip_trailing_commas(src: &str) -> String {
    let bytes: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let next = bytes
                .get(i + 1..)
                .and_then(|rest| rest.iter().find(|n| !n.is_whitespace()));
            if matches!(next, Some('}' | ']')) {
                continue; // drop the trailing comma
            }
        }
        out.push(c);
    }
    out
}

// ── TOML (toml_edit — comment/format-preserving) ─────────────────────────────

#[expect(
    clippy::expect_used,
    reason = "the entry above guarantees the slot is a table; a conditional \
              reborrow in this loop hits an NLL limitation, so expect is the \
              clean infallible descent"
)]
fn descend_create_toml_edit<'a>(
    doc: &'a mut DocumentMut,
    key_path: &[String],
) -> &'a mut toml_edit::Table {
    let mut cur: &mut toml_edit::Table = doc.as_table_mut();
    for seg in key_path {
        let slot = cur.entry(seg).or_insert_with(toml_edit::table);
        if !slot.is_table() {
            *slot = toml_edit::table();
        }
        cur = slot
            .as_table_mut()
            .expect("slot ensured to be a table above");
    }
    cur
}

fn descend_existing_toml_edit<'a>(
    doc: &'a mut DocumentMut,
    key_path: &[String],
) -> Option<&'a mut toml_edit::Table> {
    let mut cur: &mut toml_edit::Table = doc.as_table_mut();
    for seg in key_path {
        cur = cur.get_mut(seg)?.as_table_mut()?;
    }
    Some(cur)
}

fn descend_existing_toml_edit_ref<'a>(
    table: &'a toml_edit::Table,
    key_path: &[String],
) -> Option<&'a toml_edit::Table> {
    let mut cur = table;
    for seg in key_path {
        cur = cur.get(seg)?.as_table()?;
    }
    Some(cur)
}

fn read_toml_edit(path: &Path) -> Result<Option<DocumentMut>, ClientError> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ClientError::Io(e)),
    };
    if text.chars().all(char::is_whitespace) {
        return Ok(None);
    }
    let doc: DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ClientError::ParseToml {
                path: path.to_owned(),
                source: serde::de::Error::custom(e.to_string()),
            })?;
    Ok(Some(doc))
}

/// Convert a JSON value into a `toml_edit::Item` for insertion into a TOML table.
/// Objects become inline tables so they stay on one line under their parent key.
fn json_to_toml_edit_item(v: &Value) -> toml_edit::Item {
    toml_edit::Item::Value(json_to_toml_edit_value(v))
}

fn json_to_toml_edit_value(v: &Value) -> toml_edit::Value {
    match v {
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => n.as_i64().map_or_else(
            || toml_edit::Value::from(n.as_f64().unwrap_or(0.0)),
            toml_edit::Value::from,
        ),
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for val in a {
                arr.push(json_to_toml_edit_value(val));
            }
            toml_edit::Value::Array(arr)
        }
        Value::Object(m) => {
            let mut tbl = toml_edit::InlineTable::new();
            for (k, val) in m {
                tbl.insert(k.as_str(), json_to_toml_edit_value(val));
            }
            toml_edit::Value::InlineTable(tbl)
        }
        Value::Null => toml_edit::Value::from(""),
    }
}

/// Convert a `toml_edit::Item` to a `serde_json::Value` for the `is_target` check
/// in `purge_where`. Only needs to cover the cases our entries can produce.
fn toml_item_to_json(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::Value(v) => toml_edit_value_to_json(v),
        toml_edit::Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_item_to_json(v)))
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn toml_edit_value_to_json(v: &toml_edit::Value) -> Value {
    match v {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::Number((*i.value()).into()),
        toml_edit::Value::Float(f) => {
            serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number)
        }
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(a) => Value::Array(a.iter().map(toml_edit_value_to_json).collect()),
        toml_edit::Value::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_edit_value_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        unused_qualifications,
        reason = "test ergonomics"
    )]

    use super::*;
    use serde_json::json;

    fn schema(
        format: ConfigFormat,
        key_path: &[&str],
        template: serde_json::Value,
    ) -> ConfigSchema {
        ConfigSchema {
            format,
            key_path: key_path.iter().map(|s| (*s).to_string()).collect(),
            entry_template: template,
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        use crate::daemon::time_util::now_unix_nanos;
        std::env::temp_dir().join(format!("conduit-render-test-{name}-{}", now_unix_nanos()))
    }

    // Guard: an empty key_path (corrupt/defaulted schema) must NOT write our
    // entries at the file root — it would mangle the user's real editor config.
    #[test]
    fn render_refuses_empty_key_path_and_leaves_file_untouched() {
        let dir = tmp("empty-keypath");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"userSetting":"keep"}"#).unwrap();

        let s = schema(ConfigFormat::Json, &[], json!({"url": "${url}"}));
        let err = render_servers(&s, &path, "k0000000", &["gmail".to_string()], 9876);
        assert!(matches!(err, Err(ClientError::EmptyKeyPath)));

        // File is byte-for-byte untouched (no root-level junk written).
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"userSetting":"keep"}"#
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 1: recursive substitution of the full token vocab at any depth,
    // including inside a nested array (the mcp-remote / Zed shape).
    #[test]
    fn substitutes_all_tokens_recursively_including_nested_arrays() {
        let dir = tmp("tokens");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zed.json");
        let s = schema(
            ConfigFormat::Json,
            &["context_servers"],
            json!({
                "source": "custom",
                "command": "npx",
                "args": ["mcp-remote", "${url}"],
                "meta": { "label": "${name}@${port}", "tok": "${key}" }
            }),
        );

        render_servers(&s, &path, "abc12345", &["gmail".to_string()], 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &v["context_servers"]["gmail"];
        assert_eq!(entry["args"][1], "http://127.0.0.1:9876/abc12345/gmail/mcp");
        assert_eq!(entry["meta"]["label"], "gmail@9876");
        assert_eq!(entry["meta"]["tok"], "abc12345");
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 2: JSONC reads tolerate // and /* */ comments and trailing commas.
    #[test]
    fn jsonc_read_tolerates_comments_and_trailing_commas() {
        let dir = tmp("jsonc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{
                // top comment
                "userSetting": "keep me", /* inline */
                "servers": {
                    "mine": { "url": "http://x", }, // trailing comma
                },
            }"#,
        )
        .unwrap();

        let s = schema(
            ConfigFormat::Jsonc,
            &["servers"],
            json!({"type":"http","url":"${url}"}),
        );
        render_servers(&s, &path, "vsc00001", &["gmail".to_string()], 9876).unwrap();

        // The output is JSONC (comments preserved) — read via JSONC reader.
        let root = read_json(&path, ConfigFormat::Jsonc).unwrap().unwrap();
        let v = Value::Object(root);
        assert_eq!(v["userSetting"], "keep me");
        assert_eq!(v["servers"]["mine"]["url"], "http://x");
        assert_eq!(v["servers"]["gmail"]["type"], "http");
        std::fs::remove_dir_all(&dir).ok();
    }

    // The stripper must NOT touch // or /* inside string literals.
    #[test]
    fn jsonc_strip_preserves_comment_like_strings() {
        let src = r#"{ "url": "http://example.com/a", "note": "a /* b */ c" }"#;
        let cleaned = strip_jsonc(src);
        let v: Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["url"], "http://example.com/a");
        assert_eq!(v["note"], "a /* b */ c");
    }

    // Rule 3: walk-and-create a multi-segment key_path, preserving siblings.
    #[test]
    fn creates_nested_key_path_and_keeps_siblings() {
        let dir = tmp("nested");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        std::fs::write(&path, r#"{"mcp":{"existing":1}}"#).unwrap();

        let s = schema(
            ConfigFormat::Json,
            &["mcp", "servers"],
            json!({"url":"${url}"}),
        );
        render_servers(&s, &path, "k0000000", &["gmail".to_string()], 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["mcp"]["existing"], 1);
        assert!(v["mcp"]["servers"]["gmail"]["url"].is_string());
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 4: purge matches the endpoint anywhere in an entry (url-in-args),
    // not just a fixed `url` field.
    #[test]
    fn purge_matches_endpoint_inside_nested_args() {
        let dir = tmp("purge-args");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zed.json");
        let s = schema(
            ConfigFormat::Json,
            &["context_servers"],
            json!({"command":"npx","args":["mcp-remote","${url}"]}),
        );
        render_servers(&s, &path, "ourkey00", &["gmail".to_string()], 9876).unwrap();
        // Add a user entry that is NOT ours.
        let mut cur: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cur["context_servers"]["theirs"] = json!({"command":"npx","args":["other"]});
        std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

        purge_mcp(&s, &path, "ourkey00", 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            v["context_servers"]["gmail"].is_null(),
            "url-in-args entry purged"
        );
        assert!(
            v["context_servers"]["theirs"].is_object(),
            "user entry kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // CC-7: purge must match our entries regardless of port number.
    #[test]
    fn purge_matches_any_port_for_same_key() {
        let dir = tmp("any-port");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let s = schema(
            ConfigFormat::Json,
            &["mcpServers"],
            json!({"url": "${url}"}),
        );

        // Write an entry under the old port (9876).
        render_servers(&s, &path, "mykey001", &["server1".to_string()], 9876).unwrap();

        // Now purge using a DIFFERENT port (8080) — should still find the entry.
        purge_mcp(&s, &path, "mykey001", 8080).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            v["mcpServers"]["server1"].is_null(),
            "entry written on old port should be purged on new port"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // CC-18: a leading UTF-8 BOM must be stripped before JSON parsing.
    #[test]
    fn read_json_strips_utf8_bom() {
        let dir = tmp("bom");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bom.json");
        // Prepend a UTF-8 BOM (EF BB BF) to a valid JSON object.
        let mut content = b"\xef\xbb\xbf".to_vec();
        content.extend_from_slice(br#"{"mcpServers":{}}"#);
        std::fs::write(&path, &content).unwrap();

        let s = schema(
            ConfigFormat::Json,
            &["mcpServers"],
            json!({"url": "${url}"}),
        );
        // Should succeed rather than failing with a parse error.
        render_servers(&s, &path, "k0000001", &["svc".to_string()], 9876).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v["mcpServers"]["svc"].is_object());
        std::fs::remove_dir_all(&dir).ok();
    }

    // CC-18: a file with invalid UTF-8 bytes must return an error, not corrupt.
    #[test]
    fn read_json_errors_on_invalid_utf8() {
        let dir = tmp("bad-utf8");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        // 0xFF is invalid in UTF-8.
        std::fs::write(&path, b"\xff\xfe{}").unwrap();

        let s = schema(
            ConfigFormat::Json,
            &["mcpServers"],
            json!({"url": "${url}"}),
        );
        let result = render_servers(&s, &path, "k0000002", &["svc".to_string()], 9876);
        assert!(result.is_err(), "invalid UTF-8 file should return an error");
        std::fs::remove_dir_all(&dir).ok();
    }

    // CC-16: concurrent render + purge to the same path must not lose or
    // duplicate entries — the per-path lock must serialize the writers.
    #[test]
    fn concurrent_render_and_purge_are_consistent() {
        use std::sync::Arc;
        use std::thread;

        let dir = tmp("concurrent");
        std::fs::create_dir_all(&dir).unwrap();
        let path = Arc::new(dir.join("config.json"));

        let s = schema(
            ConfigFormat::Json,
            &["mcpServers"],
            json!({"url": "${url}"}),
        );
        let s = Arc::new(s);

        // Prime the file with a user entry that must never be lost.
        std::fs::write(
            path.as_ref(),
            r#"{"mcpServers":{"user-entry":{"url":"http://other"}}}"#,
        )
        .unwrap();

        let n = 20usize;
        let mut handles = Vec::with_capacity(n * 2);

        for i in 0..n {
            let path2 = Arc::clone(&path);
            let s2 = Arc::clone(&s);
            let server = format!("srv{i}");
            handles.push(thread::spawn(move || {
                render_servers(&s2, &path2, "cc16test", &[server], 9876).unwrap();
            }));

            let path3 = Arc::clone(&path);
            let s3 = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                purge_mcp(&s3, &path3, "cc16test", 9876).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Regardless of which render/purge won last, the user entry must survive.
        let v: Value = serde_json::from_slice(&std::fs::read(path.as_ref()).unwrap()).unwrap();
        assert!(
            v["mcpServers"]["user-entry"].is_object(),
            "user entry must survive concurrent render+purge"
        );

        // The file must be valid JSON (no torn writes).
        assert!(v["mcpServers"].is_object(), "mcpServers must be an object");

        std::fs::remove_dir_all(&dir).ok();
    }
}

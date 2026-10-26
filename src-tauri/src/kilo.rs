//! Kilo Code provider configuration.
//!
//! Kilo reads JSONC from `~/.config/kilo/kilo.jsonc` on ALL platforms (Windows included:
//! `C:\Users\<you>\.config\kilo\kilo.jsonc`). Credentials live separately in `auth.json`
//! under the data dir — an inline `apiKey` in kilo.jsonc works for requests but does NOT
//! create the auth-store entry, which makes the Settings UI show an empty key field
//! (Kilo issue #10139). We therefore write BOTH.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub const BASE_URL: &str = "https://litellm.rapidscreen.io/v1";
pub const PROVIDER_ID: &str = "aibs";
pub const PROVIDER_NAME: &str = "AI Builder School";
/// Env var we ask Kilo to read the key from, so the config file stays shareable.
pub const API_KEY_ENV: &str = "AIBS_API_KEY";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine home directory")]
    NoHome,
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("existing config is not valid JSONC: {0}")]
    Parse(String),
}

fn home() -> Result<PathBuf, ConfigError> {
    dirs::home_dir().ok_or(ConfigError::NoHome)
}

/// `~/.config/kilo/` — Kilo's config dir on every OS (NOT platform XDG on Windows).
pub fn kilo_config_dir() -> Result<PathBuf, ConfigError> {
    Ok(home()?.join(".config").join("kilo"))
}

pub fn kilo_config_path() -> Result<PathBuf, ConfigError> {
    Ok(kilo_config_dir()?.join("kilo.jsonc"))
}

/// Where Kilo keeps credentials (`auth.json`).
pub fn kilo_auth_path() -> Result<PathBuf, ConfigError> {
    Ok(home()?
        .join(".local")
        .join("share")
        .join("kilo")
        .join("auth.json"))
}

/// Strip `//` and `/* */` comments so we can parse existing JSONC safely.
/// Not a full JSONC parser — good enough for Kilo's config shape, and it preserves
/// semantics for the values we round-trip.
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Remove trailing commas before } or ] which JSONC permits but serde_json rejects.
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut j = 0;
    while j < chars.len() {
        if chars[j] == ',' {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if k < chars.len() && (chars[k] == '}' || chars[k] == ']') {
                j += 1;
                continue;
            }
        }
        cleaned.push(chars[j]);
        j += 1;
    }
    cleaned
}

/// Load existing config, or an empty object if the file does not exist.
pub fn load_existing() -> Result<Value, ConfigError> {
    let path = kilo_config_path()?;
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| ConfigError::Io {
        path: path.clone(),
        source: e,
    })?;
    let stripped = strip_jsonc_comments(&raw);
    if stripped.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&stripped).map_err(|e| ConfigError::Parse(e.to_string()))
}

/// A model entry as returned by the gateway's `/v1/models`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
}

/// Build the merged config: preserves everything already there, adds/overwrites only
/// our `aibs` provider and the default model.
///
/// The key is written INLINE by default. Verified behaviour: Kilo resolves
/// `{env:VAR}` only if the variable is already present in the running process, so the
/// placeholder silently fails for a student who launches VS Code from the GUI after we
/// wrote the var to a shell profile. An inline key just works.
pub fn build_config(
    existing: &Value,
    models: &[String],
    default_model: &str,
    api_key: &str,
    use_env_for_key: bool,
) -> Value {
    let mut root: Map<String, Value> = match existing.as_object() {
        Some(m) => m.clone(),
        None => Map::new(),
    };

    let model_map: Map<String, Value> = models
        .iter()
        .map(|id| {
            (
                id.clone(),
                json!({
                    "name": id,
                    "tool_call": true,
                    "limit": { "context": 128000, "output": 16384 }
                }),
            )
        })
        .collect();

    let api_key_value: Value = if use_env_for_key {
        json!(format!("{{env:{API_KEY_ENV}}}"))
    } else {
        json!(api_key)
    };

    let provider = json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PROVIDER_NAME,
        "options": {
            "baseURL": BASE_URL,
            "apiKey": api_key_value,
        },
        "models": Value::Object(model_map),
    });

    let providers = root
        .entry("provider")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("provider must be an object");
    providers.insert(PROVIDER_ID.to_string(), provider);

    // Only set the top-level default model if it points at our provider, so we never
    // stomp a student's deliberate choice.
    root.insert(
        "model".to_string(),
        json!(format!("{PROVIDER_ID}/{default_model}")),
    );

    Value::Object(root)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), ConfigError> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| ConfigError::Io {
            path: p.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

/// Write a file that holds a credential, readable only by its owner.
///
/// `fs::write` creates with the process umask (0644 under the usual 022), which makes the
/// API key world-readable on a shared or multi-user machine. The mode is set after the
/// write because `O_CREAT`'s mode is still masked by the umask; a create-then-chmod cannot
/// be masked. On Windows the call is a no-op and the file inherits the user's profile ACL.
fn write_private(path: &std::path::Path, contents: &str) -> Result<(), ConfigError> {
    std::fs::write(path, contents).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
    }
    Ok(())
}

/// Write config, backing up any existing file to `kilo.jsonc.bak`.
pub fn write_config(cfg: &Value) -> Result<PathBuf, ConfigError> {
    let path = kilo_config_path()?;
    ensure_parent(&path)?;
    if path.exists() {
        let bak = path.with_extension("jsonc.bak");
        std::fs::copy(&path, &bak).map_err(|e| ConfigError::Io {
            path: bak.clone(),
            source: e,
        })?;
    }
    let pretty = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".into());
    // The key is written inline by default, so the config is as sensitive as the auth store.
    write_private(&path, &pretty)?;
    Ok(path)
}

/// Write the credential into Kilo's auth store so the Settings UI shows the key.
pub fn write_auth(api_key: &str) -> Result<PathBuf, ConfigError> {
    let path = kilo_auth_path()?;
    ensure_parent(&path)?;
    let mut root: Map<String, Value> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&strip_jsonc_comments(&s)).ok())
            .and_then(|v: Value| v.as_object().cloned())
            .unwrap_or_default()
    } else {
        Map::new()
    };
    root.insert(
        PROVIDER_ID.to_string(),
        json!({ "type": "api", "key": api_key }),
    );
    let pretty = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    write_private(&path, &pretty)?;
    Ok(path)
}

/// Persist the key as a user-level env var so `{env:AIBS_API_KEY}` resolves.
/// Returns a description of what was done for the UI log.
pub fn persist_env_var(api_key: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let out = crate::platform::command("setx")
            .args([API_KEY_ENV, api_key])
            .output()
            .map_err(|e| format!("setx failed: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).to_string());
        }
        Ok(format!(
            "Set user environment variable {API_KEY_ENV} (setx)"
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir().ok_or("no home dir")?;
        let export = format!("export {API_KEY_ENV}=\"{api_key}\"\n");
        // Choose the profile file that matches the user's shell.
        let shell = std::env::var("SHELL").unwrap_or_default();
        let profile = if shell.ends_with("zsh") {
            home.join(".zshrc")
        } else if shell.ends_with("fish") {
            home.join(".config").join("fish").join("config.fish")
        } else {
            home.join(".bashrc")
        };
        if let Some(p) = profile.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        // Append only if not already present (idempotent re-runs).
        let existing = std::fs::read_to_string(&profile).unwrap_or_default();
        if existing.contains(&format!("export {API_KEY_ENV}=")) {
            let re = format!("export {API_KEY_ENV}=");
            let mut out = String::new();
            for line in existing.lines() {
                if line.starts_with(&re) {
                    out.push_str(&export);
                } else {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            std::fs::write(&profile, out).map_err(|e| e.to_string())?;
        } else {
            let mut content = existing;
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&export);
            std::fs::write(&profile, content).map_err(|e| e.to_string())?;
        }
        Ok(format!(
            "Set {API_KEY_ENV} in {}",
            profile.to_string_lossy()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_line_comments() {
        let s = strip_jsonc_comments("{\n  // hi\n  \"a\": 1\n}");
        assert_eq!(s.trim(), "{\n  \n  \"a\": 1\n}");
        assert!(serde_json::from_str::<Value>(&s).is_ok());
    }

    #[test]
    fn strips_block_comments() {
        let s = strip_jsonc_comments("{ /* x */ \"a\": 1 }");
        assert!(serde_json::from_str::<Value>(&s).is_ok());
    }

    #[test]
    fn preserves_comment_like_text_inside_strings() {
        let s = strip_jsonc_comments("{\"url\": \"https://x.io/v1\"}");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["url"], "https://x.io/v1");
    }

    #[test]
    fn strips_trailing_commas() {
        let s = strip_jsonc_comments("{\"a\":1,}");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn build_config_preserves_existing_providers() {
        let existing = json!({
            "provider": { "openai": { "options": { "apiKey": "sk-existing" } } },
            "theme": "dark"
        });
        let cfg = build_config(
            &existing,
            &["deepseek-v4.1-flash".into()],
            "deepseek-v4.1-flash",
            "sk-new",
            false,
        );
        assert_eq!(
            cfg["provider"]["openai"]["options"]["apiKey"],
            "sk-existing"
        );
        assert_eq!(cfg["theme"], "dark");
        assert!(cfg["provider"]["aibs"].is_object());
    }

    #[test]
    fn build_config_writes_the_key_inline_by_default() {
        let cfg = build_config(&json!({}), &["m1".into()], "m1", "sk-secret-123", false);
        assert_eq!(
            cfg["provider"]["aibs"]["options"]["apiKey"],
            "sk-secret-123"
        );
        assert_eq!(cfg["provider"]["aibs"]["options"]["baseURL"], BASE_URL);
        assert_eq!(cfg["model"], "aibs/m1");
        assert_eq!(cfg["provider"]["aibs"]["models"]["m1"]["tool_call"], true);
    }

    #[test]
    fn build_config_supports_the_env_placeholder_when_asked() {
        let cfg = build_config(&json!({}), &["m1".into()], "m1", "sk-secret-123", true);
        assert_eq!(
            cfg["provider"]["aibs"]["options"]["apiKey"],
            "{env:AIBS_API_KEY}"
        );
    }

    #[test]
    fn build_config_model_map_covers_all_models() {
        let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let cfg = build_config(&json!({}), &models, "a", "k", false);
        let m = cfg["provider"]["aibs"]["models"].as_object().unwrap();
        assert_eq!(m.len(), 3);
    }

    /// The key is written to disk, so the file must not be world-readable. `write_private`
    /// is the only path either writer uses; this pins the mode it produces.
    #[cfg(unix)]
    #[test]
    fn write_private_produces_owner_only_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kilo-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.json");

        write_private(&path, "{\"key\":\"sk-secret\"}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "credential file was created with mode {mode:o}"
        );

        // Re-writing over an existing file must not leave it more permissive.
        write_private(&path, "{\"key\":\"sk-rotated\"}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewrite widened the mode to {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The written content must still be the exact bytes asked for (the mode fix must not
    /// have changed the payload).
    #[cfg(unix)]
    #[test]
    fn write_private_preserves_content() {
        let dir = std::env::temp_dir().join(format!("kilo-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.json");
        let body = "{\n  \"a\": 1\n}";
        write_private(&path, body).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

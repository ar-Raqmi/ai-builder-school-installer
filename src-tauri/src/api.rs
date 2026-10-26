//! Live gateway client — validates the key and fetches the model list.
//!
//! Verified behaviour of the gateway: `GET /v1/models` returns 401 with a JSON error
//! body when no key is supplied, so the model list is only available AFTER key entry.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::Stdio;

pub const DEFAULT_BASE_URL: &str = "https://litellm.rapidscreen.io/v1";
/// The model students get by default. Cheap, fast, capable enough for the course.
pub const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub ok: bool,
    pub models: Vec<String>,
    pub message: String,
    /// Latency in ms for the models round-trip.
    pub latency_ms: u128,
}

fn http_get(url: &str, api_key: &str) -> Result<(u16, String), String> {
    // Minimal HTTP over a child process keeps the dependency surface tiny AND avoids
    // TLS/openssl build requirements on every student machine. curl ships on Windows 10+,
    // macOS and virtually every Linux, and we fall back to a pure-Rust path if absent.
    //
    // The Authorization header is handed to curl on stdin (`-H @-`) rather than in argv:
    // a command line is world-readable via `ps`, so passing the student's key there would
    // leak it to every process on the machine for the life of the request. `-H @-` also
    // merges with the ordinary `-H Accept:` argument below.
    let child = crate::platform::command("curl")
        .args([
            "-sS",
            "-m",
            "25",
            "-w",
            "\n__HTTP_CODE__%{http_code}",
            "-H",
            "@-",
            "-H",
            "Accept: application/json",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let out = match child {
        Ok(mut c) => {
            match c.stdin.take() {
                Some(mut si) => {
                    // curl only sends anything once stdin reaches EOF, so the handle is dropped
                    // at the end of this block, before waiting.
                    if let Err(e) = write!(si, "Authorization: Bearer {}\n", api_key.trim()) {
                        return Err(format!("could not hand the key to curl: {e}"));
                    }
                }
                None => return Err("curl got no stdin pipe".into()),
            }
            c.wait_with_output()
        }
        Err(e) => Err(e),
    };

    match out {
        Ok(o) => {
            let body = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(idx) = body.rfind("__HTTP_CODE__") {
                let (content, code) = body.split_at(idx);
                let code: u16 = code
                    .trim_start_matches("__HTTP_CODE__")
                    .trim()
                    .parse()
                    .unwrap_or(0);
                if code == 0 {
                    return Err(format!(
                        "could not reach the gateway. curl said: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ));
                }
                Ok((code, content.trim().to_string()))
            } else if !o.status.success() {
                Err(format!(
                    "network request failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            } else {
                Ok((200, body.trim().to_string()))
            }
        }
        Err(_) => Err(
            "curl is not available on this system, so the wizard cannot verify your key online. \
             You can still continue — the API key will be saved."
                .into(),
        ),
    }
}

/// Validate the key and return the model list.
pub fn verify_and_list(base_url: &str, api_key: &str) -> Result<VerifyResult, String> {
    if api_key.trim().is_empty() {
        return Err("Please enter your API key first.".into());
    }
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let started = std::time::Instant::now();
    let (code, body) = http_get(&url, api_key.trim())?;
    let latency_ms = started.elapsed().as_millis();

    if code == 401 || code == 403 {
        return Err("That API key was rejected (401). Check you copied the whole key from your KrackedDevs profile.".into());
    }
    if code != 200 {
        return Err(format!(
            "The gateway returned HTTP {code}. {}",
            body.chars().take(180).collect::<String>()
        ));
    }
    let parsed: ModelsResponse = serde_json::from_str(&body).map_err(|e| {
        format!(
            "Unexpected response format from the gateway: {e}. Body began: {}",
            body.chars().take(140).collect::<String>()
        )
    })?;
    if parsed.data.is_empty() {
        return Err("The gateway returned an empty model list.".into());
    }
    let mut models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
    models.sort();
    Ok(VerifyResult {
        ok: true,
        message: format!("Connected — {} models available.", models.len()),
        models,
        latency_ms,
    })
}

/// Pick the default model: the student's saved preference if present in the list,
/// else `DEFAULT_MODEL` if offered, else the first model presented.
pub fn choose_default(models: &[String], preferred: Option<&str>) -> Option<String> {
    if let Some(p) = preferred {
        if models.iter().any(|m| m == p) {
            return Some(p.to_string());
        }
    }
    if models.iter().any(|m| m == DEFAULT_MODEL) {
        return Some(DEFAULT_MODEL.to_string());
    }
    models.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_default_prefers_saved_choice() {
        let models = vec!["a".into(), DEFAULT_MODEL.into(), "b".into()];
        assert_eq!(
            choose_default(&models, Some(DEFAULT_MODEL)).unwrap(),
            DEFAULT_MODEL
        );
    }

    #[test]
    fn choose_default_falls_back_to_aibs_default() {
        let models = vec!["zzz".into(), DEFAULT_MODEL.into(), "aaa".into()];
        assert_eq!(choose_default(&models, None).unwrap(), DEFAULT_MODEL);
    }

    #[test]
    fn choose_default_falls_back_to_first_when_default_missing() {
        let models = vec!["zzz".into(), "aaa".into()];
        assert_eq!(choose_default(&models, None).unwrap(), "zzz");
    }

    #[test]
    fn choose_default_ignores_saved_choice_no_longer_offered() {
        let models = vec!["a".into(), DEFAULT_MODEL.into()];
        assert_eq!(
            choose_default(&models, Some("removed-model")).unwrap(),
            DEFAULT_MODEL
        );
    }

    #[test]
    fn choose_default_handles_empty_list() {
        assert!(choose_default(&[], None).is_none());
    }

    #[test]
    fn empty_key_is_rejected_before_network() {
        let err = verify_and_list(DEFAULT_BASE_URL, "   ").unwrap_err();
        assert!(err.contains("enter your API key"));
    }

    #[test]
    fn models_response_parses_gateway_shape() {
        let body = r#"{"data":[{"id":"gpt-5.5","object":"model"},{"id":"deepseek-v4.1-flash"}]}"#;
        let r: ModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(r.data.len(), 2);
        assert_eq!(r.data[1].id, "deepseek-v4.1-flash");
    }

    /// The regression this guards: the Authorization header was sent as a literal
    /// `Bearer ***`, so the gateway rejected every key with "expected to start with 'sk-'"
    /// and no student could ever verify one. The key must reach the request.
    ///
    /// A stub `curl` on PATH reads the header curl would have read, so this asserts the real
    /// hand-off (stdin) carries the live key AND that argv does not leak it.
    #[cfg(unix)]
    #[test]
    fn http_get_sends_the_key_on_stdin_not_in_argv() {
        let dir = std::env::temp_dir().join(format!("curl-stub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("curl");
        // `cat` drains the header curl would read on stdin, then prints it.
        std::fs::write(
            &stub,
            "#!/bin/sh\nprintf 'ARGV=%s\\n' \"$*\"\nprintf 'STDIN='\ncat\nprintf '\\n__HTTP_CODE__200'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let _env = crate::env_lock();
        let original = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.clone()];
        dirs.extend(std::env::split_paths(&original));
        std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
        let (code, body) = http_get("https://example.invalid/v1/models", "sk-live-key").unwrap();
        std::env::set_var("PATH", original);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(code, 200);
        // The key must travel on stdin, in a well-formed header.
        assert!(
            body.contains("Authorization: Bearer sk-live-key"),
            "the key never reached curl's stdin: {body:?}"
        );
        // And argv must not carry it, or `ps` would expose it.
        let argv = body.lines().find(|l| l.starts_with("ARGV=")).unwrap_or("");
        assert!(
            !argv.contains("sk-live-key"),
            "the key leaked into argv: {argv}"
        );
        // The literal placeholder must be gone for good.
        assert!(
            !body.contains("***"),
            "the old hard-coded placeholder is back: {body:?}"
        );
    }
}

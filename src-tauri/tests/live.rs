//! Integration tests that hit the REAL gateway and the REAL filesystem.
//!
//! These are `#[ignore]`d by default so `cargo test` stays hermetic. Run with:
//!   AIBS_TEST_KEY=sk-... cargo test --test live -- --ignored --nocapture

use aibs_setup_lib::api;
use aibs_setup_lib::kilo;
use aibs_setup_lib::platform::{self, Tool};

fn key() -> String {
    std::env::var("AIBS_TEST_KEY").unwrap_or_default()
}

#[test]
#[ignore = "hits the live gateway"]
fn live_models_endpoint_returns_expected_models() {
    let k = key();
    assert!(!k.is_empty(), "set AIBS_TEST_KEY to run this test");

    let res = api::verify_and_list("https://litellm.rapidscreen.io/v1", &k)
        .expect("gateway should accept the key");

    println!("latency: {} ms", res.latency_ms);
    println!("models ({}):", res.models.len());
    for m in &res.models {
        println!("  - {m}");
    }

    assert!(res.ok);
    assert!(res.models.len() >= 10, "expected >=10 models");
    assert!(
        res.models.iter().any(|m| m == "deepseek-v4.1-flash"),
        "deepseek-v4.1-flash must be offered"
    );

    // The default-picking logic must select our chosen default.
    let chosen = api::choose_default(&res.models, None).unwrap();
    assert_eq!(chosen, "deepseek-v4.1-flash");
    println!("default chosen: {chosen}");
}

#[test]
#[ignore = "hits the live gateway"]
fn live_rejects_bad_key() {
    let err = api::verify_and_list("https://litellm.rapidscreen.io/v1", "sk-obviously-wrong")
        .expect_err("a bad key must be rejected");
    println!("rejection message: {err}");
    assert!(err.contains("rejected") || err.contains("401"));
}

#[test]
#[ignore = "hits the live gateway"]
fn live_chat_completion_works_with_the_configured_provider() {
    // Proves the exact provider shape we write into kilo.jsonc would work.
    let k = key();
    assert!(!k.is_empty(), "set AIBS_TEST_KEY to run this test");

    let body = r#"{"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":"Reply with the single word: READY"}],"max_tokens":24}"#;
    let mut child = std::process::Command::new("curl")
        .args([
            "-sS",
            "-m",
            "60",
            "-w",
            "\n__CODE__%{http_code}",
            "-X",
            "POST",
            "-H",
            "@-",
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
            "https://litellm.rapidscreen.io/v1/chat/completions",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("curl should run");
    {
        use std::io::Write;
        let mut si = child.stdin.take().expect("stdin pipe");
        // Same stdin hand-off the app uses, so the key never lands in argv.
        writeln!(si, "Authorization: Bearer {}", k.trim()).expect("write header");
    }
    let out = child.wait_with_output().expect("curl should finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    println!("response: {}", &text.chars().take(500).collect::<String>());
    assert!(text.contains("__CODE__200"), "expected HTTP 200");
    assert!(text.contains("choices"), "expected a chat completion body");
}

#[test]
#[ignore = "reads and writes the real Kilo config directory"]
fn live_config_roundtrip_against_real_paths() {
    // Show where we would write on this machine, and prove the merge is correct
    // without touching an existing file.
    let cfg_path = kilo::kilo_config_path().unwrap();
    let auth_path = kilo::kilo_auth_path().unwrap();
    println!("config path: {}", cfg_path.display());
    println!("auth   path: {}", auth_path.display());

    let existing = kilo::load_existing().unwrap();
    println!(
        "existing config has {} top-level keys",
        existing.as_object().map(|o| o.len()).unwrap_or(0)
    );

    let models = vec!["deepseek-v4.1-flash".to_string(), "gpt-5.5".to_string()];
    let merged = kilo::build_config(
        &existing,
        &models,
        "deepseek-v4.1-flash",
        "sk-inline-test",
        false,
    );

    // Existing keys must survive.
    let before: Vec<String> = existing
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for k in &before {
        assert!(merged.get(k).is_some(), "merge dropped key `{k}`");
    }
    assert_eq!(merged["model"], "aibs/deepseek-v4.1-flash");
    assert_eq!(
        merged["provider"]["aibs"]["options"]["baseURL"],
        kilo::BASE_URL
    );
    assert_eq!(
        merged["provider"]["aibs"]["options"]["apiKey"],
        "sk-inline-test"
    );
    println!(
        "merged config:\n{}",
        serde_json::to_string_pretty(&merged).unwrap()
    );
}

#[test]
#[ignore = "inspects the real system"]
fn live_detects_installed_tools() {
    for t in [Tool::Vscode, Tool::Git, Tool::Node] {
        let p = platform::probe(t);
        println!(
            "{:22} found={} version={:?} path={:?}",
            t.label(),
            p.found,
            p.version.as_deref().map(|v| v.lines().next().unwrap_or("")),
            p.path
        );
        if let Some(c) = platform::install_command(t) {
            println!("{:22} install: {}", "", c.display);
        } else {
            println!("{:22} install: (manual)", "");
        }
    }
    println!("VS Code CLI: {:?}", platform::find_code_cli());
    println!("Linux family: {:?}", platform::linux_family());
}

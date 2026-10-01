//! Remote-only embedded compatibility control. Sentinels are not native Herdr proof.
//! The actual runtime must produce every session ID, tool output and hook event.
#![cfg(unix)]

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use app_test_support::MockResponsesConfig;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server::in_process;
use codex_app_server::in_process::InProcessClientHandle;
use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server::in_process::InProcessStartArgs;
use codex_app_server_protocol::HookEventName;
use codex_features::Feature;
use core_test_support::responses;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::timeout;

const TERMINAL_KEYS: [&str; 5] = [
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
];
const DEADLINE: Duration = Duration::from_secs(45);

#[tokio::test]
async fn de_identity_embedded_preserves_local_context() -> Result<()> {
    isolated_embedded_case(
        "de_identity_embedded_preserves_local_context",
        /*snapshot*/ false,
    )
    .await
}

#[tokio::test]
async fn de_identity_embedded_snapshot_preserves_local_context() -> Result<()> {
    isolated_embedded_case(
        "de_identity_embedded_snapshot_preserves_local_context",
        /*snapshot*/ true,
    )
    .await
}

async fn isolated_embedded_case(name: &str, snapshot: bool) -> Result<()> {
    if std::env::var("DE_IDENTITY_EMBEDDED_CASE").ok().as_deref() == Some(name) {
        return exercise_embedded(snapshot).await;
    }
    // The owning process receives synthetic metadata at process creation. No
    // process-global environment mutation, daemon, transport assertion or native ID.
    let root = TempDir::new()?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").context("runner PATH")?)
        .env("RUST_MIN_STACK", "8388608")
        .env("DE_IDENTITY_EMBEDDED_CASE", name)
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "fixture-local-pane")
        .env("HERDR_TAB_ID", "fixture-local-tab")
        .env("HERDR_WORKSPACE_ID", "fixture-local-workspace")
        .env("HERDR_SOCKET_PATH", root.path().join("absent-herdr.sock"))
        .current_dir(root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let result = timeout(Duration::from_secs(180), child.wait()).await;
    let status = match result {
        Ok(status) => status?,
        Err(error) => {
            child.kill().await?;
            child.wait().await?;
            return Err(error.into());
        }
    };
    ensure!(status.success(), "embedded child failed: {status}");
    Ok(())
}

async fn exercise_embedded(snapshot: bool) -> Result<()> {
    let root = TempDir::new()?;
    let home = root.path().join("codex-home");
    std::fs::create_dir(&home)?;
    let tool_script = root.path().join("capture-tool.py");
    std::fs::write(
        &tool_script,
        r#"import json, os
from pathlib import Path
keys = ['HERDR_ENV', 'HERDR_PANE_ID', 'HERDR_TAB_ID', 'HERDR_WORKSPACE_ID',
        'HERDR_SOCKET_PATH', 'CODEX_SESSION_ID', 'CODEX_THREAD_ID']
observed = {'cwd': os.getcwd(), 'env': {key: os.environ.get(key) for key in keys}}
Path('tool.json').write_text(json.dumps(observed))
print(json.dumps(observed))
"#,
    )?;
    let hook_script = root.path().join("capture-hook.py");
    std::fs::write(
        &hook_script,
        r#"import json, os, sys
from pathlib import Path
payload = json.load(sys.stdin)
keys = ['HERDR_ENV', 'HERDR_PANE_ID', 'HERDR_TAB_ID', 'HERDR_WORKSPACE_ID',
        'HERDR_SOCKET_PATH']
observed = {'session_id': payload['session_id'], 'cwd': payload['cwd'],
            'transcript_path': payload.get('transcript_path'),
            'env': {key: os.environ.get(key) for key in keys}}
with (Path(__file__).parent / 'hooks.jsonl').open('a') as handle:
    handle.write(json.dumps(observed) + '\n')
"#,
    )?;
    let hooks = json!({"hooks":{"SessionStart":[{"hooks":[{
        "type":"command", "command":format!("python3 {}", quote(&hook_script))
    }]}]}});
    std::fs::write(home.join("hooks.json"), serde_json::to_vec(&hooks)?)?;
    let args = json!({"cmd":format!("python3 {}", quote(&tool_script)), "login":snapshot,
                      "yield_time_ms":1000, "max_output_tokens":2000});
    let mut model_responses = Vec::new();
    for label in ["b", "c"] {
        model_responses.push(responses::sse(vec![
            responses::ev_response_created(&format!("tool-{label}")),
            responses::ev_function_call(
                &format!("call-{label}"),
                "exec_command",
                &args.to_string(),
            ),
            responses::ev_completed(&format!("tool-{label}")),
        ]));
        model_responses.push(responses::sse(vec![
            responses::ev_response_created(&format!("done-{label}")),
            responses::ev_assistant_message(&format!("message-{label}"), "Captured"),
            responses::ev_completed(&format!("done-{label}")),
        ]));
    }
    let model = create_mock_responses_server_sequence(model_responses).await;
    let mut config = MockResponsesConfig::new(&model.uri())
        .with_sandbox_mode("danger-full-access")
        .enable_feature(Feature::UnifiedExec)
        .enable_feature(Feature::CodexHooks)
        .disable_feature(Feature::ShellZshFork)
        .with_extra_config("[analytics]\nenabled = false");
    config = if snapshot {
        config.enable_feature(Feature::ShellSnapshot)
    } else {
        config.disable_feature(Feature::ShellSnapshot)
    };
    config.write(&home)?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.clone())
        .fallback_cwd(Some(root.path().to_path_buf()))
        .build()
        .await?;
    let config = Arc::new(config);
    let state_db = codex_rollout::state_db::try_init(config.as_ref()).await?;
    let mut client = in_process::start(InProcessStartArgs {
        arg0_paths: codex_arg0::Arg0DispatchPaths::default(),
        config,
        cli_overrides: Vec::new(),
        loader_overrides: codex_config::LoaderOverrides::default(),
        strict_config: false,
        cloud_config_bundle: codex_config::CloudConfigBundleLoader::default(),
        embedded_network_policy: Default::default(),
        thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
        feedback: codex_feedback::CodexFeedback::new(),
        log_db: None,
        state_db: Some(state_db),
        environment_manager: Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        config_warnings: Vec::new(),
        session_source: codex_protocol::protocol::SessionSource::Cli,
        enable_codex_api_key_env: false,
        initialize: serde_json::from_value(json!({
            "clientInfo":{"name":"de_identity_embedded", "version":"1"},
            "capabilities":{"experimentalApi":true}
        }))?,
        channel_capacity: 256,
    })
    .await?;
    let observations = async {
        let mut clients = Vec::new();
        for label in ["b", "c"] {
            let cwd = root.path().join(label);
            std::fs::create_dir(&cwd)?;
            if label == "b" {
                trust_fixture_hook(&mut client, &home, &hook_script).await?;
            }
            let started = request(&mut client, 2, "thread/start", json!({"cwd":cwd})).await?;
            clients.push((cwd, started["thread"].clone()));
        }
        let mut captured = Vec::new();
        for (cwd, thread) in clients {
            request(
                &mut client,
                3,
                "turn/start",
                json!({
                    "threadId":thread["id"], "input":[{"type":"text","text":"Capture context"}]
                }),
            )
            .await?;
            timeout(DEADLINE, async {
                loop {
                    let message = next_json(&mut client).await?;
                    if message["method"] == "turn/completed" {
                        ensure!(
                            message["params"]["turn"]["status"] == "completed",
                            "turn did not complete: {message}"
                        );
                        return Ok::<(), anyhow::Error>(());
                    }
                }
            })
            .await??;
            let tool_path = cwd.join("tool.json");
            let tool_bytes = std::fs::read(&tool_path)
                .with_context(|| format!("read tool observation {}", tool_path.display()))?;
            let tool: Value = serde_json::from_slice(&tool_bytes)
                .with_context(|| format!("parse tool observation {}", tool_path.display()))?;
            captured.push(json!({"thread":thread, "tool":tool}));
        }
        let hook_path = root.path().join("hooks.jsonl");
        let hook_text = std::fs::read_to_string(&hook_path).with_context(|| {
            format!(
                "read SessionStart observations {} after {} tool captures",
                hook_path.display(),
                captured.len()
            )
        })?;
        let hooks = hook_text
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok::<_, anyhow::Error>((captured, hooks))
    }
    .await;
    if let Err(error) = &observations {
        eprintln!("de_identity original observations error: {error:#}");
    }
    // Always close the actual embedded runtime before evaluating observations.
    client.shutdown().await?;
    let (captured, hooks) = observations?;
    eprintln!(
        "de_identity embedded runtime observations: {}",
        json!({"snapshot":snapshot,
        "clients":captured, "hooks":hooks, "nativeHerdrProof":false})
    );
    ensure!(
        captured.len() == 2 && hooks.len() == 2,
        "both real tools and hooks required"
    );
    ensure!(
        captured[0]["thread"]["id"] != captured[1]["thread"]["id"],
        "distinct runtime threads"
    );
    for observation in &captured {
        let thread = &observation["thread"];
        let session = thread["sessionId"]
            .as_str()
            .context("runtime sessionId missing")?;
        ensure!(!session.is_empty(), "empty runtime session ID");
        ensure!(
            observation["tool"]["env"]["CODEX_SESSION_ID"] == session,
            "runtime root ID mismatch"
        );
        ensure!(
            observation["tool"]["env"]["CODEX_THREAD_ID"] == thread["id"],
            "runtime thread ID mismatch"
        );
        let hook = hooks
            .iter()
            .find(|hook| hook["session_id"] == session)
            .context("real SessionStart event for runtime ID missing")?;
        for key in TERMINAL_KEYS {
            ensure!(
                observation["tool"]["env"][key] == std::env::var(key)?,
                "embedded tool lost local {key}"
            );
            ensure!(
                hook["env"][key] == std::env::var(key)?,
                "embedded hook lost local {key}"
            );
        }
    }
    Ok(())
}

// Approve only this fixture's command through the public, hash-scoped trust API.
async fn trust_fixture_hook(
    client: &mut InProcessClientHandle,
    home: &Path,
    hook_script: &Path,
) -> Result<()> {
    let listed = request(client, 4, "hooks/list", json!({"cwds":[home]})).await?;
    let entries = listed["data"]
        .as_array()
        .context("hooks/list data missing")?;
    ensure!(entries.len() == 1, "one hook discovery cwd required");
    ensure!(
        entries[0]["errors"] == json!([]),
        "hook discovery errors: {listed}"
    );
    let hooks = entries[0]["hooks"]
        .as_array()
        .context("hooks/list hooks missing")?;
    let matches: Vec<_> = hooks
        .iter()
        .filter(|hook| {
            hook["sourcePath"] == json!(home.join("hooks.json"))
                && hook["eventName"] == json!(HookEventName::SessionStart)
        })
        .collect();
    ensure!(matches.len() == 1, "one fixture SessionStart hook required");
    let hook = matches[0];
    ensure!(
        hook["handlerType"] == "command"
            && hook["command"] == format!("python3 {}", quote(hook_script))
            && hook["source"] == "user"
            && hook["enabled"] == true
            && hook["isManaged"] == false
            && hook["trustStatus"] == "untrusted",
        "unexpected fixture hook declaration: {hook}"
    );
    let key = hook["key"].as_str().context("hook key missing")?;
    let hash = hook["currentHash"].as_str().context("hook hash missing")?;
    ensure!(
        !key.is_empty() && !hash.is_empty(),
        "empty hook trust identity"
    );
    request(
        client,
        5,
        "config/batchWrite",
        json!({
            "edits":[{"keyPath":"hooks.state", "mergeStrategy":"upsert",
                "value":{(key):{"trusted_hash":hash}}}],
            "reloadUserConfig":true
        }),
    )
    .await?;
    let confirmed = request(client, 6, "hooks/list", json!({"cwds":[home]})).await?;
    let trusted = confirmed["data"][0]["hooks"]
        .as_array()
        .context("trusted hooks/list hooks missing")?
        .iter()
        .find(|entry| entry["key"] == key)
        .context("trusted fixture hook missing")?;
    ensure!(
        trusted["currentHash"] == hash
            && trusted["trustStatus"] == "trusted"
            && trusted["enabled"] == true,
        "fixture hook trust not applied: {trusted}"
    );
    eprintln!("de_identity fixture hook trust: {trusted}");
    Ok(())
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

async fn next_json(client: &mut InProcessClientHandle) -> Result<Value> {
    match client
        .next_event()
        .await
        .context("embedded runtime disconnected")?
    {
        InProcessServerEvent::ServerNotification(notification) => {
            Ok(serde_json::to_value(notification)?)
        }
        event => anyhow::bail!("unexpected embedded event: {event:?}"),
    }
}

async fn request(
    client: &mut InProcessClientHandle,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value> {
    let request = serde_json::from_value(json!({"id":id,"method":method,"params":params}))?;
    timeout(DEADLINE, client.request(request))
        .await??
        .map_err(|error| anyhow::anyhow!("{method}: {error:?}"))
}

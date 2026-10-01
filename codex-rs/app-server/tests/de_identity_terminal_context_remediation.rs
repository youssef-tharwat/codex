//! Remote-only regression control. Sentinel terminal metadata is not Herdr proof.
//! The actual runtime must produce every session ID, tool output and hook event.
#![cfg(unix)]

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use app_test_support::DISABLE_PLUGIN_STARTUP_TASKS_ARG;
use app_test_support::MockResponsesConfig;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::HookEventName;
use codex_features::Feature;
use codex_uds::UnixStream;
use core_test_support::responses;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

const TERMINAL_KEYS: [&str; 5] = [
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
];
const DEADLINE: Duration = Duration::from_secs(45);

#[tokio::test]
async fn de_identity_direct_rpc_excludes_terminal_overrides() -> Result<()> {
    exercise_legacy_clients(/*snapshot*/ false).await
}

#[tokio::test]
async fn de_identity_shared_login_replays_filtered_snapshot() -> Result<()> {
    exercise_legacy_clients(/*snapshot*/ true).await
}

async fn exercise_legacy_clients(snapshot: bool) -> Result<()> {
    let root = TempDir::new()?;
    let home = root.path().join("codex-home");
    std::fs::create_dir(&home)?;
    let tool_script = root.path().join("capture-tool.py");
    std::fs::write(
        &tool_script,
        r#"import json, os, sys
from pathlib import Path
keys = ['HERDR_ENV', 'HERDR_PANE_ID', 'HERDR_TAB_ID', 'HERDR_WORKSPACE_ID',
        'HERDR_SOCKET_PATH', 'CODEX_SESSION_ID', 'CODEX_THREAD_ID', 'herdr_pane_id', 'HeRdR_TaB_Id', 'DE_IDENTITY_REPLAY_MARKER']
observed = {'cwd': os.getcwd(), 'env': {key: os.environ.get(key) for key in keys}}
Path(sys.argv[1] if len(sys.argv) > 1 else 'tool.json').write_text(json.dumps(observed))
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
        .disable_feature(Feature::ShellSnapshotV2)
        .with_extra_config("[analytics]\nenabled = false");
    config = if snapshot {
        config.enable_feature(Feature::ShellSnapshot)
    } else {
        config.disable_feature(Feature::ShellSnapshot)
    };
    config.write(&home)?;
    let startup_home = root.path().join("bash-home");
    std::fs::create_dir(&startup_home)?;
    let startup = root.path().join("capture-startup.sh");
    let startup_probe = root.path().join("startup-probe.txt");
    if snapshot {
        std::fs::write(&startup, format!("export HOME={}\nunset BASH_ENV\nprintf 'home-seed HOME=%s BASH=%s\\n' \"$HOME\" \"${{BASH_VERSION-absent}}\" >> {}\n", quote(&startup_home), quote(&startup_probe)))?;
        std::fs::write(
            startup_home.join(".bashrc"),
            format!("export DE_IDENTITY_REPLAY_MARKER=actual-snapshot\ndeclare -rx HERDR_ENV=stale-readonly\ndeclare -rx herdr_pane_id=stale-mixed\nexport HeRdR_TaB_Id=stale-mixed-tab\nprintf 'bashrc HOME=%s BASH=%s\\n' \"$HOME\" \"${{BASH_VERSION-absent}}\" >> {}\n", quote(&startup_probe)),
        )?;
    }
    let startup_fixture = if snapshot {
        Some((std::fs::read(&startup)?, std::fs::read(startup_home.join(".bashrc"))?))
    } else {
        None
    };
    let socket = root.path().join("control/server.sock");
    let binary = codex_utils_cargo_bin::cargo_bin("codex-app-server")?;
    let stderr = std::fs::File::create(root.path().join("server.stderr"))?;
    let mut server = Command::new(binary);
    server
        .args(["--listen", &format!("unix://{}", socket.display())])
        .arg(DISABLE_PLUGIN_STARTUP_TASKS_ARG)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").context("runner PATH")?)
        .env("CODEX_HOME", &home)
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "fixture-daemon-pane")
        .env("HERDR_TAB_ID", "fixture-daemon-tab")
        .env("HERDR_WORKSPACE_ID", "fixture-daemon-workspace")
        .env("HERDR_SOCKET_PATH", root.path().join("absent-herdr.sock"))
        .current_dir(root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true);
    server.env("SHELL", "/bin/bash");
    if snapshot {
        eprintln!("de_identity detected shell: {:?}", codex_core::shell::default_user_shell());
        server.env("HOME", &startup_home).env("BASH_ENV", &startup)
            .env("RUST_LOG", "codex_core::shell_snapshot=trace,codex_core::environment_selection=trace");
    }
    // No CODEX_SESSION_ID or CODEX_THREAD_ID is inherited or supplied by the fixture.
    let mut child = server.spawn()?;
    let observations = async {
        let mut captured = Vec::new();
        for label in ["b", "c"] {
            let mut client = connect(&socket).await?;
            let cwd = root.path().join(label);
            std::fs::create_dir(&cwd)?;
            if label == "b" {
                trust_fixture_hook(&mut client, &home, &hook_script).await?;
            }
            if let Some((seed, bashrc)) = &startup_fixture {
                std::fs::write(&startup, seed)?;
                std::fs::write(startup_home.join(".bashrc"), bashrc)?;
            }
            let started = request(&mut client, 2, "thread/start", json!({"cwd":cwd})).await?;
            let thread = started["thread"].clone();
        if snapshot {
            // Consume each genuine snapshot before starting another unpersisted thread.
            // Snapshot cleanup may remove a different fresh thread's capture before its first turn.
            // Never author a snapshot file; require this runtime ID and its captured marker.
            timeout(DEADLINE, async {
                loop {
                    let snapshots = std::fs::read_dir(home.join("shell_snapshots"))
                        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect::<Vec<_>>())
                        .unwrap_or_default();
                    let id = thread["id"].as_str().unwrap();
                    let complete = snapshots.iter().any(|path| path.extension().is_some_and(|ext| ext == "sh")
                        && path.file_name().unwrap().to_string_lossy().starts_with(id)
                        && std::fs::read_to_string(path).is_ok_and(|text| text.contains("DE_IDENTITY_REPLAY_MARKER")));
                    if complete { break; }
                    sleep(Duration::from_millis(25)).await;
                }
            }).await?;
            // The command cannot obtain the marker from startup. It must replay the captured file.
            std::fs::remove_file(startup_home.join(".bashrc"))?;
            std::fs::remove_file(&startup)?;
        }
            let mut rpc_observations = Vec::new();
            if !snapshot {
                let overrides = json!({"HERDR_ENV":"request-assertion", "herdr_pane_id":"request-lower", "HeRdR_TaB_Id":"request-mixed"});
                request(&mut client, 20, "command/exec", json!({
                    "command":["python3",tool_script,"rpc-command.json"], "cwd":cwd,
                    "env":overrides
                })).await?;
                request(&mut client, 21, "process/spawn", json!({
                    "processHandle":format!("fixture-{}",thread["id"].as_str().unwrap()),
                    "command":["python3",tool_script,"rpc-process.json"], "cwd":cwd,
                    "env":overrides, "tty":false,"streamStdin":false,"streamStdoutStderr":false
                })).await?;
                timeout(DEADLINE, async {
                    while std::fs::read(cwd.join("rpc-process.json")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).is_none() { sleep(Duration::from_millis(25)).await; }
                }).await?;
                for file in ["rpc-command.json", "rpc-process.json"] {
                    let actual: Value = serde_json::from_slice(&std::fs::read(cwd.join(file))?)?;
                    eprintln!("de_identity actual RPC {file}: {actual}");
                    rpc_observations.push(actual);
                }
            }

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
            captured.push(json!({"thread":thread, "tool":tool, "rpc":rpc_observations}));
            client.close(None).await?;
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
        eprintln!("de_identity startup probe: {:?}", std::fs::read_to_string(&startup_probe));
        let snapshot_dir = home.join("shell_snapshots");
        eprintln!("de_identity snapshot directory: {}", snapshot_dir.display());
        match std::fs::read_dir(&snapshot_dir) {
            Ok(entries) => for entry in entries {
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        let text = std::fs::read_to_string(&path);
                        eprintln!("de_identity snapshot file: {:?}; metadata={:?}; marker={:?}; terminal={:?}",
                            path.file_name(), entry.metadata().map(|m| m.len()),
                            text.as_ref().map(|t| t.contains("DE_IDENTITY_REPLAY_MARKER")),
                            text.as_ref().map(|t| t.lines().filter(|line| line.contains("HERDR_") || line.contains("herdr_")).collect::<Vec<_>>()));
                    }
                    Err(error) => eprintln!("de_identity snapshot entry error: {error}"),
                }
            },
            Err(error) => eprintln!("de_identity snapshot directory error: {error}"),
        }
    }
    // The only child belongs to this fixture. Always reap it before assertions.
    child.kill().await?;
    child.wait().await?;
    let stderr = std::fs::read_to_string(root.path().join("server.stderr"))?;
    eprintln!("de_identity server stderr: {stderr}");
    let (captured, hooks) = observations?;
    eprintln!(
        "de_identity real runtime observations: {}",
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
        if !snapshot {
            let rpc = observation["rpc"].as_array().context("RPC observations")?;
            ensure!(rpc.len() == 2, "both direct RPC captures required");
            for actual in rpc {
                for key in TERMINAL_KEYS
                    .into_iter()
                    .chain(["herdr_pane_id", "HeRdR_TaB_Id"])
                {
                    ensure!(
                        actual["env"][key].is_null(),
                        "direct RPC inherited terminal {key}"
                    );
                }
            }
        }
        if snapshot {
            ensure!(
                observation["tool"]["env"]["DE_IDENTITY_REPLAY_MARKER"] == "actual-snapshot",
                "snapshot was not replayed"
            );
            ensure!(
                observation["tool"]["env"]["herdr_pane_id"].is_null(),
                "snapshot restored mixed-case terminal"
            );
            ensure!(
                observation["tool"]["env"]["HeRdR_TaB_Id"].is_null(),
                "snapshot restored mixed-case tab"
            );
        }

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
                observation["tool"]["env"][key].is_null(),
                "legacy tool inherited daemon {key}"
            );
            ensure!(
                hook["env"][key].is_null(),
                "legacy hook inherited daemon {key}"
            );
        }
    }
    Ok(())
}

// Approve only this fixture's command through the public, hash-scoped trust API.
async fn trust_fixture_hook(
    client: &mut WebSocketStream<UnixStream>,
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

async fn connect(socket: &Path) -> Result<WebSocketStream<UnixStream>> {
    let mut client = timeout(DEADLINE, async {
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await
                && let Ok((client, _)) = client_async("ws://localhost/", stream).await
            {
                return client;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    request(
        &mut client,
        1,
        "initialize",
        json!({
            "clientInfo":{"name":"de_identity_baseline", "version":"1"},
            "capabilities":{"experimentalApi":true}
        }),
    )
    .await?;
    client
        .send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))
        .await?;
    Ok(client)
}

async fn next_json(client: &mut WebSocketStream<UnixStream>) -> Result<Value> {
    loop {
        let frame = client.next().await.context("client disconnected")??;
        if let Message::Text(text) = frame {
            let message: Value = serde_json::from_str(&text)?;
            if matches!(
                message["method"].as_str(),
                Some("item/completed" | "hook/completed" | "configWarning")
            ) {
                // Diagnostics stay separate from the two complete observation records.
                eprintln!("de_identity protocol diagnostic: {message}");
            }
            return Ok(message);
        }
    }
}

async fn request(
    client: &mut WebSocketStream<UnixStream>,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value> {
    client
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await?;
    timeout(DEADLINE, async {
        loop {
            let message = next_json(client).await?;
            if message["id"] == id {
                ensure!(message.get("error").is_none(), "{method}: {message}");
                return Ok(message["result"].clone());
            }
        }
    })
    .await?
}

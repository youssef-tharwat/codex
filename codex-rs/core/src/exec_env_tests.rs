use super::*;
use codex_protocol::config_types::ShellEnvironmentPolicyInherit;
use maplit::hashmap;
use pretty_assertions::assert_eq;

fn make_vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn inject_permission_profile_env_overrides_policy_value() {
    let mut env = HashMap::from([(
        CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(
        &mut env,
        Some(&ActivePermissionProfile::new("current-profile")),
    );

    assert_eq!(
        env.get(CODEX_PERMISSION_PROFILE_ENV_VAR)
            .map(String::as_str),
        Some("current-profile")
    );
}

#[test]
fn inject_permission_profile_env_removes_stale_value_without_active_profile() {
    let mut env = HashMap::from([(
        CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(&mut env, /*active_permission_profile*/ None);

    assert_eq!(env.get(CODEX_PERMISSION_PROFILE_ENV_VAR), None);
}

#[test]
fn inject_apply_patch_env_follows_preserve_line_endings_feature() {
    let mut env = HashMap::from([(
        CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR.to_ascii_lowercase(),
        "stale".to_string(),
    )]);
    let mut features = Features::with_defaults();

    inject_apply_patch_env(&mut env, &features);
    assert_eq!(env, HashMap::new());

    features.enable(Feature::ApplyPatchPreserveLineEndings);
    inject_apply_patch_env(&mut env, &features);
    assert_eq!(
        env,
        HashMap::from([(
            CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR.to_string(),
            "1".to_string(),
        )])
    );
}

#[cfg(target_os = "windows")]
#[test]
fn inject_permission_profile_env_replaces_differently_cased_windows_key() {
    let mut env = HashMap::from([(
        "codex_permission_profile".to_string(),
        "stale-profile".to_string(),
    )]);

    inject_permission_profile_env(
        &mut env,
        Some(&ActivePermissionProfile::new("current-profile")),
    );

    assert_eq!(
        env,
        HashMap::from([(
            CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
            "current-profile".to_string(),
        )])
    );
}

#[test]
fn test_core_inherit_defaults_keep_sensitive_vars() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy::default(); // inherit All, default excludes ignored
    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "HOME".to_string() => "/home/user".to_string(),
        "API_KEY".to_string() => "secret".to_string(),
        "SECRET_TOKEN".to_string() => "t".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_core_inherit_with_default_excludes_enabled() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy {
        ignore_default_excludes: false, // apply KEY/SECRET/TOKEN filter
        ..Default::default()
    };
    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "HOME".to_string() => "/home/user".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_include_only() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        // skip default excludes so nothing is removed prematurely
        ignore_default_excludes: true,
        include_only: vec![EnvironmentVariablePattern::new_case_insensitive("*PATH")],
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_set_overrides() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);

    let mut policy = ShellEnvironmentPolicy {
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy.r#set.insert("NEW_VAR".to_string(), "42".to_string());

    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
        "NEW_VAR".to_string() => "42".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn populate_env_inserts_thread_id() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));

    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn populate_env_omits_thread_id_when_missing() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let result = populate_env(vars, &policy, /*thread_id*/ None);

    let expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };

    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: true, // keep everything
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = populate_env(vars.clone(), &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = vars.into_iter().collect();
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all_with_default_excludes() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("API_KEY", "secret")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: false,
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "PATH".to_string() => "/usr/bin".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn test_core_inherit_respects_case_insensitive_names_on_windows() {
    let vars = make_vars(&[
        ("Path", "C:\\Windows\\System32"),
        ("PathExt", ".COM;.EXE;.BAT;.CMD"),
        ("TEMP", "C:\\Temp"),
        ("FOO", "bar"),
    ]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "Path".to_string() => "C:\\Windows\\System32".to_string(),
        "PathExt".to_string() => ".COM;.EXE;.BAT;.CMD".to_string(),
        "TEMP".to_string() => "C:\\Temp".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());

    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn create_env_inserts_pathext_on_windows_when_missing() {
    let vars = make_vars(&[]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let result = create_env_from_vars(vars, &policy, /*thread_id*/ None);

    let expected: HashMap<String, String> = hashmap! {
        "PATHEXT".to_string() => ".COM;.EXE;.BAT;.CMD".to_string(),
    };
    assert_eq!(result, expected);
}

#[test]
#[cfg(target_os = "windows")]
fn create_env_preserves_existing_pathext_case_insensitively_on_windows() {
    let vars = make_vars(&[("PathExt", ".COM;.EXE;.BAT;.CMD;.PS1")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let result = create_env_from_vars(vars, &policy, /*thread_id*/ None);

    let pathext_vars = result
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("PATHEXT"))
        .collect::<Vec<_>>();

    assert_eq!(pathext_vars.len(), 1);
    assert_eq!(pathext_vars[0].1, ".COM;.EXE;.BAT;.CMD;.PS1");
}

#[test]
fn test_inherit_none() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("HOME", "/home")]);

    let mut policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy
        .r#set
        .insert("ONLY_VAR".to_string(), "yes".to_string());

    let thread_id = ThreadId::new();
    let result = populate_env(vars, &policy, Some(thread_id));
    let mut expected: HashMap<String, String> = hashmap! {
        "ONLY_VAR".to_string() => "yes".to_string(),
    };
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn unavailable_terminal_filters_inheritance_and_config_overrides() {
    let mut vars = make_vars(&[("PATH", "/bin"), ("ordinary", "kept")]);
    vars.extend(TERMINAL_ENV_KEYS.map(|key| (key.to_string(), "daemon".to_string())));
    vars.push(("herdr_pane_id".to_string(), "daemon-lowercase".to_string()));
    let policy = ShellEnvironmentPolicy {
        r#set: HashMap::from([
            ("HERDR_PANE_ID".to_string(), "client-assertion".to_string()),
            ("herdr_socket_path".to_string(), "client-socket".to_string()),
        ]),
        ..Default::default()
    };
    let filtered = TerminalEnvironment::Unavailable.shell_environment_policy(&policy);
    let env = populate_env(vars, &filtered, /*thread_id*/ None);
    assert_eq!(
        env,
        HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("ordinary".to_string(), "kept".to_string()),
        ])
    );
    // Filtering a session's policy must not change another session's input.
    assert_eq!(policy.r#set.len(), 2);
}

#[cfg(unix)]
#[test]
fn snapshot_preserves_effective_terminal_environment() -> anyhow::Result<()> {
    use crate::shell::Shell;
    use crate::shell::ShellType;
    use crate::tools::runtimes::RuntimePathPrepends;
    use crate::tools::runtimes::maybe_wrap_shell_lc_with_snapshot;
    use codex_utils_absolute_path::AbsolutePathBuf;

    let root = tempfile::tempdir()?;
    let snapshot = root.path().join("snapshot.sh");
    let stale = TERMINAL_ENV_KEYS.map(|key| format!("export {key}=stale-snapshot"));
    std::fs::write(&snapshot, stale.join("\n"))?;
    let snapshot = AbsolutePathBuf::try_from(snapshot)?;
    let shell = Shell {
        shell_type: ShellType::Sh,
        shell_path: "/bin/sh".into(),
    };
    let command = shell.derive_exec_args(
        r#"printf '%s\n' "${HERDR_ENV-unset}" "${HERDR_PANE_ID-unset}" "${HERDR_TAB_ID-unset}" "${HERDR_WORKSPACE_ID-unset}" "${HERDR_SOCKET_PATH-unset}""#,
        /*use_login_shell*/ true,
    );
    for (terminal, excluded) in [
        (TerminalEnvironment::LocalProcess, false),
        (TerminalEnvironment::Unavailable, false),
        (TerminalEnvironment::LocalProcess, true),
    ] {
        let policy = ShellEnvironmentPolicy {
            exclude: if excluded {
                TERMINAL_ENV_KEYS
                    .map(EnvironmentVariablePattern::new_case_insensitive)
                    .to_vec()
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        let effective_policy = terminal.shell_environment_policy(&policy);
        let vars = TERMINAL_ENV_KEYS.map(|key| (key.to_string(), "local-owner".to_string()));
        let env = populate_env(vars, &effective_policy, /*thread_id*/ None);
        let wrapped = maybe_wrap_shell_lc_with_snapshot(
            &command,
            &shell,
            Some(&snapshot),
            &policy.r#set,
            &env,
            &RuntimePathPrepends::default(),
        );
        let output = std::process::Command::new(&wrapped[0])
            .args(&wrapped[1..])
            .env_clear()
            .envs(&env)
            .output()?;
        assert!(output.status.success(), "{:?}", output);
        let expected = if matches!(terminal, TerminalEnvironment::LocalProcess) && !excluded {
            "local-owner\n".repeat(5)
        } else {
            "unset\n".repeat(5)
        };
        assert_eq!(String::from_utf8(output.stdout)?, expected);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn snapshot_replay_rejects_readonly_terminal_mismatch() -> anyhow::Result<()> {
    for key in ["HERDR_ENV", "herdr_pane_id"] {
        let output = terminal_replay_probe(&format!("declare -rx {key}=stale\n"), &HashMap::new())?;
        assert!(
            !output.status.success(),
            "readonly mismatch launched user command: {output:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "user command ran after rejected replay"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn snapshot_replay_removes_mixed_case_terminal() -> anyhow::Result<()> {
    let output = terminal_replay_probe(
        "export herdr_pane_id=stale\nexport ORDINARY=preserved\n",
        &HashMap::new(),
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout)?, "absent:preserved\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn snapshot_replay_preserves_mixed_case_local_terminal() -> anyhow::Result<()> {
    let env = HashMap::from([("herdr_pane_id".to_string(), "local-owner".to_string())]);
    let output = terminal_replay_probe(
        "export herdr_pane_id=stale\nexport ORDINARY=preserved\n",
        &env,
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout)?, "local-owner:preserved\n");
    let output = terminal_replay_probe(
        "declare -rx herdr_pane_id=local-owner\nexport ORDINARY=preserved\n",
        &env,
    )?;
    assert!(
        output.status.success(),
        "matching readonly value must remain usable: {output:?}"
    );
    assert_eq!(String::from_utf8(output.stdout)?, "local-owner:preserved\n");
    Ok(())
}

#[cfg(unix)]
fn terminal_replay_probe(
    snapshot_text: &str,
    env: &HashMap<String, String>,
) -> anyhow::Result<std::process::Output> {
    use crate::shell::{Shell, ShellType};
    use crate::tools::runtimes::{RuntimePathPrepends, maybe_wrap_shell_lc_with_snapshot};
    use codex_utils_absolute_path::AbsolutePathBuf;
    let root = tempfile::tempdir()?;
    let path = root.path().join("snapshot.sh");
    std::fs::write(&path, snapshot_text)?;
    let path = AbsolutePathBuf::try_from(path)?;
    let shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: "/bin/bash".into(),
    };
    let command = shell.derive_exec_args(
        r#"printf '%s:%s\n' "${herdr_pane_id-absent}" "${ORDINARY-missing}""#,
        /*use_login_shell*/ true,
    );
    let wrapped = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &shell,
        Some(&path),
        &HashMap::new(),
        env,
        &RuntimePathPrepends::default(),
    );
    Ok(std::process::Command::new(&wrapped[0])
        .args(&wrapped[1..])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(env)
        .output()?)
}

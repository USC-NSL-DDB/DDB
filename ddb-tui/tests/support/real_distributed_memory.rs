use super::*;

#[test]
#[ignore = "requires real_dbt fixture, sibling DDB binary, GDB and a PTY"]
fn real_gdb_distributed_caller_memory() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    workflow("gdb");
}

#[test]
#[ignore = "requires real_dbt fixture, sibling DDB binary, LLDB and a PTY"]
fn real_lldb_distributed_caller_memory() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    workflow("lldb");
}

fn execute(ddb: &Ddb, session: &str, action: v2::ExecutionAction) {
    ddb.runtime.block_on(async {
        let admitted = ddb
            .client
            .execute(v2::ExecuteRequest {
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: session.into(),
                    })),
                }),
                action: action as i32,
                ..Default::default()
            })
            .await
            .unwrap()
            .operation
            .unwrap();
        let completed = ddb
            .client
            .wait_operation(admitted.operation_id, TIMEOUT, POLL)
            .await
            .unwrap();
        assert_eq!(
            completed.state,
            v2::OperationState::Completed as i32,
            "{completed:?}"
        );
    });
}

fn context(ddb: &Ddb, thread: &str) -> Vec<(&'static str, u64)> {
    let frame = ddb
        .runtime
        .block_on(ddb.client.list_frames(v2::ListFramesRequest {
            thread_id: thread.into(),
            ..Default::default()
        }))
        .unwrap()
        .frames
        .remove(0);
    let registers = if cfg!(target_arch = "aarch64") {
        vec![("pc", "pc"), ("sp", "sp"), ("fp", "x29"), ("lr", "x30")]
    } else {
        vec![("pc", "rip"), ("sp", "rsp"), ("fp", "rbp")]
    };
    registers
        .into_iter()
        .map(|(alias, name)| {
            let value = ddb.register_value(&frame.frame_id, name, v2::RegisterFormat::Hexadecimal);
            (
                alias,
                u64::from_str_radix(value.trim_start_matches("0x"), 16).unwrap(),
            )
        })
        .collect()
}

fn spawn(backend: &str) -> Ddb {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = manifest.join("../ddb/core/tests/fixtures/real_dbt/target/debug/DDB");
    assert!(
        binary.is_file(),
        "Build ddb/core/tests/fixtures/real_dbt/Cargo.toml first"
    );
    let directory = tempfile::tempdir().unwrap();
    let _guard = ddb_start_guard();
    let port = reserve_port();
    let mut config = format!(
        r#"Framework: grpc
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: {port}
  api_insecure_allow_unauthenticated_v2: true
  base_dir: "{}/state"
  log_dir: "{}/logs"
  Debugger:
    backend: {backend}
StaticSessions:
"#,
        directory.path().display(),
        directory.path().display()
    );
    for (role, alias) in [(1, "dbt-root"), (2, "dbt-leaf")] {
        let mut args = vec![
            "--logical-pid".to_string(),
            (9500 + role).to_string(),
            "--role-index".into(),
            role.to_string(),
            "--self-ctx-file".into(),
            directory
                .path()
                .join(format!("ctx-{role}"))
                .display()
                .to_string(),
        ];
        if role == 2 {
            args.extend([
                "--parent-ctx-file".into(),
                directory.path().join("ctx-1").display().to_string(),
                "--caller-ip".into(),
                "127.0.0.1".into(),
                "--caller-pid".into(),
                "9501".into(),
                "--caller-tid".into(),
                "1".into(),
            ]);
        }
        config.push_str(&format!(
            r#"  - tag: "127.0.0.1:-{}"
    alias: "{alias}"
    hash: "tui-distributed-memory"
    pid: {}
    ip: "127.0.0.1"
    start_delay_ms: 0
    start_mode: binary
    binary_path: {}
    stop_at_entry: false
    binary_args: {}
"#,
            9500 + role,
            9500 + role,
            serde_json::to_string(&binary).unwrap(),
            serde_json::to_string(&args).unwrap()
        ));
    }
    let environment =
        (backend == "lldb").then_some(("FAKETIME", "-00000000000000000000.000000000"));
    Ddb::spawn_config(manifest, directory, port, &config, environment.as_slice())
}

fn click_text(tui: &mut Tui, text: &str) {
    tui.wait_for_current_frame(|screen| screen.contains(text));
    let (column, row) = tui
        .frames
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            line.find(text)
                .map(|offset| (line[..offset].chars().count() + 1, row + 1))
        })
        .unwrap();
    tui.send(format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes());
}

fn reject_selected_memory(tui: &mut Tui) {
    // z follows M in the same input stream. If M wrongly changes focus, this
    // expands Memory instead of Variables, even if an old error remains visible.
    tui.send(b"Mz");
    tui.wait_for_current_frame(|screen| {
        screen.contains("[Restore]")
            && screen.contains("Variables / Watches")
            && !screen.contains("Raw Memory")
            && screen.contains("Selected value has no memory address")
    });
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| !screen.contains("[Restore]"));
}

fn workflow(backend: &str) {
    let mut ddb = spawn(backend);
    let snapshot = ddb.wait_for_snapshot(|snapshot| {
        snapshot.sessions.len() == 2
            && snapshot.sessions.iter().any(|session| {
                session.tag == "127.0.0.1:-9501"
                    && session.status == v2::SessionStatus::Stopped as i32
            })
    });
    let root = snapshot
        .sessions
        .iter()
        .find(|session| session.tag == "127.0.0.1:-9501")
        .unwrap()
        .session_id
        .clone();
    let leaf = snapshot
        .sessions
        .iter()
        .find(|session| session.tag == "127.0.0.1:-9502")
        .unwrap()
        .session_id
        .clone();
    let snapshot = ddb.wait_for_snapshot(|snapshot| {
        snapshot.threads.iter().any(|thread| {
            thread.session_id == root && thread.state == v2::ThreadState::Stopped as i32
        })
    });
    let root_thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.session_id == root)
        .unwrap()
        .thread_id
        .clone();
    let saved = context(&ddb, &root_thread);
    assert!(saved
        .iter()
        .filter(|(name, _)| matches!(*name, "pc" | "sp"))
        .all(|(_, value)| *value != 0));
    let mut handoff = tempfile::NamedTempFile::new_in(ddb._tempdir.path()).unwrap();
    for (name, value) in &saved {
        writeln!(handoff, "{name}={value}").unwrap();
    }
    handoff.persist(ddb._tempdir.path().join("ctx-1")).unwrap();
    execute(&ddb, &root, v2::ExecutionAction::Continue);
    let snapshot = ddb.wait_for_snapshot(|snapshot| {
        snapshot.threads.iter().any(|thread| {
            thread.session_id == leaf && thread.state == v2::ThreadState::Stopped as i32
        })
    });
    let leaf_thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.session_id == leaf)
        .unwrap()
        .thread_id
        .clone();
    execute(&ddb, &root, v2::ExecutionAction::Interrupt);
    ddb.wait_for_snapshot(|snapshot| {
        snapshot.threads.iter().any(|thread| {
            thread.thread_id == root_thread && thread.state == v2::ThreadState::Stopped as i32
        })
    });
    assert_ne!(
        context(&ddb, &root_thread)[0].1,
        saved[0].1,
        "caller must move beyond the saved context"
    );

    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("DDB Debugger");
    tui.click_topology_thread("dbt-leaf");
    let snapshot = ddb.wait_for_snapshot(|snapshot| {
        snapshot.operations.iter().any(|operation| {
            let Some(v2::operation_result::Value::DistributedBacktrace(stack)) = operation
                .result
                .as_ref()
                .and_then(|result| result.value.as_ref())
            else {
                return false;
            };
            !stack.truncated
                && stack
                    .frames
                    .first()
                    .is_some_and(|frame| frame.session_id == leaf && frame.thread_id == leaf_thread)
                && stack.frames.iter().any(|frame| frame.boundary.is_some())
                && stack.frames.iter().any(|frame| {
                    frame.session_id == root
                        && frame.thread_id == root_thread
                        && frame.frame.as_ref().is_some_and(|frame| {
                            frame
                                .function_name
                                .as_deref()
                                .is_some_and(|name| name.contains("root_stop"))
                                && frame.level > 0
                        })
                })
                && stack
                    .frames
                    .iter()
                    .filter(|frame| frame.session_id == root)
                    .filter_map(|frame| frame.frame.as_ref()?.location.as_ref()?.address.as_deref())
                    .any(|address| {
                        u64::from_str_radix(address.trim_start_matches("0x"), 16) == Ok(saved[0].1)
                    })
        })
    });
    assert_ne!(root_thread, leaf_thread);
    assert!(snapshot
        .threads
        .iter()
        .filter(|thread| thread.session_id == root || thread.session_id == leaf)
        .all(|thread| thread.state == v2::ThreadState::Stopped as i32));
    click_text(&mut tui, "DDB Distributed Call Stack");
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| {
        screen.contains("[Restore]")
            && screen.contains("root_stop")
            && screen.contains("dbt-root")
            && screen.contains("dbt-leaf")
    });
    click_text(&mut tui, "root_stop");
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| {
        !screen.contains("[Restore]") && screen.contains("memory_marker :")
    });
    tui.expand_visible_variable("memory_marker");
    // GDB may expose a C++ access-group node even for this Rust repr(C) struct.
    tui.wait_for_current_frame(|screen| {
        screen.lines().any(|line| {
            let variables = line.chars().skip(105).collect::<String>();
            variables.contains("value :") || variables.contains("▸ public")
        })
    });
    if tui
        .frames
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .lines()
        .any(|line| {
            line.chars()
                .skip(105)
                .collect::<String>()
                .contains("▸ public")
        })
    {
        tui.expand_visible_variable("public");
    }
    tui.wait_for_variable_value("value", "9501");
    tui.select_visible_variable("value");
    tui.send(b"M");
    tui.wait_for_current_frame(|screen| screen.contains("Raw Memory · 256 B"));
    tui.send(b"z");
    let bytes = 9501_u64
        .to_ne_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    tui.wait_for_current_frame(|screen| screen.contains("[Restore]") && screen.contains(&bytes));
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| !screen.contains("[Restore]"));
    tui.send(b"w9501\r");
    tui.wait_for_variable_value("Watch: 9501", "9501");
    tui.select_visible_variable("Watch: 9501");
    reject_selected_memory(&mut tui);
    tui.send(b"w_ddb_missing\r");
    tui.wait_for_variable_value("Watch: _ddb_missing", "Unavailable:");
    tui.select_visible_variable("Watch: _ddb_missing");
    reject_selected_memory(&mut tui);
    assert!(tui.quit().contains("\u{1b}[?1049l"));

    // The same reconstructed caller must disappear only by explicit choice.
    // Local mode still uses real frame handles for source and variable inspection.
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
    let mut local = Tui::spawn_shell(&format!(
        "stty rows 40 cols 140; exec {} --stack-mode local connect --api {} --refresh-ms 10000",
        shell_quote(&binary),
        shell_quote(Path::new(&ddb.base_url()))
    ));
    local.click_topology_thread("dbt-leaf");
    click_text(&mut local, "DDB Local Call Stack");
    local.send(b"z");
    local.wait_for_current_frame(|screen| {
        screen.contains("[Restore]") && screen.contains("dbt-leaf") && screen.contains("extraction")
    });
    let screen = local.frames.lock().unwrap().back().unwrap().clone();
    assert!(!screen.contains("root_stop"));
    assert!(!screen.contains("dbt-root"));
    assert!(!screen.contains("DDB Distributed Call Stack"));
    local.send(b"z");
    let line = ddb.top_frame_line(&leaf_thread);
    local.wait_for_source_marker('▶', line);
    local.send(b"w9502\r");
    local.wait_for_variable_value("Watch: 9502", "9502");
    assert!(local.quit().contains("\u{1b}[?1049l"));
}

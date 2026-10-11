#![cfg(unix)]

use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use ddb_api_client::{v2, ClientConfig, DdbClient};
use serde_json::Value;
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

static REAL_DEBUGGER_LOCK: Mutex<()> = Mutex::new(());
static DDB_START_LOCK: Mutex<()> = Mutex::new(());

#[path = "support/real_distributed_memory.rs"]
mod real_distributed_memory;

struct OwnedTestProcess(Option<Child>);
impl Drop for OwnedTestProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            // A ptraced child may not be reapable until the debugger drops.
            // Do not prevent the following DDB guard from running cleanup.
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[test]
#[ignore = "requires DDB_GREETER_WORKSPACE, instrumented greeters, ddb_runapp and a free configured loopback discovery port"]
fn greeter_auto_attach_all_stop_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK.lock().unwrap();
    let workspace = PathBuf::from(
        std::env::var_os("DDB_GREETER_WORKSPACE").expect("set DDB_GREETER_WORKSPACE"),
    );
    let discovery = fs::read_to_string("/tmp/ddb/service_discovery/config").unwrap();
    let mut lines = discovery.lines();
    let endpoint = lines
        .next()
        .unwrap()
        .strip_prefix("tcp://127.0.0.1:")
        .expect("test requires loopback discovery");
    let broker_port: u16 = endpoint.parse().unwrap();
    assert_eq!(lines.next(), Some("service_discovery/report"));
    let reserved = TcpListener::bind(("127.0.0.1", broker_port))
        .expect("discovery port is occupied; do not disturb an existing broker");
    let directory = tempfile::tempdir().unwrap();
    let broker_config = directory.path().join("mosquitto.conf");
    fs::write(
        &broker_config,
        format!("listener {broker_port} 127.0.0.1\nallow_anonymous true\npersistence false\n"),
    )
    .unwrap();
    drop(reserved);
    let mut broker = OwnedTestProcess(Some(
        Command::new("mosquitto")
            .arg("-c")
            .arg(&broker_config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + TIMEOUT;
    while std::net::TcpStream::connect(("127.0.0.1", broker_port)).is_err() {
        assert!(broker.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "broker did not start");
        thread::sleep(POLL);
    }
    let port = reserve_port();
    let config = format!("Framework: grpc\nFrameFilter:\n  filter_preset: [cpp-stdlib, protobuf-gen, ddb-runtime]\nServiceDiscovery:\n  Broker:\n    hostname: 127.0.0.1\n    port: {broker_port}\nConf:\n  auto_shutdown: false\n  on_exit: kill\n  api_server_port: {port}\n  api_insecure_allow_unauthenticated_v2: true\n  base_dir: {}/state\n  log_dir: {}/logs\n  Debugger:\n    backend: gdb\n", directory.path().display(), directory.path().display());
    let app_port = reserve_port();
    let spawn_app = |role: &str, argument: String| {
        OwnedTestProcess(Some(
            Command::new("ddb_runapp")
                .arg(workspace.join("build").join(format!("greeter_{role}")))
                .arg("--ddb")
                .arg(format!("--ddb_proc_alias=tui-{role}"))
                .arg(argument)
                .current_dir(workspace.join("build"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ))
    };
    let mut ddb = Ddb::spawn_config(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        directory,
        port,
        &config,
        &[],
    );
    let mut tui = Tui::spawn(&ddb.base_url());
    let _server = spawn_app("server", format!("--port={app_port}"));
    ddb.wait_for_snapshot(|s| s.sessions.len() == 1 && !s.threads.is_empty());
    tui.click_topology_thread("tui-server");
    let server_source = workspace.join("greeter_server.cc");
    let server_line = fs::read_to_string(&server_source)
        .unwrap()
        .lines()
        .position(|line| line.contains("reply->set_message"))
        .unwrap()
        + 1;
    tui.send(format!("g{}:{server_line}\r", server_source.display()).as_bytes());
    tui.wait_for_source_marker('▸', server_line);
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    ddb.wait_for_snapshot(|s| s.breakpoints.len() == 1 && s.breakpoints[0].verified);
    tui.send(b"\x1b[15~");
    ddb.wait_for_snapshot(|s| {
        s.threads
            .iter()
            .any(|t| t.state == v2::ThreadState::Running as i32)
    });
    let deadline = Instant::now() + TIMEOUT;
    while std::net::TcpStream::connect(("127.0.0.1", app_port)).is_err() {
        assert!(Instant::now() < deadline, "greeter server did not listen");
        thread::sleep(POLL);
    }
    let _client = spawn_app("client", format!("--target=127.0.0.1:{app_port}"));
    ddb.wait_for_snapshot(|s| {
        s.sessions.iter().any(|session| {
            session.display_name.contains("tui-client")
                && s.threads.iter().any(|thread| {
                    thread.session_id == session.session_id
                        && thread.state == v2::ThreadState::Stopped as i32
                })
        })
    });
    // Backend discovery can precede the TUI's event projection. Wait for the
    // rendered membership before scanning: the client sorts above the server
    // and would otherwise be inserted behind an already advancing search.
    tui.wait_for_current_frame(|frame| frame.contains("2 sessions · 2 groups"));
    tui.click_topology_thread("tui-client");
    let client_source = workspace.join("greeter_client.cc");
    let client_line = fs::read_to_string(&client_source)
        .unwrap()
        .lines()
        .position(|line| line.contains("Status status = stub_->SayHello"))
        .unwrap()
        + 1;
    tui.send(format!("g{}:{client_line}\r", client_source.display()).as_bytes());
    tui.wait_for_source_marker('▸', client_line);
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    ddb.wait_for_snapshot(|s| s.breakpoints.len() == 2 && s.breakpoints.iter().all(|b| b.verified));
    tui.send(b"ccc\x1b[15~");
    ddb.wait_for_snapshot(|s| {
        s.threads.iter().any(|t| {
            t.location.as_ref().is_some_and(|l| {
                l.path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("greeter_client.cc"))
                    && l.line == client_line as u32
            })
        }) && s
            .threads
            .iter()
            .all(|t| t.state == v2::ThreadState::Stopped as i32)
    });
    tui.send(b"\x1b[15~");
    ddb.wait_for_snapshot(|s| {
        s.threads.iter().any(|t| {
            t.location.as_ref().is_some_and(|l| {
                l.path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("greeter_server.cc"))
                    && l.line == server_line as u32
            })
        }) && s
            .threads
            .iter()
            .all(|t| t.state == v2::ThreadState::Stopped as i32)
    });
    let stopped = ddb
        .runtime
        .block_on(ddb.client.get_snapshot(v2::GetSnapshotRequest {
            sections: vec![
                v2::SnapshotSection::Topology as i32,
                v2::SnapshotSection::Execution as i32,
                v2::SnapshotSection::Breakpoints as i32,
            ],
            ..Default::default()
        }))
        .unwrap()
        .snapshot
        .unwrap();
    let server = stopped
        .sessions
        .iter()
        .find(|session| session.display_name.contains("tui-server"))
        .unwrap();
    let server_threads = stopped
        .threads
        .iter()
        .filter(|thread| thread.session_id == server.session_id)
        .map(|thread| thread.thread_id.as_str())
        .collect::<Vec<_>>();
    let server_stops = stopped.execution_states.iter().filter(|state| {
        matches!(state.target.as_ref().and_then(|target| target.selector.as_ref()), Some(v2::target::Selector::Thread(target)) if server_threads.contains(&target.thread_id.as_str()))
    }).collect::<Vec<_>>();
    assert!(server_stops.iter().any(|state| {
        let Some(v2::target::Selector::Thread(target)) = state.target.as_ref().and_then(|target| target.selector.as_ref()) else { return false };
        !state.running && state.stop_reason.as_ref().is_some_and(|reason| {
            reason.kind == v2::StopReasonKind::Breakpoint as i32
                && reason.thread_id.as_deref().is_none_or(|owner| owner == target.thread_id)
                && reason.breakpoint_id.as_ref().is_some_and(|id| stopped.breakpoints.iter().any(|bp| bp.breakpoint_id == *id || bp.sub_breakpoints.iter().any(|member| member.sub_breakpoint_id == *id && member.session_id == server.session_id)))
        })
    }), "server breakpoint location must retain a navigable canonical breakpoint stop: {server_stops:#?}");
    tui.wait_for_current_frame(|screen| screen.contains("▶ 1 paused"));
    let hit_row = tui
        .frames
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .lines()
        .position(|line| {
            line.chars()
                .take(33)
                .collect::<String>()
                .contains("▶ 1 paused")
        })
        .expect("server breakpoint should have a current hit")
        + 1;
    tui.send(format!("\x1b[<2;5;{hit_row}M\x1b[<2;5;{hit_row}m").as_bytes());
    tui.wait_for_current_frame(|screen| {
        screen.contains("Paused breakpoint hits") && screen.contains("tui-server (")
    });
    tui.clear_capture();
    tui.send(b"\r");
    tui.wait_for_source_marker('▶', server_line);
    tui.quit();
    ddb.shutdown();
    assert_eq!(
        fs::read_to_string("/tmp/ddb/service_discovery/config").unwrap(),
        discovery
    );
}

fn topology_thread_row(frame: &str, session_label: &str) -> Option<usize> {
    let mut under_session = false;
    frame.lines().enumerate().find_map(|(row, line)| {
        let topology = line.chars().take(33).collect::<String>();
        if topology.contains('◇') {
            under_session = topology.contains(session_label);
            return None;
        }
        (under_session && topology.contains('•')).then_some(row + 1)
    })
}

fn topology_reached_top(current: Option<&Vec<String>>) -> bool {
    // The list renders from offset zero until the selection needs scrolling.
    // Its first visible row can therefore be selected only at the actual top.
    // Identical captures alone do not acknowledge that an input was processed.
    current
        .and_then(|rows| rows.first())
        .is_some_and(|row| row.contains('▸'))
}

#[test]
fn unchanged_topology_capture_is_not_an_acknowledgement_of_page_up() {
    let rows = vec!["│      • 16 stopped".into(), "│▸     • 5 stopped".into()];
    assert!(!topology_reached_top(Some(&rows)));
    assert!(!topology_reached_top(None));
    assert!(topology_reached_top(Some(&vec![
        "│▸▾ client 1 sessions".into()
    ])));
}

#[test]
fn topology_mouse_target_does_not_confuse_a_group_label_with_a_session() {
    let frame = "│  ▾ beta-worker  2 sessions\n│    ◇ alpha-worker (alpha)\n│      • 1 stopped\n│    ◇ beta-worker (beta)\n│      • 1 stopped";
    assert_eq!(topology_thread_row(frame, "beta-worker"), Some(5));
    assert_eq!(topology_thread_row(frame, "alpha-worker"), Some(3));
}

#[test]
#[ignore = "requires the sibling DDB binary and a PTY"]
fn distributed_source_ownership_terminal_workflow() {
    distributed_source_ownership_workflow(false);
}

#[test]
#[ignore = "requires the sibling DDB binary and a PTY"]
fn local_distributed_source_ownership_terminal_workflow() {
    distributed_source_ownership_workflow(true);
}

fn distributed_source_ownership_workflow(local_workspace: bool) {
    let directory = tempfile::tempdir().unwrap();
    for owner in ["alpha", "beta"] {
        let root = directory.path().join(owner);
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("main.rs"),
            (1..=30)
                .map(|line| format!("{owner}_CHECKOUT line {line}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }
    let roots = "  - name: alpha-checkout\n    path: ./alpha\n    debugger_path: /build/app\n    session_tags: [alpha]\n  - name: beta-checkout\n    path: ./beta\n    debugger_path: /build/app\n    session_tags: [beta]\n";
    let workspace_path = directory.path().join("workspace.yaml");
    let exports = if local_workspace {
        fs::write(&workspace_path, format!("roots:\n{roots}")).unwrap();
        String::new()
    } else {
        format!("SourceRoots:\n{roots}")
    };
    let mut ddb = {
        let _guard = ddb_start_guard();
        let port = reserve_port();
        let mut config = format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  api_server_port: {port}
  api_insecure_allow_unauthenticated_v2: true
  base_dir: "{}/state"
  log_dir: "{}/logs"
  Debugger:
    backend: mock
{exports}StaticSessions:
"#,
            directory.path().display(),
            directory.path().display()
        );
        for (index, owner) in ["alpha", "beta"].into_iter().enumerate() {
            config.push_str(&format!(
                r#"  - tag: {owner}
    alias: {owner}-worker
    hash: shared-program
    pid: {}
    start_delay_ms: 0
    mock:
      source_file: /build/app/main.rs
      source_line: 9
      function: {owner}_work
      exit_on_continue: false
      exit_on_bootstrap: false
"#,
                1900 + index
            ));
        }
        Ddb::spawn_config(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            directory,
            port,
            &config,
            &[],
        )
    };
    let snapshot = ddb
        .wait_for_snapshot(|snapshot| snapshot.sessions.len() == 2 && snapshot.threads.len() == 2);
    let alpha = snapshot
        .sessions
        .iter()
        .find(|session| session.display_name.contains("alpha-worker"))
        .unwrap()
        .session_id
        .clone();
    let beta = snapshot
        .sessions
        .iter()
        .find(|session| session.display_name.contains("beta-worker"))
        .unwrap()
        .session_id
        .clone();
    let alpha_thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.session_id == alpha)
        .unwrap()
        .thread_id
        .clone();
    let beta_thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.session_id == beta)
        .unwrap()
        .thread_id
        .clone();
    let mut tui = if local_workspace {
        Tui::spawn_with_workspace(&ddb.base_url(), &workspace_path)
    } else {
        Tui::spawn(&ddb.base_url())
    };
    let root_kind = if local_workspace { "Local" } else { "DDB" };
    let file_kind = if local_workspace { "local" } else { "file" };
    tui.wait_for("2 sessions");
    // Exercise a prior visit: later waits must not accept beta's old content.
    tui.click_topology_thread("beta-worker");
    tui.wait_for_frame(|frame| {
        frame
            .lines()
            .any(|line| line.contains("9 │ beta_CHECKOUT line 9") && line.contains('▶'))
    });
    tui.click_topology_thread("alpha-worker");
    tui.wait_for("alpha_CHECKOUT line 9");
    tui.wait_for_source_marker('▶', 9);

    // Open beta's independently configured root while alpha remains selected.
    tui.send(b"o");
    tui.wait_for(&format!("[{root_kind}] beta-checkout"));
    tui.send(b"beta-checkout\r");
    tui.wait_for(&format!("[{file_kind}] beta-checkout/main.rs"));
    tui.send(b"\r");
    tui.wait_for_current_frame(|frame| {
        frame.contains("beta_CHECKOUT line 1") && !frame.contains("Source explorer")
    });
    assert!(
        tui.frames
            .lock()
            .unwrap()
            .back()
            .is_some_and(|frame| frame.contains("beta_CHECKOUT line 1")),
        "retained terminal text was mistaken for the currently displayed beta document"
    );
    tui.clear_capture();
    tui.send(b"g9\r");
    tui.wait_for_frame(|frame| {
        frame.lines().any(|line| {
            line.contains("9 │ beta_CHECKOUT line 9")
                && line.contains('▸')
                && line.contains('◆')
                && !line.contains('▶')
        })
    });
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    let snapshot = ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.len() == 1);
    let breakpoint = &snapshot.breakpoints[0];
    assert!(
        matches!(breakpoint.target.as_ref().and_then(|target| target.selector.as_ref()), Some(v2::target::Selector::Session(target)) if target.session_id == beta)
    );
    assert!(
        matches!(breakpoint.spec.as_ref().and_then(|spec| spec.location.as_ref()), Some(v2::breakpoint_spec::Location::Source(location)) if location.source == "/build/app/main.rs")
    );

    // Stepping still applies to alpha. Beta's document/cursor remain on screen.
    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    assert_eq!(ddb.wait_for_top_frame_line_change(&alpha_thread, 9), 10);
    assert_eq!(ddb.top_frame_line(&beta_thread), 9);
    tui.wait_for_frame(|frame| {
        frame.lines().any(|line| {
            line.contains("9 │ beta_CHECKOUT line 9")
                && line.contains('●')
                && line.contains('▸')
                && !line.contains('▶')
        })
    });
    tui.clear_capture();
    tui.send(b"b");
    ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.is_empty());
    tui.send(b"f");
    tui.wait_for("alpha_CHECKOUT line 10");
    tui.wait_for_source_marker('▶', 10);
    tui.quit();
    ddb.shutdown();
}

#[test]
#[ignore = "requires the sibling DDB binary and a PTY"]
fn source_explorer_terminal_workflow_without_sessions() {
    source_explorer_workflow_without_sessions(false);
}

#[test]
#[ignore = "requires the sibling DDB binary and a PTY"]
fn local_source_explorer_terminal_workflow_without_sessions() {
    source_explorer_workflow_without_sessions(true);
}

fn source_explorer_workflow_without_sessions(local_workspace: bool) {
    let directory = tempfile::tempdir().unwrap();
    let checkout = directory.path().join("checkout");
    fs::create_dir_all(checkout.join("src")).unwrap();
    fs::write(
        checkout.join("src/notes.txt"),
        "outside debug symbols\nsecond line\n",
    )
    .unwrap();
    fs::write(checkout.join(".hidden"), "hidden workspace file\n").unwrap();
    fs::write(
        checkout.join("src/zlong.txt"),
        (1..=1000)
            .map(|line| format!("large file line {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let workspace_path = directory.path().join("workspace.yaml");
    let roots = "  - name: checkout\n    path: ./checkout\n";
    let exports = if local_workspace {
        fs::write(&workspace_path, format!("roots:\n{roots}")).unwrap();
        String::new()
    } else {
        format!("SourceRoots:\n{roots}")
    };
    let mut ddb = {
        let _guard = ddb_start_guard();
        let port = reserve_port();
        let config = format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  api_server_port: {port}
  api_insecure_allow_unauthenticated_v2: true
  base_dir: "{}/state"
  log_dir: "{}/logs"
  Debugger:
    backend: mock
{exports}StaticSessions: []
"#,
            directory.path().display(),
            directory.path().display()
        );
        Ddb::spawn_config(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            directory,
            port,
            &config,
            &[],
        )
    };
    let mut tui = if local_workspace {
        Tui::spawn_with_workspace(&ddb.base_url(), &workspace_path)
    } else {
        Tui::spawn(&ddb.base_url())
    };
    let root_kind = if local_workspace { "Local" } else { "DDB" };
    let file_kind = if local_workspace { "local" } else { "file" };
    tui.wait_for("connected");
    tui.send(b"o");
    tui.wait_for(&format!("[{root_kind}] checkout"));
    tui.send(b"\r");
    tui.wait_for("[dir] src");
    tui.send(b"\r");
    tui.wait_for("notes.txt");
    tui.send(b"\r");
    tui.wait_for("outside debug symbols");
    tui.wait_for_source_marker('▸', 1);
    tui.send(b"g2\r");
    tui.wait_for_source_marker('▸', 2);
    tui.clear_capture();
    tui.send(b"o");
    tui.wait_for(&format!("[{root_kind}] checkout"));
    tui.send(b"\r");
    tui.wait_for("[dir] src");
    // Ctrl-H toggles dotfiles; typing filters the loaded directory page.
    tui.send(b"\x08.hidden");
    tui.wait_for(&format!("[{file_kind}] checkout/.hidden"));
    tui.send(b"\r");
    tui.wait_for("hidden workspace file");
    tui.clear_capture();
    tui.send(b"\x1b[1;3D");
    tui.wait_for("outside debug symbols");
    tui.wait_for_source_marker('▸', 2);
    tui.send(b"\x10");
    tui.wait_for("Debugger source catalog");
    tui.wait_for("No matching debugger sources");
    tui.clear_capture();
    tui.send(b"\x1b");
    tui.wait_for_frame(|frame| {
        frame.contains("outside debug symbols") && !frame.contains("Source explorer")
    });
    tui.clear_capture();
    tui.send(b"o");
    tui.wait_for(&format!("[{root_kind}] checkout"));
    tui.send(b"\r");
    tui.wait_for("[dir] src");
    tui.send(b"\r");
    tui.wait_for("zlong.txt");
    tui.send(b"zlong\r");
    tui.wait_for("large file line 1");
    tui.send(b"j");
    tui.wait_for_source_marker('▸', 2);
    tui.send(b"k");
    tui.wait_for_source_marker('▸', 1);
    // At 140x40 the source pane contains twenty text rows, not a fixed
    // ten-line paging step. Repeated keys also cross source-window loads.
    let page_destination = 1 + 24 * 20;
    tui.send(&b"\x1b[6~".repeat(24));
    tui.wait_for_source_marker('▸', page_destination);
    // Crossing a source read boundary must not reset the cursor to row eight.
    // Source content occupies terminal rows 5..24 in this fixed-size fixture.
    tui.wait_for_current_frame(|frame| {
        frame
            .lines()
            .nth(23)
            .is_some_and(|row| row.contains(&format!("▸ {page_destination:>5} │")))
    });
    tui.clear_capture();
    tui.send(b"\x1b[1;3D");
    tui.wait_for("outside debug symbols");
    tui.wait_for_source_marker('▸', 2);
    assert!(ddb
        .wait_for_snapshot(|snapshot| snapshot.sessions.is_empty())
        .sessions
        .is_empty());
    ddb.shutdown();
    if local_workspace {
        // Local discovery and reads remain usable after the backend is gone.
        tui.clear_capture();
        tui.send(b"o");
        tui.wait_for("[Local] checkout");
        tui.send(b"\r");
        tui.wait_for("[dir] src");
        tui.send(b"\r");
        tui.wait_for("zlong.txt");
        tui.send(b"zlong\r");
        tui.wait_for(&format!("large file line {page_destination}"));
        tui.wait_for_source_marker('▸', page_destination);
    }
    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn watch_file_survives_terminal_restart_and_tracks_edits_and_deletions() {
    let mut ddb = Ddb::spawn_mock();
    ddb.wait_for_snapshot(|snapshot| snapshot.sessions.len() == 1);
    let directory = tempfile::tempdir().unwrap();
    let watch_file = directory.path().join("watches.json");
    let spawn = || {
        Tui::spawn_shell(&format!(
            "stty rows 40 cols 140; exec {} --watch-file {} connect --api {} --refresh-ms 10000",
            shell_quote(Path::new(env!("CARGO_BIN_EXE_ddb-tui"))),
            shell_quote(&watch_file),
            shell_quote(Path::new(&ddb.base_url())),
        ))
    };
    let mut first = spawn();
    first.wait_for("counter");
    first.send(b"wcounter\r");
    first.wait_for_current_frame(|screen| screen.contains("Watch: counter"));
    first.send(b"W + 1\r");
    first.wait_for_current_frame(|screen| screen.contains("Watch: counter + 1"));
    // Verify autosave while the first frontend is still alive, not just on exit.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let saved = fs::read(&watch_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if saved
            .as_ref()
            .is_some_and(|json| json["expressions"] == serde_json::json!(["counter + 1"]))
        {
            break;
        }
        assert!(Instant::now() < deadline, "watch edits were not saved");
        thread::sleep(POLL);
    }
    assert!(first.quit().contains("\u{1b}[?1049l"));
    let mut second = spawn();
    second.wait_for_current_frame(|screen| {
        screen.contains("Watch: counter + 1") && !screen.contains("Waiting for a stopped frame")
    });
    second.select_visible_variable("Watch: counter + 1");
    second.send(b"\x1b[3~");
    second.wait_for_current_frame(|screen| !screen.contains("Watch: counter + 1"));
    assert!(second.quit().contains("\u{1b}[?1049l"));
    let saved: Value = serde_json::from_slice(&fs::read(&watch_file).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({"version":1,"expressions":[]}));
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn topology_details_export_terminal_workflow() {
    let mut ddb = Ddb::spawn_mock();
    let snapshot = ddb.wait_for_snapshot(|snapshot| snapshot.sessions.len() == 1);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("details.json");
    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("counter");
    tui.click_topology_thread(&snapshot.sessions[0].display_name);
    tui.send(b"i");
    tui.wait_for_current_frame(|screen| {
        screen.contains("Topology details") && screen.contains("[e Export]")
    });
    assert!(!tui
        .output
        .lock()
        .unwrap()
        .windows(5)
        .any(|bytes| bytes == b"\x1b]52;"));
    tui.send(b"y");
    tui.wait_for_current_frame(|screen| screen.contains("Copy sent to terminal"));
    let copied = {
        use base64::Engine;
        let output = tui.output.lock().unwrap();
        let prefix = b"\x1b]52;c;";
        let start = output
            .windows(prefix.len())
            .position(|bytes| bytes == prefix)
            .unwrap()
            + prefix.len();
        let end = output[start..]
            .windows(2)
            .position(|bytes| bytes == b"\x1b\\")
            .unwrap()
            + start;
        base64::prelude::BASE64_STANDARD
            .decode(&output[start..end])
            .unwrap()
    };
    let copied_json: Value = serde_json::from_slice(&copied).unwrap();
    assert_eq!(copied_json["serverInstanceId"], snapshot.server_instance_id);
    assert_eq!(
        copied_json["sessions"][0]["sessionId"],
        snapshot.sessions[0].session_id
    );
    tui.send(b"e");
    tui.wait_for_current_frame(|screen| screen.contains("Export snapshot to NEW file"));
    tui.send(path.to_str().unwrap().as_bytes());
    tui.send(b"\x1b");
    tui.wait_for_current_frame(|screen| !screen.contains("Export snapshot to NEW file"));
    assert!(!path.exists());
    tui.send(b"i");
    tui.wait_for_current_frame(|screen| screen.contains("Topology details"));
    tui.send(b"e");
    tui.wait_for_current_frame(|screen| screen.contains("Export snapshot to NEW file"));
    tui.send(format!("{}\r", path.display()).as_bytes());
    tui.wait_for_current_frame(|screen| screen.contains("Details exported to"));
    let saved = fs::read(&path).unwrap();
    let document: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(document["serverInstanceId"], snapshot.server_instance_id);
    assert_eq!(
        document["sessions"][0]["sessionId"],
        snapshot.sessions[0].session_id
    );
    assert_eq!(document["selection"], "session");
    tui.send(b"i");
    tui.wait_for_current_frame(|screen| screen.contains("Topology details"));
    tui.send(b"e");
    tui.wait_for_current_frame(|screen| screen.contains("Export snapshot to NEW file"));
    tui.send(format!("{}\r", path.display()).as_bytes());
    tui.wait_for_current_frame(|screen| screen.contains("never overwritten"));
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(tui.quit().contains("\u{1b}[?1049l"));
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn mock_backend_full_terminal_workflow() {
    let mut ddb = Ddb::spawn_mock();
    ddb.wait_for_snapshot(|snapshot| snapshot.sessions.len() == 1);

    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("DDB Debugger");
    tui.wait_for("mock_session");
    tui.wait_for("counter");
    tui.wait_for_source_marker('▶', 9);

    // Switch presentation with the rendered header control, then restore it
    // with the keyboard. Neither operation should move the stopped frame.
    tui.wait_for_current_frame(|screen| screen.contains("Flat [v]"));
    let (column, row) = tui
        .frames
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            line.find("Flat [v]")
                .map(|byte| (line[..byte].chars().count() + 1, row + 1))
        })
        .unwrap();
    tui.send(format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes());
    tui.wait_for_current_frame(|screen| {
        screen.contains("Group [v]") && screen.contains("mock_session")
    });
    tui.wait_for_source_marker('▶', 9);
    tui.send(b"v");
    tui.wait_for_current_frame(|screen| screen.contains("Flat [v]"));
    // Confirmation is safe by default: Enter cancels without a signal.
    tui.send(b"K");
    tui.wait_for_current_frame(|screen| screen.contains("Terminate one session?"));
    tui.wait_for_current_frame(|screen| screen.contains("SIGKILL terminates this process"));
    tui.send(b"\r");
    tui.wait_for_current_frame(|screen| !screen.contains("Terminate one session?"));
    tui.wait_for_source_marker('▶', 9);
    // Threads -> Breakpoints -> Source.
    tui.send(b"\t\t");

    // Use rendered button coordinates, including Unicode border cell widths,
    // to exercise the same mouse path as a user clicking the source pane.
    for (label, expanded) in [("[Expand]", true), ("[Restore]", false)] {
        let locate = |screen: &str| {
            screen.lines().enumerate().find_map(|(row, line)| {
                let cells = line.chars().collect::<Vec<_>>();
                let needle = label.chars().collect::<Vec<_>>();
                cells
                    .windows(needle.len())
                    .enumerate()
                    .find_map(|(column, text)| {
                        (text == needle && (label == "[Restore]" || (33..105).contains(&column)))
                            .then_some((column + 1, row + 1))
                    })
            })
        };
        tui.wait_for_current_frame(|screen| locate(screen).is_some());
        let (column, row) = locate(tui.frames.lock().unwrap().back().unwrap()).unwrap();
        tui.send(format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes());
        tui.wait_for_current_frame(|screen| {
            screen.contains("[Restore]") == expanded
                && if expanded {
                    !screen.contains("Breakpoints")
                } else {
                    screen.contains("Breakpoints")
                }
        });
    }

    tui.send(b"Bif counter == 42\r");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("✓ breakpoint");
    ddb.wait_for_snapshot(|snapshot| {
        snapshot.breakpoints.len() == 1
            && snapshot.breakpoints[0]
                .spec
                .as_ref()
                .and_then(|spec| spec.condition.as_deref())
                == Some("counter == 42")
    });

    // Source -> Stack -> Variables -> Registers -> Timeline -> Threads -> Breakpoints.
    tui.clear_capture();
    tui.send(b"\t\t\t\t\t\t");
    tui.wait_for("\u{25c6} DDB Breakpoints");
    tui.send(b"v");
    tui.wait_for_current_frame(|screen| screen.contains("File: "));
    tui.send(b"x");
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .breakpoints
            .first()
            .and_then(|breakpoint| breakpoint.spec.as_ref())
            .and_then(|spec| spec.enabled)
            == Some(false)
    });
    tui.wait_for("○ ");
    tui.send(b"x");
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .breakpoints
            .first()
            .and_then(|breakpoint| breakpoint.spec.as_ref())
            .and_then(|spec| spec.enabled)
            == Some(true)
    });
    tui.send(b"v");
    tui.wait_for_current_frame(|screen| !screen.contains("File: "));
    tui.send(b"\t");

    tui.send(b"m0x1000 ; 256\r");
    tui.wait_for("Raw Memory");
    tui.wait_for("Raw Memory · 256 B");
    // Focus belongs to Memory after submission. Expansion shows full fixed
    // address/hex/ASCII rows; paging reaches the final row without another read.
    tui.send(b"z");
    tui.wait_for("|*...............|");
    tui.wait_for_current_frame(|screen| {
        screen.contains("[Restore]") && screen.contains("00000000000010f0")
    });
    tui.send(b"z\x1b[6~\x1b[6~\x1b[6~\x1b[6~");
    tui.wait_for_current_frame(|screen| {
        screen.contains("00000000000010f0") && !screen.contains("[Restore]")
    });

    tui.send(b"e counter\r");
    tui.wait_for("evaluate counter: completed");

    tui.send(b":print counter\r");
    tui.wait_for("print counter: completed");

    tui.send(b"Japi_v1.rs:10\r");
    tui.wait_for("jump to api_v1.rs:10");
    tui.send(b"s");
    tui.wait_for("DDB signal catalog");
    tui.wait_for("SIGUSR1");
    tui.send(b"\x1b[B\x1b[B\r");
    tui.wait_for("signal SIGUSR1");

    tui.clear_capture();
    tui.send(b"\x1b[15~");
    tui.wait_for("continue: completed");
    tui.wait_for("exec · running");
    tui.wait_for_frame(|frame| {
        frame.contains("exec · running")
            && frame.contains("▸     9 │")
            && !frame.lines().skip(3).any(|line| line.contains('▶'))
    });
    tui.clear_capture();
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .threads
            .iter()
            .any(|thread| thread.state == v2::ThreadState::Stopped as i32)
    });
    // A stopped event precedes the asynchronous frame/source inspection.
    // Synchronize on the rendered execution location before acting on source.
    tui.wait_for_source_marker('▶', 9);

    // SGR mouse click on the toolbar's Refresh control (terminal coordinates are 1-based).
    tui.send(b"\x1b[<0;90;2M\x1b[<0;90;2m");
    tui.wait_for("1 sessions · 1 groups · 1 breakpoints");

    tui.send(b"b");
    tui.wait_for("delete breakpoint");
    ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.is_empty());

    tui.send(b"g11\r");
    tui.wait_for_source_marker('▸', 11);

    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    ddb.wait_for_snapshot(|snapshot| {
        snapshot.threads.iter().any(|thread| {
            thread.state == v2::ThreadState::Stopped as i32
                && thread
                    .location
                    .as_ref()
                    .is_some_and(|location| location.line == 10)
        })
    });
    tui.wait_for_source_marker('▶', 10);
    tui.wait_for_source_marker('▸', 11);

    tui.clear_capture();
    tui.send(b"f");
    tui.wait_for_source_marker('▸', 10);

    let output = tui.quit();
    assert!(
        output.contains("\u{1b}[?1049l"),
        "TUI did not leave the alternate screen on clean exit"
    );
    assert!(
        output.contains("\u{1b}[?1006l") || output.contains("\u{1b}[?1000l"),
        "TUI did not disable mouse reporting on clean exit"
    );

    ddb.shutdown();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires Linux and the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_backend_crash_is_visible_and_terminal_stays_recoverable() {
    let directory = tempfile::tempdir().expect("backend-crash test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("backend-crash.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, false);
    tui.wait_for("managed DDB");
    let ddb_pid = wait_for_managed_ddb_descendant(tui.child.id());
    assert_eq!(
        unsafe { libc::kill(ddb_pid as libc::pid_t, libc::SIGKILL) },
        0
    );
    wait_for_process_not_running(ddb_pid);
    tui.wait_for("reconnecting");
    let output = tui.quit();
    wait_for_process_exit(ddb_pid);
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(output.contains("managed DDB exited"));
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires Linux and the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_sigterm_restores_terminal_and_stops_owned_ddb() {
    let directory = tempfile::tempdir().expect("SIGTERM test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("sigterm-backend.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, false);
    tui.wait_for("managed DDB");
    let root_pid = tui.child.id();
    let frontend_pid = wait_for_ddb_tui_descendant(root_pid);
    let ddb_pid = wait_for_managed_ddb_descendant(root_pid);
    assert_eq!(
        unsafe { libc::kill(frontend_pid as libc::pid_t, libc::SIGTERM) },
        0
    );
    let output = tui.finish_external_exit();
    wait_for_process_exit(ddb_pid);
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(output.contains("\u{1b}[?1006l") || output.contains("\u{1b}[?1000l"));
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires Linux and the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_panic_restores_terminal_and_stops_owned_ddb() {
    let directory = tempfile::tempdir().expect("panic test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("panic-backend.log");

    let mut tui = Tui::spawn_managed_with_panic_hook(&config, &backend_log);
    tui.wait_for("managed_mock_session");
    let ddb_pid = wait_for_managed_ddb_descendant(tui.child.id());
    let token_path = managed_argument_path(ddb_pid, "--api-auth-token-file");
    let runtime_directory = token_path.parent().unwrap().to_path_buf();
    tui.send(b"P");
    let output = tui.finish_external_exit();

    wait_for_process_exit(ddb_pid);
    wait_for_path_absent(&runtime_directory);
    assert!(output.contains("intentional ddb-tui panic fault injection"));
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(output.contains("\u{1b}[?1006l") || output.contains("\u{1b}[?1000l"));
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_mock_one_command_workflow() {
    let directory = tempfile::tempdir().expect("managed test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("managed-backend.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, false);
    tui.wait_for("DDB Debugger");
    tui.wait_for("managed DDB");
    tui.wait_for("managed_mock_session");
    tui.wait_for_source_marker('▶', 9);

    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    tui.wait_for_source_marker('▶', 10);

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(backend_log.is_file());
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires sibling DDB binary and a PTY"]
fn managed_backend_arguments_reach_ddb_without_changing_transport() {
    for dispatcher in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let config = managed_mock_config(directory.path());
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let ddb_binary = ddb_e2e_binary();
        let prefix = if dispatcher {
            format!(
                "env DDB_TUI_PATH={} {} tui",
                shell_quote(&tui_binary),
                shell_quote(&ddb_binary)
            )
        } else {
            format!(
                "{} --ddb-path {}",
                shell_quote(&tui_binary),
                shell_quote(&ddb_binary)
            )
        };
        let mut tui = Tui::spawn_shell(&format!("stty rows 40 cols 140; exec {prefix} --trace-api --ddb-arg=--command-workers=4 --ddb-arg=--file-level=warn '--ddb-arg=--session-id=argument with spaces' {}", shell_quote(&config)));
        tui.wait_for("managed_mock_session");
        tui.wait_for_source_marker('▶', 9);
        let pid = wait_for_managed_ddb_descendant(tui.child.id());
        let arguments = process_cmdline(pid);
        for expected in [
            "--command-workers=4",
            "--file-level=warn",
            "--session-id=argument with spaces",
            "--managed",
            "--api-auth-token-file",
            "--startup-report",
        ] {
            assert!(
                arguments.iter().any(|arg| arg == expected.as_bytes()),
                "missing {expected}"
            );
        }
        assert!(!arguments
            .iter()
            .any(|arg| arg.starts_with(b"--api-port") || arg.starts_with(b"--api-bind")));
        tui.send(b"Oz\x1b[H");
        tui.wait_for_current_frame(|screen| {
            screen.contains("[Restore]")
                && screen.contains("[DDB API]")
                && screen.contains("/GetServerInfo -> 200")
        });
        assert!(tui.quit().contains("\u{1b}[?1049l"));
        wait_for_process_exit(pid);
    }
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn ddb_tui_dispatcher_runs_the_managed_frontend() {
    let directory = tempfile::tempdir().expect("dispatcher test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("dispatcher-backend.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, true);
    tui.wait_for("DDB Debugger");
    tui.wait_for("managed DDB");
    tui.wait_for("managed_mock_session");

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(backend_log.is_file());
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_distributed_sessions_surface_partial_readiness() {
    let directory = tempfile::tempdir().expect("distributed test directory should exist");
    let config = managed_distributed_mock_config(directory.path());
    let backend_log = directory.path().join("distributed-backend.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, false);
    tui.wait_for("DDB Debugger");
    tui.wait_for("first-ready (");
    tui.wait_for("1 sessions · 1 groups");
    tui.wait_for("child_leaf");
    tui.wait_for("second-delayed (");
    tui.wait_for("2 sessions · 2 groups");
    tui.wait_for("distributed call boundary");
    tui.wait_for("parent_handler");

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires Linux and the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_terminal_loss_does_not_orphan_ddb() {
    let directory = tempfile::tempdir().expect("terminal-loss test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("terminal-loss-backend.log");

    let mut tui = Tui::spawn_managed(&config, &backend_log, false);
    // The startup status can be replaced before the first rendered frame.
    // Wait for an inspected session before destroying its terminal instead.
    tui.wait_for_current_frame(|screen| {
        screen.contains("● connected") && screen.contains("managed_mock_session")
    });
    let ddb_pid = wait_for_managed_ddb_descendant(tui.child.id());
    let token_path = managed_argument_path(ddb_pid, "--api-auth-token-file");
    let runtime_directory = token_path.parent().unwrap().to_path_buf();
    tui.crash_terminal();
    wait_for_process_exit(ddb_pid);
    wait_for_path_absent(&runtime_directory);
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires Linux and the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn managed_credentials_never_cross_process_or_diagnostic_boundaries() {
    let directory = tempfile::tempdir().expect("credential test directory should exist");
    let config = managed_mock_config(directory.path());
    let backend_log = directory.path().join("credential-backend.log");
    let (ddb_shim, token_capture) = credential_capture_shim(directory.path());

    let mut tui = Tui::spawn_managed_with_ddb(&config, &backend_log, false, &ddb_shim);
    tui.wait_for("managed DDB");
    let ddb_pid = wait_for_managed_ddb_descendant(tui.child.id());
    let cmdline = fs::read(format!("/proc/{ddb_pid}/cmdline")).unwrap();
    let environment = fs::read(format!("/proc/{ddb_pid}/environ")).unwrap();
    let token_path = managed_argument_path(ddb_pid, "--api-auth-token-file");
    let report_path = managed_argument_path(ddb_pid, "--startup-report");
    let runtime_directory = token_path.parent().unwrap().to_path_buf();
    assert!(
        !token_path.exists(),
        "managed DDB retained its credential document after loading it"
    );
    let token_document: Value = serde_json::from_slice(&fs::read(&token_capture).unwrap()).unwrap();
    let tokens = token_document["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["token"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let backend_bytes = fs::read(&backend_log).unwrap();
    let report_bytes = fs::read(&report_path).unwrap();
    let terminal_output = tui.output_text();

    for token in &tokens {
        let secret = token.as_bytes();
        assert!(
            !contains_bytes(&cmdline, secret),
            "token leaked through argv"
        );
        assert!(
            !contains_bytes(&environment, secret),
            "token leaked through env"
        );
        assert!(
            !contains_bytes(&backend_bytes, secret),
            "token leaked through backend log"
        );
        assert!(
            !contains_bytes(&report_bytes, secret),
            "token leaked through startup report"
        );
        assert!(
            !terminal_output.contains(token),
            "token leaked through TUI output"
        );
    }

    tui.quit();
    assert!(
        !runtime_directory.exists(),
        "managed runtime credentials were not removed on clean exit"
    );
}

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn reconnects_and_bootstraps_when_ddb_starts_late() {
    let start_guard = ddb_start_guard();
    let port = reserve_port();
    let api = format!("http://127.0.0.1:{port}");
    let mut tui = Tui::spawn_with_refresh(&api, 250);
    tui.wait_for("DDB Debugger");
    tui.wait_for("reconnecting");

    let mut ddb = Ddb::spawn_mock_on_reserved(port);
    drop(start_guard);
    tui.wait_for("mock_session");
    tui.wait_for("counter");
    tui.wait_for("connected");

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    ddb.shutdown();
}

#[test]
#[ignore = "requires the sibling DDB/fixture binaries and GDB; see ddb-tui/README.md#end-to-end-tests"]
fn real_gdb_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    real_debugger_workflow("gdb");
}

#[test]
#[ignore = "requires the sibling DDB/fixture binaries and LLDB; see ddb-tui/README.md#end-to-end-tests"]
fn real_lldb_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    real_debugger_workflow("lldb");
}

#[test]
#[ignore = "requires sibling DDB/fixture binaries, GDB and LLDB"]
fn real_debugger_settings_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for backend in ["gdb", "lldb"] {
        let (mut ddb, fixture) = Ddb::spawn_real(backend);
        ddb.wait_for_snapshot(|snapshot| {
            snapshot
                .sessions
                .iter()
                .any(|session| session.status == v2::SessionStatus::Stopped as i32)
        });
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.yaml");
        fs::write(&settings, format!(
            "entry_function: ddb_real_loop::function_breakpoint_target\nsource_mappings:\n  - from: /ddb-setup-build\n    to: {}\ngdb:\n  commands: ['echo DDB_SETUP_APPLIED']\nlldb:\n  commands: [\"script print('DDB_SETUP_APPLIED')\"]\n",
            serde_json::to_string(fixture.source.parent().unwrap().to_str().unwrap()).unwrap()
        )).unwrap();
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let mut tui = Tui::spawn_shell(&format!(
            "stty rows 40 cols 140; exec {} --debugger-settings {} connect --api {} --refresh-ms 10000",
            shell_quote(&tui_binary), shell_quote(&settings), shell_quote(Path::new(&ddb.base_url()))
        ));
        let snapshot = ddb.wait_for_snapshot(|snapshot| {
            snapshot.operations.iter().any(|op| {
                op.state == v2::OperationState::Failed as i32
                    || (op.kind == v2::OperationKind::RawCommand as i32
                        && op.state == v2::OperationState::Completed as i32)
            })
        });
        for operation in &snapshot.operations {
            assert_ne!(
                operation.state,
                v2::OperationState::Failed as i32,
                "{backend} setup failed: {:?}",
                operation.error
            );
        }
        tui.send(b"O");
        tui.wait_for("DDB_SETUP_APPLIED");
        let entry = snapshot.breakpoints.iter().find(|bp| {
            matches!(bp.spec.as_ref().and_then(|spec| spec.location.as_ref()),
                Some(v2::breakpoint_spec::Location::Function(function)) if function.function_name == "ddb_real_loop::function_breakpoint_target")
        }).expect("typed entry breakpoint must be installed before startup commands");
        assert!(entry.spec.as_ref().unwrap().temporary);
        assert!(entry.verified);
        assert_eq!(entry.hit_count, 0, "setup must not resume execution");
        assert!(snapshot
            .threads
            .iter()
            .all(|thread| thread.state == v2::ThreadState::Stopped as i32));
        assert_eq!(
            snapshot
                .operations
                .iter()
                .filter(|op| op.kind == v2::OperationKind::ConfigureDebugger as i32
                    && op.state == v2::OperationState::Completed as i32)
                .count(),
            if backend == "gdb" { 2 } else { 1 }
        );
        tui.wait_for("◎ thread");
        tui.send(if backend == "gdb" {
            b":show substitute-path\r"
        } else {
            b":settings show target.source-map\r"
        });
        tui.wait_for("/ddb-setup-build");
        tui.clear_capture();
        tui.send(b"\x1b[15~");
        ddb.wait_for_snapshot(|snapshot| {
            snapshot.threads.iter().any(|thread| {
                thread.state == v2::ThreadState::Stopped as i32
                    && thread
                        .location
                        .as_ref()
                        .and_then(|location| location.function_name.as_deref())
                        .is_some_and(|function| function.contains("function_breakpoint_target"))
            })
        });
        let stopped_line = ddb.top_frame_line(&ddb.global_thread_id());
        tui.wait_for_source_marker('▶', stopped_line);
        tui.send(b"wcounter\r");
        tui.wait_for_variable_value("Watch: counter", "0");
        tui.quit();
        ddb.shutdown();
    }
}

#[test]
#[ignore = "requires sibling DDB/fixture binaries, GDB and LLDB"]
fn real_logpoint_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for backend in ["gdb", "lldb"] {
        let (mut ddb, fixture) = Ddb::spawn_real(backend);
        ddb.wait_for_snapshot(|snapshot| {
            snapshot
                .threads
                .iter()
                .any(|thread| thread.state == v2::ThreadState::Stopped as i32)
        });
        let mut tui = Tui::spawn(&ddb.base_url());
        tui.wait_for("◎ thread");
        tui.send(b"Fddb_real_loop::function_breakpoint_target\r");
        tui.wait_for("DDB breakpoint targets");
        tui.send(b" \r");
        ddb.wait_for_snapshot(|snapshot| {
            snapshot.breakpoints.len() == 1 && snapshot.breakpoints[0].verified
        });
        tui.wait_for("1 breakpoints");
        let header = tui
            .frames
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .lines()
            .position(|line| {
                line.chars()
                    .take(33)
                    .collect::<String>()
                    .contains("DDB Breakpoints")
            })
            .unwrap()
            + 1;
        tui.send(format!("\x1b[<0;5;{header}M\x1b[<0;5;{header}m").as_bytes());
        tui.send(b"Llog counter={counter}\r");
        tui.wait_for("Logpoint configuration applied");
        tui.send(
            format!(
                "g{}:{}\r",
                fixture.source.display(),
                fixture.breakpoint_line
            )
            .as_bytes(),
        );
        tui.wait_for_source_marker('▸', fixture.breakpoint_line);
        tui.send(b"b");
        tui.wait_for("DDB breakpoint targets");
        tui.send(b"\r");
        ddb.wait_for_snapshot(|snapshot| {
            snapshot.breakpoints.len() == 2 && snapshot.breakpoints.iter().all(|bp| bp.verified)
        });
        tui.send(b"\x1b[15~");
        tui.wait_for_current_frame(|screen| screen.contains("log counter=0"));
        ddb.wait_for_snapshot(|snapshot| {
            snapshot.breakpoints.iter().all(|bp| bp.hit_count == 1)
                && snapshot.threads.iter().any(|thread| {
                    thread.state == v2::ThreadState::Stopped as i32
                        && thread
                            .location
                            .as_ref()
                            .is_some_and(|location| location.line == fixture.breakpoint_line as u32)
                })
        });
        tui.wait_for_source_marker('▶', fixture.breakpoint_line);
        tui.quit();
        ddb.shutdown();
    }
}

#[test]
#[ignore = "requires sibling DDB/fixture binaries, GDB and LLDB"]
fn real_function_breakpoint_terminal_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for backend in ["gdb", "lldb"] {
        let (mut ddb, _fixture) = Ddb::spawn_real(backend);
        ddb.wait_for_snapshot(|snapshot| {
            snapshot
                .sessions
                .first()
                .is_some_and(|session| session.status == v2::SessionStatus::Stopped as i32)
        });
        let mut tui = Tui::spawn(&ddb.base_url());
        tui.wait_for("connected");
        tui.wait_for("◎ thread");
        tui.send(b"Fddb_real_loop::function_breakpoint_target -- --ignore-count 3\r");
        tui.wait_for("DDB breakpoint targets");
        // Function breakpoints have no source owner; choose broadcast explicitly.
        tui.send(b" \r");
        let snapshot = ddb.wait_for_snapshot(|snapshot| {
            snapshot.operations.iter().any(|operation| {
                operation.kind == v2::OperationKind::CreateBreakpoint as i32
                    && operation.state == v2::OperationState::Failed as i32
            }) || snapshot.breakpoints.first().is_some_and(|bp| {
                matches!(bp.spec.as_ref().and_then(|spec| spec.location.as_ref()),
                Some(v2::breakpoint_spec::Location::Function(function))
                    if function.function_name == "ddb_real_loop::function_breakpoint_target")
            })
        });
        assert!(
            !snapshot.breakpoints.is_empty(),
            "{backend} function breakpoint failed: {:?}\n{}",
            snapshot.operations,
            ddb.output_text()
        );
        assert_eq!(
            snapshot.breakpoints[0].spec.as_ref().unwrap().ignore_count,
            Some(3)
        );
        tui.wait_for("1 breakpoints");
        tui.clear_capture();
        tui.send(b"\x1b[15~");
        tui.wait_for("exec · stopped");
        tui.wait_for("counter");
        ddb.wait_for_snapshot(|snapshot| {
            snapshot
                .breakpoints
                .first()
                .is_some_and(|bp| bp.hit_count == 1)
        });
        let stopped_line = ddb.top_frame_line(&ddb.global_thread_id());
        assert!(stopped_line > 0);
        tui.wait_for_source_marker('▶', stopped_line);
        // The fixture starts at entry with counter=0. Skipping three function
        // hits must stop on counter=3, not merely store the option in the API.
        tui.send(b"wcounter\r");
        tui.wait_for_variable_value("Watch: counter", "3");
        tui.wait_for_current_frame(|screen| screen.contains("▶ 1 paused"));
        let hit_row = tui
            .frames
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .lines()
            .position(|line| {
                line.chars()
                    .take(33)
                    .collect::<String>()
                    .contains("▶ 1 paused")
            })
            .expect("current hit should be visible in the breakpoint pane")
            + 1;
        tui.send(format!("\x1b[<2;5;{hit_row}M\x1b[<2;5;{hit_row}m").as_bytes());
        tui.wait_for_current_frame(|screen| screen.contains("Paused breakpoint hits"));
        // Cancel, reopen with the keyboard, then navigate through the real
        // stopped-thread inspection path rather than just opening a source file.
        tui.send(b"\x1b");
        tui.wait_for_current_frame(|screen| !screen.contains("Paused breakpoint hits"));
        tui.send(b"P");
        tui.wait_for_current_frame(|screen| screen.contains("Paused breakpoint hits"));
        tui.send(b"\r");
        tui.wait_for_current_frame(|screen| !screen.contains("Paused breakpoint hits"));
        tui.wait_for_variable_value("Watch: counter", "3");
        tui.wait_for_source_marker('▶', stopped_line);
        tui.quit();
        ddb.shutdown();
    }
}

#[test]
#[ignore = "requires the sibling DDB/fixture binaries and GDB; see ddb-tui/README.md#end-to-end-tests"]
fn managed_real_gdb_launch_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    managed_real_launch_workflow("gdb");
}

#[test]
#[ignore = "requires the sibling DDB/fixture binaries and LLDB; see ddb-tui/README.md#end-to-end-tests"]
fn managed_real_lldb_launch_workflow() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    managed_real_launch_workflow("lldb");
}

#[test]
#[ignore = "requires the sibling DDB/fixture binaries and GDB; see ddb-tui/README.md#end-to-end-tests"]
fn managed_real_gdb_attach_detaches_on_quit() {
    let _guard = REAL_DEBUGGER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = prepared_real_fixture("gdb");
    let directory = tempfile::tempdir().expect("attach test directory should exist");
    let pid_file = directory.path().join("attached.pid");
    let backend_log = directory.path().join("attach-backend.log");
    let mut debuggee = Command::new(&fixture.binary)
        .args([
            "--pid-file",
            pid_file.to_str().unwrap(),
            "--sleep-ms",
            "10",
            "--max-iterations",
            "100000",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("attach fixture should start");
    let pid = wait_for_pid_file(&pid_file);
    assert_eq!(pid, debuggee.id());

    let mut tui = Tui::spawn_managed_attach(pid, &backend_log);
    tui.wait_for("DDB Debugger");
    tui.wait_for("managed DDB");
    tui.wait_for("stopped");
    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    assert!(
        debuggee.try_wait().unwrap().is_none(),
        "attach mode must detach and leave the original process alive"
    );

    debuggee.kill().expect("test fixture should be terminated");
    debuggee.wait().expect("test fixture should be reaped");
}

fn managed_real_launch_workflow(backend: &str) {
    let fixture = prepared_real_fixture(backend);
    let directory = tempfile::tempdir().expect("launch test directory should exist");
    let pid_file = directory.path().join("launched.pid");
    let backend_log = directory
        .path()
        .join(format!("{backend}-managed-backend.log"));
    let mut tui = Tui::spawn_managed_launch(backend, &fixture.binary, &pid_file, &backend_log);

    tui.wait_for("DDB Debugger");
    tui.wait_for("managed DDB");
    tui.wait_for("stopped");
    tui.wait_for("1 sessions · 1 groups");
    // Startup symbol names vary across debugger and linker versions; assert the
    // stable frame index and executable identity rendered by both backends.
    tui.wait_for("#0");
    tui.wait_for("ddb_real_loop");
    // Session stop output may precede the API thread projection on LLDB.
    // The breakpoint and execution checks below need a projected thread.
    tui.wait_for("◎ thread");

    let goto_breakpoint = format!(
        "g{}:{}\r",
        fixture.source.display(),
        fixture.breakpoint_line
    );
    tui.send(goto_breakpoint.as_bytes());
    tui.wait_for_source_marker('▸', fixture.breakpoint_line);
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("1 breakpoints");

    tui.clear_capture();
    tui.send(b"\x1b[15~");
    tui.wait_for("continue:");
    tui.wait_for("exec · stopped");
    tui.wait_for("breakpoint_target");
    tui.wait_for_source_marker('▶', fixture.breakpoint_line);
    let before_step_line = tui
        .latest_execution_line()
        .expect("stopped execution line should be rendered");

    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    let after_step_line = tui.wait_for_execution_line_change(before_step_line);
    assert_ne!(after_step_line, before_step_line);

    let pid = wait_for_pid_file(&pid_file);
    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    wait_for_process_exit(pid);
}

fn prepared_real_fixture(backend: &str) -> PreparedRealFixture {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = manifest_dir.join("../ddb/core/tests/fixtures/real_loop");
    let source = fs::canonicalize(fixture_root.join("src/main.rs"))
        .expect("real fixture source should canonicalize");
    let binary = fixture_root.join("target/debug/ddb_real_loop");
    assert!(
        binary.is_file(),
        "real debugger fixture not found at {}; build it with cargo build --manifest-path {}",
        binary.display(),
        fixture_root.join("Cargo.toml").display()
    );
    let status = Command::new(backend)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("{backend} should be installed: {error}"));
    assert!(status.success(), "{backend} --version should succeed");
    let breakpoint_line = fs::read_to_string(&source)
        .expect("real fixture source should be readable")
        .lines()
        .position(|line| line.contains("BREAKPOINT_MARKER"))
        .map(|index| index + 1)
        .expect("real fixture breakpoint marker should exist");
    PreparedRealFixture {
        binary,
        source,
        breakpoint_line,
    }
}

struct PreparedRealFixture {
    binary: PathBuf,
    source: PathBuf,
    breakpoint_line: usize,
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            return contents
                .trim()
                .parse()
                .expect("fixture PID file should contain a process ID");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture PID file {}",
            path.display()
        );
        thread::sleep(POLL);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_process_not_running(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, suffix)| suffix.as_bytes()[0])
            });
        if state.is_none() || state == Some(b'Z') {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "managed DDB PID {pid} kept executing after SIGKILL"
        );
        thread::sleep(POLL);
    }
}

fn wait_for_path_absent(path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for cleanup of {}",
            path.display()
        );
        thread::sleep(POLL);
    }
}

fn wait_for_process_exit(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "managed launch left debuggee PID {pid} alive"
        );
        thread::sleep(POLL);
    }
}

fn real_debugger_workflow(backend: &str) {
    let (mut ddb, fixture) = Ddb::spawn_real(backend);
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .sessions
            .first()
            .is_some_and(|session| session.status == v2::SessionStatus::Stopped as i32)
    });

    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("DDB Debugger");
    tui.wait_for("connected");
    tui.wait_for("stopped");
    tui.wait_for("1 sessions · 1 groups");
    // LLDB may stop in the C main wrapper, without a source path or a
    // Rust-qualified symbol. Verify the stopped frame, then navigate explicitly
    // to the fixture; the breakpoint below asserts the exact Rust function.
    tui.wait_for("#0");
    // Session stop output may precede the API thread projection on LLDB.
    // The breakpoint and execution checks below need a projected thread.
    tui.wait_for("◎ thread");

    if backend == "gdb" {
        let snapshot = ddb.wait_for_snapshot(|snapshot| {
            snapshot.operations.iter().any(|operation| {
                operation.kind == v2::OperationKind::ConfigureDebugger as i32
                    && operation.state == v2::OperationState::Completed as i32
            })
        });
        assert_eq!(
            snapshot
                .operations
                .iter()
                .filter(|operation| operation.kind == v2::OperationKind::ConfigureDebugger as i32)
                .count(),
            1
        );
    }

    // Edit a scratch register through the TUI, observe the real backend and
    // refreshed pane, then restore it before allowing the fixture to execute.
    let register_name = if cfg!(target_arch = "aarch64") {
        "x0"
    } else {
        "rax"
    };
    let register_frame = ddb
        .runtime
        .block_on(ddb.client.list_frames(v2::ListFramesRequest {
            thread_id: ddb.global_thread_id(),
            ..Default::default()
        }))
        .unwrap()
        .frames[0]
        .frame_id
        .clone();
    let original = ddb.register_value(
        &register_frame,
        register_name,
        v2::RegisterFormat::Hexadecimal,
    );
    tui.select_visible_register(register_name);
    tui.send(b"=31337\r");
    tui.wait_for("assign value: completed");
    let assigned = ddb.register_value(
        &register_frame,
        register_name,
        v2::RegisterFormat::Hexadecimal,
    );
    assert_eq!(
        u64::from_str_radix(assigned.trim_start_matches("0x"), 16),
        Ok(31337),
        "register read after completed assignment: {assigned:?}"
    );
    ddb.wait_until("register assignment to reach backend", |ddb| {
        let value = ddb.register_value(
            &register_frame,
            register_name,
            v2::RegisterFormat::Hexadecimal,
        );
        u64::from_str_radix(value.trim_start_matches("0x"), 16) == Ok(31337)
    });
    let rendered = ddb.register_value(&register_frame, register_name, v2::RegisterFormat::Natural);
    tui.wait_for_register_value(register_name, &rendered);
    tui.send(format!("={original}\r").as_bytes());
    ddb.wait_until("register restoration to reach backend", |ddb| {
        ddb.register_value(
            &register_frame,
            register_name,
            v2::RegisterFormat::Hexadecimal,
        ) == original
    });
    let restored = ddb.register_value(&register_frame, register_name, v2::RegisterFormat::Natural);
    tui.wait_for_register_value(register_name, &restored);

    // Read a real stack range through the typed API and expand Memory alone.
    // Both backends expose the stack-pointer register as $sp.
    tui.send(b"m$sp ; 256\r");
    tui.wait_for_current_frame(|screen| screen.contains("Raw Memory · 256 B"));
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| {
        screen.contains("Raw Memory · 256 B")
            && screen.contains("[Restore]")
            && !screen.contains("Breakpoints")
    });
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| {
        screen.contains("Breakpoints") && !screen.contains("[Restore]")
    });

    // Exercise the native CLI through the currently selected canonical frame.
    tui.send(b":print 314159 + 271828\r");
    tui.wait_for("585987");
    tui.wait_for("print 314159 + 271828: completed");
    // The shared diagnostic launcher opts into the timeline. Hide it explicitly
    // to prove console output is independent, then restore it for later checks.
    tui.send(b"t");
    tui.wait_for_current_frame(|screen| {
        screen.contains("Output ·") && screen.contains("585987") && !screen.contains("DDB Timeline")
    });
    tui.send(b"t");
    tui.wait_for_current_frame(|screen| screen.contains("DDB Timeline"));

    // Query the real debugger's catalog through the explorer before using a
    // breakpoint. No frame/source response is injected by this PTY workflow.
    tui.send(format!("g{}:2\r", fixture.source.display()).as_bytes());
    tui.wait_for_source_marker('▸', 2);
    tui.clear_capture();
    tui.send(b"\x10");
    tui.wait_for("Debugger source catalog");
    tui.send(b"fixtures/real_loop/src/main.rs");
    tui.wait_for_frame(|frame| {
        frame.contains("search: fixtures/real_loop/src/main.rs")
            && frame.contains("[debug]")
            && !frame.contains("Loading sources")
    });
    tui.clear_capture();
    tui.send(b"\r");
    tui.wait_for_frame(|frame| !frame.contains("Source explorer") && frame.contains("use std::{"));
    tui.wait_for_source_marker('▸', 2);

    let goto_breakpoint = format!(
        "g{}:{}\r",
        fixture.source.display(),
        fixture.breakpoint_line
    );
    tui.send(goto_breakpoint.as_bytes());
    tui.wait_for_source_marker('▸', fixture.breakpoint_line);
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("1 breakpoints");
    ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.len() == 1);

    tui.clear_capture();
    tui.send(b"\x1b[15~");
    tui.wait_for("continue:");
    tui.wait_for("exec · stopped");
    tui.wait_for("breakpoint_target");
    tui.wait_for("counter");
    tui.wait_for("std::hint::black_box");
    // The paused-hit prefix can push the cumulative count beyond the narrow
    // pane. Exercise expansion rather than relying on a transient short label.
    let breakpoint_header = tui
        .frames
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .lines()
        .position(|line| {
            line.chars()
                .take(33)
                .collect::<String>()
                .contains("DDB Breakpoints")
        })
        .unwrap()
        + 1;
    tui.send(format!("\x1b[<0;5;{breakpoint_header}M\x1b[<0;5;{breakpoint_header}m").as_bytes());
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| screen.contains("[Restore]") && screen.contains("hits:1"));
    tui.send(b"z");
    tui.wait_for_current_frame(|screen| {
        screen.contains("DDB Topology") && !screen.contains("[Restore]")
    });
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .breakpoints
            .first()
            .is_some_and(|breakpoint| breakpoint.hit_count == 1)
    });
    let thread_id = ddb.global_thread_id();
    let before_step_line = ddb.top_frame_line(&thread_id);
    assert_eq!(before_step_line, fixture.breakpoint_line);
    tui.wait_for_source_marker('▶', before_step_line);

    // Explicit source inspection uses HOVER, remains expandable, and is not a retained watch.
    tui.send(b"Erequest\r");
    tui.wait_for_current_frame(|screen| {
        screen.contains("Inspect: request") && !screen.contains("Evaluating")
    });
    tui.expand_visible_variable("Inspect: request");
    tui.wait_for_current_frame(|screen| {
        screen.lines().any(|line| {
            line.chars()
                .skip(105)
                .collect::<String>()
                .contains("headers :")
        })
    });
    tui.select_visible_variable("Inspect: request");
    tui.send(b"\x1b[3~");
    tui.wait_for_current_frame(|screen| !screen.contains("Inspect: request"));

    // Drive lazy expansion through the real SDK/backend, not only the model.
    tui.send(b"wcounter\r");
    tui.wait_for("Watch: counter");
    tui.select_visible_variable("counter");
    tui.send(b"=31337\r");
    tui.wait_for("assign value: completed");
    tui.wait_for_frame(|frame| {
        let lines = frame.lines().collect::<Vec<_>>();
        lines.windows(2).any(|rows| {
            rows[0]
                .chars()
                .skip(105)
                .collect::<String>()
                .contains("counter :")
                && rows[1]
                    .chars()
                    .skip(105)
                    .collect::<String>()
                    .contains("31337")
        })
    });
    tui.wait_for_variable_value("Watch: counter", "31337");
    tui.select_visible_variable("Watch: counter");
    {
        // Native storage metadata must drive M on both backends, without a manually entered expression.
        tui.send(b"M");
        tui.wait_for_current_frame(|screen| screen.contains("Raw Memory · 256 B"));
        tui.send(b"z");
        let bytes = 31337_u64
            .to_ne_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        tui.wait_for_current_frame(|screen| {
            screen.contains("[Restore]") && screen.contains(&bytes)
        });
        tui.send(b"z");
        tui.wait_for_current_frame(|screen| !screen.contains("[Restore]"));
        tui.select_visible_variable("Watch: counter");
    }
    tui.send(b"W + 1\r");
    tui.wait_for_variable_value("Watch: counter + 1", "31338");
    tui.send(b"W");
    tui.wait_for_current_frame(|screen| screen.contains("Edit watch expression"));
    tui.send(b"\x1b");
    tui.wait_for_current_frame(|screen| !screen.contains("Edit watch expression"));
    tui.wait_for_variable_value("Watch: counter + 1", "31338");
    tui.send(b"\x1b[3~");
    tui.wait_for_current_frame(|frame| !frame.contains("Watch: counter"));
    tui.send(b"wrequest\r");
    tui.wait_for_current_frame(|frame| {
        frame.contains("Watch: request") && !frame.contains("Evaluating")
    });
    tui.expand_visible_variable("Watch: request");
    tui.wait_for("headers");
    tui.select_visible_variable("Watch: request");
    tui.send(b"\x1b[3~");
    tui.wait_for_current_frame(|frame| !frame.contains("Watch: request"));
    tui.expand_visible_variable("request");
    tui.wait_for("▾ request");
    // M leaves a Memory pane visible. Scroll to the first child in the
    // smaller Variables viewport before exercising its mouse expansion.
    tui.send(b"\x1b[B");
    tui.expand_visible_variable("headers");
    tui.wait_for("▾ headers");
    // Variables -> Stack -> Source; subsequent breakpoint commands use the source cursor.
    tui.send(b"\x1b[Z\x1b[Z");

    tui.clear_capture();
    tui.send(b"b");
    tui.wait_for("delete breakpoint");
    ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.is_empty());
    tui.wait_for("0 breakpoints");

    tui.clear_capture();
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("1 breakpoints");
    ddb.wait_for_snapshot(|snapshot| snapshot.breakpoints.len() == 1);
    tui.clear_capture();
    tui.send(b"w31337 + 1\r");
    tui.wait_for_variable_value("Watch: 31337 + 1", "31338");
    let evaluation_count = ddb
        .wait_for_snapshot(|_| true)
        .operations
        .iter()
        .filter(|operation| operation.kind == v2::OperationKind::Evaluate as i32)
        .count();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    let after_step_line = ddb.wait_for_top_frame_line_change(&thread_id, before_step_line);
    tui.wait_for_source_marker('▶', after_step_line);
    assert_ne!(after_step_line, before_step_line);
    ddb.wait_for_snapshot(|snapshot| {
        snapshot
            .operations
            .iter()
            .filter(|operation| operation.kind == v2::OperationKind::Evaluate as i32)
            .count()
            > evaluation_count
    });
    tui.wait_for_variable_value("Watch: 31337 + 1", "31338");

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    ddb.shutdown();
}

struct RealFixture {
    source: PathBuf,
    breakpoint_line: usize,
}

struct Ddb {
    _tempdir: TempDir,
    child: Child,
    stdin: ChildStdin,
    client: DdbClient,
    runtime: tokio::runtime::Runtime,
    base_url: String,
    output: Arc<Mutex<Vec<u8>>>,
    stopped: bool,
}

impl Ddb {
    fn spawn_mock() -> Self {
        let _start_guard = ddb_start_guard();
        Self::spawn_mock_on_reserved(reserve_port())
    }

    fn spawn_mock_on_reserved(port: u16) -> Self {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tempdir = tempfile::tempdir().expect("temporary DDB directory should be created");
        let config = format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  api_server_port: {port}
  api_insecure_allow_unauthenticated_v2: true
  base_dir: "{base_dir}"
  log_dir: "{log_dir}"
  Debugger:
    backend: mock
StaticSessions:
  - tag: "tui-e2e"
    alias: "frontend"
    hash: "tui-e2e-group"
    pid: 1701
    start_delay_ms: 0
    mock:
      source_file: "tests/api_v1.rs"
      source_line: 9
      function: "mock_session"
      exit_on_continue: false
      exit_on_bootstrap: false
"#,
            base_dir = tempdir.path().join("state").display(),
            log_dir = tempdir.path().join("logs").display(),
        );
        Self::spawn_config(manifest_dir, tempdir, port, &config, &[])
    }

    fn spawn_real(backend: &str) -> (Self, RealFixture) {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture_root = manifest_dir.join("../ddb/core/tests/fixtures/real_loop");
        let source = fs::canonicalize(fixture_root.join("src/main.rs"))
            .expect("real fixture source should canonicalize");
        let binary = fixture_root.join("target/debug/ddb_real_loop");
        assert!(
            binary.is_file(),
            "real debugger fixture not found at {}; build it with `cargo build --manifest-path {}`",
            binary.display(),
            fixture_root.join("Cargo.toml").display()
        );
        let status = Command::new(backend)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|error| panic!("{backend} should be installed: {error}"));
        assert!(status.success(), "{backend} --version should succeed");
        let breakpoint_line = fs::read_to_string(&source)
            .expect("real fixture source should be readable")
            .lines()
            .position(|line| line.contains("BREAKPOINT_MARKER"))
            .map(|index| index + 1)
            .expect("real fixture breakpoint marker should exist");

        let _start_guard = ddb_start_guard();
        let port = reserve_port();
        let tempdir = tempfile::tempdir().expect("temporary DDB directory should be created");
        let config = format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: {port}
  api_insecure_allow_unauthenticated_v2: true
  base_dir: "{base_dir}"
  log_dir: "{log_dir}"
  Debugger:
    backend: {backend}
StaticSessions:
  - tag: "tui-real-{backend}"
    alias: "tui-real-{backend}"
    hash: "tui-real-{backend}-group"
    pid: 2701
    ip: "127.0.0.1"
    start_delay_ms: 0
    start_mode: binary
    binary_path: "{binary}"
    stop_at_entry: true
    binary_args:
      - "--sleep-ms"
      - "10"
      - "--max-iterations"
      - "100000"
"#,
            base_dir = tempdir.path().join("state").display(),
            log_dir = tempdir.path().join("logs").display(),
            binary = binary.display(),
        );
        let environment =
            (backend == "lldb").then_some(("FAKETIME", "-00000000000000000000.000000000"));
        let environment = environment.as_slice();
        let ddb = Self::spawn_config(manifest_dir, tempdir, port, &config, environment);
        (
            ddb,
            RealFixture {
                source,
                breakpoint_line,
            },
        )
    }

    fn spawn_config(
        manifest_dir: &Path,
        tempdir: TempDir,
        port: u16,
        config: &str,
        environment: &[(&str, &str)],
    ) -> Self {
        let ddb_binary = std::env::var_os("DDB_E2E_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("../ddb/target/debug/ddb"));
        assert!(
            ddb_binary.is_file(),
            "DDB binary not found at {}; build it with `cargo build -p ddb --manifest-path ../ddb/Cargo.toml` or set DDB_E2E_BIN",
            ddb_binary.display()
        );
        let config_path = tempdir.path().join("ddb-tui-e2e.yaml");
        fs::write(&config_path, config).expect("DDB config should be written");

        let mut command = Command::new(ddb_binary);
        command
            .arg(&config_path)
            .current_dir(manifest_dir.join("../ddb/core"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("DDB should start");
        let stdin = child.stdin.take().expect("DDB stdin should be piped");
        let output = Arc::new(Mutex::new(Vec::new()));
        drain(
            child.stdout.take().expect("DDB stdout should be piped"),
            output.clone(),
        );
        drain(
            child.stderr.take().expect("DDB stderr should be piped"),
            output.clone(),
        );
        let mut config = ClientConfig::new(format!("http://127.0.0.1:{port}"));
        config.request_timeout = Duration::from_secs(5);
        let client = DdbClient::new(config).expect("SDK client should build");
        let runtime = tokio::runtime::Runtime::new().expect("SDK runtime should build");
        let mut ddb = Self {
            _tempdir: tempdir,
            child,
            stdin,
            client,
            runtime,
            base_url: format!("http://127.0.0.1:{port}"),
            output,
            stopped: false,
        };
        ddb.wait_until("DDB readiness", |ddb| {
            ddb.runtime
                .block_on(ddb.client.get_readiness(v2::GetReadinessRequest::default()))
                .is_ok()
        });
        ddb
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn wait_for_snapshot(&mut self, predicate: impl Fn(&v2::Snapshot) -> bool) -> v2::Snapshot {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            let response = self
                .runtime
                .block_on(self.client.get_snapshot(v2::GetSnapshotRequest::default()));
            if let Ok(response) = &response {
                if let Some(snapshot) = &response.snapshot {
                    if predicate(snapshot) {
                        return snapshot.clone();
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for v2 snapshot: {response:?}\nDDB output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn global_thread_id(&self) -> String {
        self.runtime
            .block_on(self.client.get_snapshot(v2::GetSnapshotRequest::default()))
            .expect("GetSnapshot should succeed")
            .snapshot
            .expect("GetSnapshot should return a snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.state == v2::ThreadState::Stopped as i32)
            .expect("a stopped thread should exist")
            .thread_id
    }

    fn top_frame_line(&self, thread_id: &str) -> usize {
        self.runtime
            .block_on(self.client.list_frames(v2::ListFramesRequest {
                thread_id: thread_id.to_string(),
                ..Default::default()
            }))
            .expect("ListFrames should succeed")
            .frames
            .first()
            .and_then(|frame| frame.location.as_ref())
            .expect("top frame should have a location")
            .line as usize
    }

    fn register_value(&self, frame_id: &str, name: &str, format: v2::RegisterFormat) -> String {
        let register = self
            .runtime
            .block_on(self.client.list_registers(v2::ListRegistersRequest {
                frame_id: frame_id.into(),
                format: format as i32,
                ..Default::default()
            }))
            .expect("ListRegisters should succeed")
            .registers
            .into_iter()
            .find(|register| register.name == name && !register.unavailable)
            .expect("scratch register should be available");
        if format == v2::RegisterFormat::Natural {
            register.value
        } else {
            register
                .formatted_value
                .expect("requested register formatting should be returned")
        }
    }

    fn wait_for_top_frame_line_change(&mut self, thread_id: &str, previous: usize) -> usize {
        let mut changed = None;
        self.wait_until("top frame line to change", |ddb| {
            let line = ddb.top_frame_line(thread_id);
            if line != previous {
                changed = Some(line);
                true
            } else {
                false
            }
        });
        changed.expect("changed top-frame line should be captured")
    }

    fn wait_until(&mut self, label: &str, mut predicate: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {label}\nDDB output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn assert_running(&mut self) {
        if let Some(status) = self
            .child
            .try_wait()
            .expect("DDB status should be readable")
        {
            panic!(
                "DDB exited unexpectedly with {status}\n{}",
                self.output_text()
            );
        }
    }

    fn output_text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.stdin.write_all(b"exit\n");
        let _ = self.stdin.flush();
        wait_or_kill(&mut self.child);
        self.stopped = true;
    }
}

impl Drop for Ddb {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Tui {
    child: Child,
    stdin: ChildStdin,
    output: Arc<Mutex<Vec<u8>>>,
    frames: Arc<Mutex<VecDeque<String>>>,
    drain_threads: Vec<thread::JoinHandle<()>>,
    stopped: bool,
}

impl Tui {
    fn spawn(api: &str) -> Self {
        Self::spawn_with_refresh(api, 10_000)
    }

    fn spawn_with_refresh(api: &str, refresh_ms: u64) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let command = format!(
            "stty rows 40 cols 140; exec {} --api {} --refresh-ms {refresh_ms}",
            shell_quote(&tui_binary),
            shell_quote(Path::new(api)),
        );
        Self::spawn_shell(&command).with_timeline()
    }

    fn spawn_with_workspace(api: &str, workspace: &Path) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        Self::spawn_shell(&format!(
            "stty rows 40 cols 140; exec {} --source-workspace {} connect --api {} --refresh-ms 10000",
            shell_quote(&tui_binary), shell_quote(workspace), shell_quote(Path::new(api))
        ))
    }

    fn spawn_managed(config: &Path, backend_log: &Path, via_dispatcher: bool) -> Self {
        let ddb_binary = ddb_e2e_binary();
        Self::spawn_managed_with_ddb(config, backend_log, via_dispatcher, &ddb_binary)
    }

    #[cfg(target_os = "linux")]
    fn spawn_managed_with_panic_hook(config: &Path, backend_log: &Path) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let ddb_binary = ddb_e2e_binary();
        let command = format!(
            "stty rows 40 cols 140; exec env DDB_TUI_TEST_PANIC_ON_KEY=1 {} --ddb-path {} --backend-log {} --refresh-ms 10000 {}",
            shell_quote(&tui_binary),
            shell_quote(&ddb_binary),
            shell_quote(backend_log),
            shell_quote(config),
        );
        Self::spawn_shell(&command)
    }

    fn spawn_managed_with_ddb(
        config: &Path,
        backend_log: &Path,
        via_dispatcher: bool,
        ddb_binary: &Path,
    ) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let command = if via_dispatcher {
            format!(
                "stty rows 40 cols 140; exec env DDB_TUI_PATH={} {} tui --backend-log {} --refresh-ms 10000 {}",
                shell_quote(&tui_binary),
                shell_quote(ddb_binary),
                shell_quote(backend_log),
                shell_quote(config),
            )
        } else {
            format!(
                "stty rows 40 cols 140; exec {} --ddb-path {} --backend-log {} --refresh-ms 10000 {}",
                shell_quote(&tui_binary),
                shell_quote(ddb_binary),
                shell_quote(backend_log),
                shell_quote(config),
            )
        };
        Self::spawn_shell(&command).with_timeline()
    }

    fn spawn_managed_launch(
        backend: &str,
        binary: &Path,
        pid_file: &Path,
        backend_log: &Path,
    ) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let ddb_binary = ddb_e2e_binary();
        let environment = if backend == "lldb" {
            "env FAKETIME=-00000000000000000000.000000000 "
        } else {
            ""
        };
        let command = format!(
            "stty rows 40 cols 140; exec {environment}{} --ddb-path {} --backend-log {} --refresh-ms 10000 launch --backend {backend} -- {} --pid-file {} --sleep-ms 10 --max-iterations 100000",
            shell_quote(&tui_binary),
            shell_quote(&ddb_binary),
            shell_quote(backend_log),
            shell_quote(binary),
            shell_quote(pid_file),
        );
        Self::spawn_shell(&command).with_timeline()
    }

    fn spawn_managed_attach(pid: u32, backend_log: &Path) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let ddb_binary = ddb_e2e_binary();
        let command = format!(
            "stty rows 40 cols 140; exec {} --ddb-path {} --backend-log {} --refresh-ms 10000 attach --backend gdb --pid {pid}",
            shell_quote(&tui_binary),
            shell_quote(&ddb_binary),
            shell_quote(backend_log),
        );
        Self::spawn_shell(&command).with_timeline()
    }

    fn spawn_shell(command: &str) -> Self {
        let mut child = Command::new("script")
            .args(["-qefc", command, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("PTY wrapper should start");
        let stdin = child.stdin.take().expect("TUI stdin should be piped");
        let output = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(Mutex::new(VecDeque::new()));
        let drain_threads = vec![
            drain_terminal(
                child.stdout.take().expect("TUI stdout should be piped"),
                output.clone(),
                frames.clone(),
            ),
            drain(
                child.stderr.take().expect("TUI stderr should be piped"),
                output.clone(),
            ),
        ];
        Self {
            child,
            stdin,
            output,
            frames,
            drain_threads,
            stopped: false,
        }
    }

    fn send(&mut self, input: &[u8]) {
        self.stdin
            .write_all(input)
            .expect("TUI input should be written");
        self.stdin.flush().expect("TUI input should flush");
    }

    // Diagnostic workflows explicitly opt in; workspace-only workflows keep
    // the default hidden timeline. Click the Source border, not a source line,
    // to restore focus without disabling execution following.
    fn with_timeline(mut self) -> Self {
        self.wait_for("DDB Debugger");
        self.send(b"t");
        self.wait_for("DDB Timeline");
        self.send(b"\x1b[<0;40;4M\x1b[<0;40;4m");
        self
    }

    fn click_topology_thread(&mut self, session_label: &str) {
        let deadline = Instant::now() + TIMEOUT;
        self.send(b"\x1b[<0;2;4M\x1b[<0;2;4m");
        // Topology sorts groups and sessions by name, not launch order. A new
        // session may sort above the current selection, so search from the top.
        self.clear_capture();
        loop {
            self.assert_running();
            self.send(b"\x1b[5~");
            thread::sleep(Duration::from_millis(100));
            let topology = self.frames.lock().unwrap().back().map(|frame| {
                frame
                    .lines()
                    .skip(4)
                    .take(4)
                    .map(|line| line.chars().take(33).collect::<String>())
                    .collect::<Vec<_>>()
            });
            if topology_reached_top(topology.as_ref()) {
                break;
            }
            assert!(Instant::now() < deadline, "topology did not reach its top");
        }
        loop {
            self.assert_running();
            let row = self
                .frames
                .lock()
                .unwrap()
                .back()
                .and_then(|frame| topology_thread_row(frame, session_label));
            if let Some(row) = row {
                self.clear_capture();
                self.send(format!("\x1b[<0;12;{row}M\x1b[<0;12;{row}m").as_bytes());
                return;
            }
            if Instant::now() >= deadline {
                let screen = self
                    .frames
                    .lock()
                    .unwrap()
                    .back()
                    .cloned()
                    .unwrap_or_default();
                panic!("thread below {session_label} not found:\n{screen}");
            }
            // Move one row so a session header at the bottom becomes visible
            // with its first thread. PageDown can skip that pairing entirely.
            self.send(b"\x1b[B");
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for(&mut self, needle: &str) {
        if needle == "DDB breakpoint targets" {
            self.wait_for_current_frame(|screen| {
                screen.contains(needle) && screen.contains("Session ·")
            });
            return;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            if self.contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}\nTUI output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn expand_visible_variable(&mut self, name: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            // The fixed 140-column test terminal places inspection in the
            // rightmost 25%; exclude source text with the same variable name.
            let row = self.frames.lock().unwrap().back().and_then(|frame| {
                frame.lines().enumerate().find_map(|(row, line)| {
                    line.chars()
                        .skip(105)
                        .collect::<String>()
                        .contains(&format!("▸ {name}"))
                        .then_some(row + 1)
                })
            });
            if let Some(row) = row {
                self.send(format!("\x1b[<2;112;{row}M\x1b[<2;112;{row}m").as_bytes());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expandable variable {name:?} not visible:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn select_visible_variable(&mut self, name: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let row = self.frames.lock().unwrap().back().and_then(|frame| {
                frame.lines().position(|line| {
                    let right = line.chars().skip(105).collect::<String>();
                    right.contains(&format!("{name} :"))
                        && (name.starts_with("Watch: ") || !right.contains("Watch: "))
                })
            });
            if let Some(row) = row {
                self.send(format!("\x1b[<0;112;{}M\x1b[<0;112;{}m", row + 1, row + 1).as_bytes());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "variable {name} not visible: {}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn select_visible_register(&mut self, name: &str) {
        self.wait_for_current_frame(|frame| {
            frame.lines().any(|line| {
                line.chars()
                    .skip(105)
                    .collect::<String>()
                    .contains(&format!(" {name} "))
            })
        });
        let row = self
            .frames
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .lines()
            .position(|line| {
                line.chars()
                    .skip(105)
                    .collect::<String>()
                    .contains(&format!(" {name} "))
            })
            .unwrap()
            + 1;
        self.send(format!("\x1b[<0;112;{row}M\x1b[<0;112;{row}m").as_bytes());
    }

    fn wait_for_register_value(&mut self, name: &str, value: &str) {
        self.wait_for_current_frame(|frame| {
            frame.lines().any(|line| {
                line.chars()
                    .skip(105)
                    .collect::<String>()
                    .contains(&format!(" {name} {value}"))
            })
        });
    }

    fn wait_for_variable_value(&mut self, name: &str, value: &str) {
        self.wait_for_current_frame(|frame| {
            frame.lines().collect::<Vec<_>>().windows(2).any(|rows| {
                rows[0]
                    .chars()
                    .skip(105)
                    .collect::<String>()
                    .contains(&format!("{name} :"))
                    && rows[1]
                        .chars()
                        .skip(105)
                        .collect::<String>()
                        .contains(value)
            })
        });
    }

    fn wait_for_current_frame(&mut self, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            if self
                .frames
                .lock()
                .unwrap()
                .back()
                .is_some_and(|frame| predicate(frame))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected current terminal frame not rendered:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn wait_for_frame(&mut self, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            if self
                .frames
                .lock()
                .unwrap()
                .iter()
                .any(|frame| predicate(frame))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected terminal frame not rendered:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn wait_for_source_marker(&mut self, marker: char, line_number: usize) {
        let deadline = Instant::now() + TIMEOUT;
        let patterns = [false, true]
            .into_iter()
            .flat_map(|executing| {
                [false, true].into_iter().flat_map(move |breakpoint| {
                    [false, true].into_iter().filter_map(move |cursor| {
                        let gutter = [
                            if executing { '▶' } else { ' ' },
                            if breakpoint { '●' } else { ' ' },
                            if cursor { '▸' } else { ' ' },
                        ];
                        gutter.contains(&marker).then(|| {
                            format!("{}{}{} {line_number:>5} │", gutter[0], gutter[1], gutter[2])
                        })
                    })
                })
            })
            .collect::<Vec<_>>();
        loop {
            self.assert_running();
            let found = self.frames.lock().unwrap().iter().any(|frame| {
                frame
                    .lines()
                    .any(|line| patterns.iter().any(|pattern| line.contains(pattern)))
            });
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for source marker {marker:?} at line {line_number}\nTUI output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn latest_execution_line(&self) -> Option<usize> {
        let gutter_prefixes = ["▶   ", "▶●  ", "▶ ▸ ", "▶●▸ "];
        self.frames.lock().unwrap().iter().rev().find_map(|frame| {
            frame.lines().find_map(|line| {
                gutter_prefixes.iter().find_map(|prefix| {
                    let start = line.find(prefix)? + prefix.len();
                    line[start..]
                        .split('│')
                        .next()?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
            })
        })
    }

    fn wait_for_execution_line_change(&mut self, previous: usize) -> usize {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.assert_running();
            if let Some(line) = self.latest_execution_line() {
                if line != previous {
                    return line;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for execution line to move from {previous}\nTUI output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn clear_capture(&mut self) {
        self.output.lock().unwrap().clear();
        self.frames.lock().unwrap().clear();
    }

    fn assert_running(&mut self) {
        if let Some(status) = self
            .child
            .try_wait()
            .expect("TUI status should be readable")
        {
            panic!(
                "TUI exited unexpectedly with {status}\n{}",
                self.output_text()
            );
        }
    }

    fn output_text(&self) -> String {
        let raw = String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned();
        let screen = self
            .frames
            .lock()
            .unwrap()
            .back()
            .cloned()
            .unwrap_or_default();
        format!("Current virtual screen:\n{screen}\nRaw PTY output:\n{raw}")
    }

    fn contains(&self, needle: &str) -> bool {
        if String::from_utf8_lossy(&self.output.lock().unwrap()).contains(needle) {
            return true;
        }
        self.frames
            .lock()
            .unwrap()
            .iter()
            .any(|frame| frame.contains(needle))
    }

    #[cfg(target_os = "linux")]
    fn crash_terminal(&mut self) {
        if self.stopped {
            return;
        }
        self.child
            .kill()
            .expect("PTY wrapper should accept SIGKILL");
        self.child.wait().expect("PTY wrapper should be reaped");
        self.finish_capture();
        self.stopped = true;
    }

    #[cfg(target_os = "linux")]
    fn finish_external_exit(&mut self) -> String {
        if !self.stopped {
            wait_or_kill(&mut self.child);
            self.finish_capture();
            self.stopped = true;
        }
        self.output_text()
    }

    fn quit(&mut self) -> String {
        if !self.stopped {
            self.send(b"q");
            wait_or_kill(&mut self.child);
            self.finish_capture();
            self.stopped = true;
        }
        self.output_text()
    }

    fn finish_capture(&mut self) {
        for drain_thread in self.drain_threads.drain(..) {
            drain_thread
                .join()
                .expect("TUI output drain thread should finish");
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stdin.write_all(b"q");
            let _ = self.stdin.flush();
            wait_or_kill(&mut self.child);
            self.finish_capture();
            self.stopped = true;
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_for_ddb_tui_descendant(root_pid: u32) -> u32 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        for pid in process_descendants(root_pid) {
            let executable = fs::read_link(format!("/proc/{pid}/exe")).ok();
            if executable
                .as_deref()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "ddb-tui")
            {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for ddb-tui below PID {root_pid}"
        );
        thread::sleep(POLL);
    }
}

fn wait_for_managed_ddb_descendant(root_pid: u32) -> u32 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        for pid in process_descendants(root_pid) {
            let arguments = process_cmdline(pid);
            if arguments.iter().any(|argument| argument == b"serve")
                && arguments.iter().any(|argument| argument == b"--managed")
            {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for managed DDB below PID {root_pid}"
        );
        thread::sleep(POLL);
    }
}

#[cfg(target_os = "linux")]
fn process_descendants(root_pid: u32) -> Vec<u32> {
    let mut descendants = Vec::new();
    let mut pending = VecDeque::from([root_pid]);
    while let Some(pid) = pending.pop_front() {
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        let Ok(children) = fs::read_to_string(children_path) else {
            continue;
        };
        for child in children
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
        {
            descendants.push(child);
            pending.push_back(child);
        }
    }
    descendants
}

#[cfg(target_os = "linux")]
fn process_cmdline(pid: u32) -> Vec<Vec<u8>> {
    fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(target_os = "linux")]
fn managed_argument_path(pid: u32, option: &str) -> PathBuf {
    let arguments = process_cmdline(pid);
    arguments
        .windows(2)
        .find(|window| window[0] == option.as_bytes())
        .map(|window| PathBuf::from(String::from_utf8_lossy(&window[1]).into_owned()))
        .unwrap_or_else(|| panic!("managed DDB PID {pid} omitted {option}"))
}

#[cfg(target_os = "linux")]
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(target_os = "linux")]
fn credential_capture_shim(directory: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let real_ddb = ddb_e2e_binary();
    let shim = directory.join("ddb-token-capture-shim");
    let capture = directory.join("captured-tokens.json");
    let script = format!(
        "#!/bin/sh\numask 077\nprevious=\nfor argument in \"$@\"; do\n  if [ \"$previous\" = '--api-auth-token-file' ]; then\n    cp -- \"$argument\" {}\n  fi\n  previous=$argument\ndone\nexec {} \"$@\"\n",
        shell_quote(&capture),
        shell_quote(&real_ddb),
    );
    fs::write(&shim, script).unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    (shim, capture)
}

fn ddb_e2e_binary() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = std::env::var_os("DDB_E2E_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../ddb/target/debug/ddb"));
    assert!(
        binary.is_file(),
        "DDB binary not found at {}; build it with cargo build -p ddb --manifest-path ../ddb/Cargo.toml or set DDB_E2E_BIN",
        binary.display()
    );
    binary
}

fn managed_distributed_mock_config(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ddb/core/tests/api_v1.rs")
        .canonicalize()
        .expect("mock source should canonicalize");
    let config = directory.join("managed-distributed-ddb.yaml");
    fs::write(
        &config,
        format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  base_dir: "{}"
  log_dir: "{}"
  Debugger:
    backend: mock
StaticSessions:
  - tag: "127.0.0.1:-1801"
    alias: first-ready
    hash: first-ready-group
    pid: 1801
    start_delay_ms: 0
    mock:
      source_file: "{}"
      source_line: 9
      function: first_ready_session
      exit_on_continue: false
      exit_on_bootstrap: false
      stack_frames:
        - function: child_leaf
          file: "{}"
          line: 9
        - function: child_dispatch
          file: "{}"
          line: 10
      dbt_parent:
        ip: 127.0.0.1
        pid: 1802
        tid: 1
        caller_ctx:
          pc: 5246976
          sp: 2147426304
          fp: 2147430400
  - tag: "127.0.0.1:-1802"
    alias: second-delayed
    hash: second-delayed-group
    pid: 1802
    start_delay_ms: 2500
    mock:
      source_file: "{}"
      source_line: 10
      function: second_delayed_session
      exit_on_continue: false
      exit_on_bootstrap: false
      stack_frames:
        - function: parent_handler
          file: "{}"
          line: 10
        - function: parent_root
          file: "{}"
          line: 11
"#,
            directory.join("state").display(),
            directory.join("logs").display(),
            source.display(),
            source.display(),
            source.display(),
            source.display(),
            source.display(),
            source.display(),
        ),
    )
    .expect("distributed managed configuration should be written");
    config
}

fn managed_mock_config(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ddb/core/tests/api_v1.rs")
        .canonicalize()
        .expect("mock source should canonicalize");
    let config = directory.join("managed-ddb.yaml");
    fs::write(
        &config,
        format!(
            r#"Framework: unspecified
Conf:
  auto_shutdown: false
  base_dir: "{}"
  log_dir: "{}"
  Debugger:
    backend: mock
StaticSessions:
  - tag: managed-tui-e2e
    alias: managed-frontend
    hash: managed-tui-group
    pid: 1702
    mock:
      source_file: "{}"
      source_line: 9
      function: managed_mock_session
      exit_on_continue: false
      exit_on_bootstrap: false
"#,
            directory.join("state").display(),
            directory.join("logs").display(),
            source.display(),
        ),
    )
    .expect("managed mock configuration should be written");
    config
}

fn reserve_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port should bind")
        .local_addr()
        .expect("ephemeral address should be available")
        .port()
}

fn ddb_start_guard() -> std::sync::MutexGuard<'static, ()> {
    DDB_START_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn drain(
    mut reader: impl Read + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => output.lock().unwrap().extend_from_slice(&buffer[..read]),
            }
        }
    })
}

fn drain_terminal(
    mut reader: impl Read + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
    frames: Arc<Mutex<VecDeque<String>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut parser = vt100::Parser::new(40, 140, 0);
        let mut tail = VecDeque::with_capacity(6);
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            output.lock().unwrap().extend_from_slice(&buffer[..read]);
            for byte in &buffer[..read] {
                parser.process(&[*byte]);
                if tail.len() == 6 {
                    tail.pop_front();
                }
                tail.push_back(*byte);
                if tail.iter().copied().eq(b"\x1b[?25l".iter().copied()) {
                    let mut frames = frames.lock().unwrap();
                    if frames.len() == 500 {
                        frames.pop_front();
                    }
                    frames.push_back(parser.screen().contents());
                }
            }
        }
    })
}

fn wait_or_kill(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("child status should be readable") {
            Some(_) => return,
            None if Instant::now() < deadline => thread::sleep(POLL),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#![cfg(unix)]

use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use serde_json::Value;
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

static REAL_DEBUGGER_LOCK: Mutex<()> = Mutex::new(());
static DDB_START_LOCK: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn mock_backend_full_terminal_workflow() {
    let mut ddb = Ddb::spawn_mock();
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/sessions")
            .and_then(Value::as_array)
            .is_some_and(|sessions| sessions.len() == 1)
    });

    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("DDB Debugger");
    tui.wait_for("mock_session");
    tui.wait_for("counter");
    tui.wait_for_source_marker('▶', 9);

    tui.send(b"Bif counter == 42\r");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("✓ breakpoint");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints")
            .and_then(Value::as_array)
            .is_some_and(|breakpoints| {
                breakpoints.len() == 1
                    && breakpoints[0]["condition"].as_str() == Some("counter == 42")
            })
    });

    // Source -> Stack -> Variables -> Timeline -> Threads -> Breakpoints.
    tui.clear_capture();
    tui.send(b"\t\t\t\t\t");
    tui.wait_for("\u{25c6} DDB Breakpoints");
    tui.send(b"x");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints/0/enabled")
            .and_then(Value::as_bool)
            == Some(false)
    });
    tui.wait_for("disable breakpoint");
    tui.send(b"x");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints/0/enabled")
            .and_then(Value::as_bool)
            == Some(true)
    });
    tui.send(b"\t");

    tui.send(b"m0x1000 ; 16\r");
    tui.wait_for("Raw Memory");
    tui.wait_for("Raw Memory · 16 B");
    tui.wait_for("|*...............|");
    tui.wait_for("16 bytes");

    tui.send(b"e counter\r");
    tui.wait_for("evaluate counter: completed");

    tui.send(b":-mock-stream-output\r");
    tui.wait_for("console · mock console output");

    tui.send(b"japi_v1.rs:10\r");
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
    tui.wait_for("Select a stopped thread to load source");
    tui.clear_capture();
    tui.wait_for("exec · stopped");
    // A stopped event precedes the asynchronous frame/source inspection.
    // Synchronize on the rendered execution location before acting on source.
    tui.wait_for_source_marker('▶', 9);

    // SGR mouse click on the toolbar's Refresh control (terminal coordinates are 1-based).
    tui.send(b"\x1b[<0;80;2M\x1b[<0;80;2m");
    tui.wait_for("1 sessions · 1 groups · 1 breakpoints");

    tui.send(b"b");
    tui.wait_for("delete breakpoint");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    });

    tui.send(b"g11\r");
    tui.wait_for_source_marker('▸', 11);

    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    tui.wait_for_source_marker('▶', 10);
    tui.wait_for_source_marker('▸', 11);

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
    tui.wait_for("managed DDB");
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
    tui.wait_for("managed DDB");
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
#[ignore = "requires the sibling DDB binary; see ddb-tui/README.md#end-to-end-tests"]
fn explicit_v1_fallback_uses_the_real_legacy_api() {
    let mut ddb = Ddb::spawn_mock();
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/sessions")
            .and_then(Value::as_array)
            .is_some_and(|sessions| sessions.len() == 1)
    });
    let proxy = V1OnlyProxy::spawn(&ddb.base_url());

    let mut tui = Tui::spawn_v1_fallback(&proxy.base_url());
    // The 140-column toolbar clips the full protocol diagnostic after this
    // unambiguous prefix; the model-level test covers the complete label.
    tui.wait_for("v1/http+");
    tui.wait_for("mock_session");
    tui.wait_for("counter");
    tui.wait_for_source_marker('▶', 9);

    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    tui.wait_for_source_marker('▶', 10);

    let output = tui.quit();
    assert!(output.contains("\u{1b}[?1049l"));
    drop(proxy);
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
    // Source navigation is valid only after the toolbar has a thread target.
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
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/sessions/0/all_threads_stopped")
            .and_then(Value::as_bool)
            == Some(true)
    });

    let mut tui = Tui::spawn(&ddb.base_url());
    tui.wait_for("DDB Debugger");
    tui.wait_for("connected");
    tui.wait_for("stopped");
    tui.wait_for("1 sessions · 1 groups");
    // Startup symbol names vary across debugger and linker versions; assert the
    // stable frame index and executable identity rendered by both backends.
    tui.wait_for("#0");
    tui.wait_for("ddb_real_loop");
    // Session stop output may precede the API thread projection on LLDB.
    // Source navigation is valid only after the toolbar has a thread target.
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
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints")
            .and_then(Value::as_array)
            .is_some_and(|breakpoints| breakpoints.len() == 1)
    });

    tui.clear_capture();
    tui.send(b"\x1b[15~");
    tui.wait_for("continue:");
    tui.wait_for("exec · stopped");
    tui.wait_for("breakpoint_target");
    tui.wait_for("counter");
    tui.wait_for("std::hint::black_box");
    tui.wait_for("hits:1");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints/0/times")
            .and_then(Value::as_u64)
            == Some(1)
    });
    let thread_id = ddb.global_thread_id();
    let before_step_line = ddb.top_frame_line(thread_id);
    assert_eq!(before_step_line, fixture.breakpoint_line);
    tui.wait_for_source_marker('▶', before_step_line);

    tui.clear_capture();
    tui.send(b"b");
    tui.wait_for("delete breakpoint");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    });
    tui.wait_for("0 breakpoints");

    tui.clear_capture();
    tui.send(b"b");
    tui.wait_for("DDB breakpoint targets");
    tui.send(b"\r");
    tui.wait_for("1 breakpoints");
    ddb.wait_for_json("/api/v1/state", |body| {
        body.pointer("/data/breakpoints")
            .and_then(Value::as_array)
            .is_some_and(|breakpoints| breakpoints.len() == 1)
    });
    tui.clear_capture();
    tui.send(b"\x1b[21~");
    tui.wait_for("next:");
    tui.wait_for("exec · stopped");
    let after_step_line = ddb.wait_for_top_frame_line_change(thread_id, before_step_line);
    tui.wait_for_source_marker('▶', after_step_line);
    assert_ne!(after_step_line, before_step_line);

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
    client: Client,
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
        let client = Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .expect("HTTP client should build");
        let mut ddb = Self {
            _tempdir: tempdir,
            child,
            stdin,
            client,
            base_url: format!("http://127.0.0.1:{port}"),
            output,
            stopped: false,
        };
        ddb.wait_until("DDB readiness", |ddb| {
            ddb.client
                .get(format!("{}/api/v1/health/ready", ddb.base_url))
                .send()
                .is_ok_and(|response| response.status().is_success())
        });
        ddb
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn wait_for_json(&mut self, path: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        let mut last_response = "no response".to_string();
        loop {
            self.assert_running();
            if let Ok(response) = self.client.get(format!("{}{}", self.base_url, path)).send() {
                let status = response.status();
                if let Ok(text) = response.text() {
                    last_response = format!("HTTP {status}: {text}");
                    if let Ok(body) = serde_json::from_str::<Value>(&text) {
                        let matches = predicate(&body);
                        if matches {
                            return body;
                        }
                        last_response.push_str(&format!(
                            "\nParsed sessions: {:?}; predicate: {matches}",
                            body.pointer("/data/sessions")
                        ));
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {path}\nLast response: {last_response}\nDDB output:\n{}",
                self.output_text()
            );
            thread::sleep(POLL);
        }
    }

    fn api_post_json(&self, path: &str, body: &Value) -> Value {
        self.client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .expect("API request should succeed")
            .error_for_status()
            .expect("API status should be successful")
            .json()
            .expect("API response should be JSON")
    }

    fn global_thread_id(&self) -> u64 {
        let body = self.api_post_json("/api/v1/threads/query", &serde_json::json!({}));
        body.pointer("/data/result/responses/0/payload/threads/0/id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse().ok())
            .expect("thread query should return a global thread id")
    }

    fn top_frame_line(&self, thread_id: u64) -> usize {
        let body = self.api_post_json(
            "/api/v1/stack/frames",
            &serde_json::json!({"thread_id": thread_id, "low": 0, "high": 1}),
        );
        body.pointer("/data/result/responses/0/payload/stack/0/line")
            .and_then(Value::as_str)
            .and_then(|line| line.parse().ok())
            .expect("stack query should return a top-frame source line")
    }

    fn wait_for_top_frame_line_change(&mut self, thread_id: u64, previous: usize) -> usize {
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
    stopped: bool,
}

impl Tui {
    fn spawn(api: &str) -> Self {
        Self::spawn_with_refresh(api, 10_000)
    }

    fn spawn_with_refresh(api: &str, refresh_ms: u64) -> Self {
        Self::spawn_with_options(api, refresh_ms, None)
    }

    fn spawn_v1_fallback(api: &str) -> Self {
        Self::spawn_with_options(api, 10_000, Some("v1-fallback"))
    }

    fn spawn_with_options(api: &str, refresh_ms: u64, api_version: Option<&str>) -> Self {
        let tui_binary = PathBuf::from(env!("CARGO_BIN_EXE_ddb-tui"));
        let api_version = api_version
            .map(|version| format!(" --api-version {version}"))
            .unwrap_or_default();
        let command = format!(
            "stty rows 40 cols 140; exec {} --api {} --refresh-ms {refresh_ms}{api_version}",
            shell_quote(&tui_binary),
            shell_quote(Path::new(api)),
        );
        Self::spawn_shell(&command)
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
        Self::spawn_shell(&command)
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
        Self::spawn_shell(&command)
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
        Self::spawn_shell(&command)
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
        drain_terminal(
            child.stdout.take().expect("TUI stdout should be piped"),
            output.clone(),
            frames.clone(),
        );
        drain(
            child.stderr.take().expect("TUI stderr should be piped"),
            output.clone(),
        );
        Self {
            child,
            stdin,
            output,
            frames,
            stopped: false,
        }
    }

    fn send(&mut self, input: &[u8]) {
        self.stdin
            .write_all(input)
            .expect("TUI input should be written");
        self.stdin.flush().expect("TUI input should flush");
    }

    fn wait_for(&mut self, needle: &str) {
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
        self.stopped = true;
    }

    #[cfg(target_os = "linux")]
    fn finish_external_exit(&mut self) -> String {
        if !self.stopped {
            wait_or_kill(&mut self.child);
            self.stopped = true;
        }
        self.output_text()
    }

    fn quit(&mut self) -> String {
        if !self.stopped {
            self.send(b"q");
            wait_or_kill(&mut self.child);
            self.stopped = true;
        }
        self.output_text()
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stdin.write_all(b"q");
            let _ = self.stdin.flush();
            wait_or_kill(&mut self.child);
            self.stopped = true;
        }
    }
}

struct V1OnlyProxy {
    base_url: String,
    running: Arc<AtomicBool>,
    listener: Option<thread::JoinHandle<()>>,
}

impl V1OnlyProxy {
    fn spawn(upstream_url: &str) -> Self {
        let upstream = upstream_url
            .strip_prefix("http://")
            .expect("test DDB endpoint should use HTTP")
            .to_string();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("v1-only proxy listener should bind");
        listener
            .set_nonblocking(true)
            .expect("v1-only proxy should become nonblocking");
        let address = listener
            .local_addr()
            .expect("v1-only proxy address should be available");
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        let listener_thread = thread::spawn(move || {
            while thread_running.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let upstream = upstream.clone();
                        thread::spawn(move || proxy_connection(client, &upstream));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL);
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            base_url: format!("http://{address}"),
            running,
            listener: Some(listener_thread),
        }
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }
}

impl Drop for V1OnlyProxy {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(address) = self.base_url.strip_prefix("http://") {
            let _ = TcpStream::connect(address);
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

fn proxy_connection(mut client: TcpStream, upstream_address: &str) {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let Ok(read) = client.read(&mut buffer) else {
            return;
        };
        if read == 0 || request.len().saturating_add(read) > MAX_HEADER_BYTES {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let request_line = request
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .unwrap_or_default();
    let path = request_line.split_ascii_whitespace().nth(1).unwrap_or("");
    if path == "/api/v2" || path.starts_with("/api/v2/") {
        let body = b"DDB API v2 is unavailable in this migration fixture";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = client.write_all(response.as_bytes());
        let _ = client.write_all(body);
        return;
    }

    let Ok(mut upstream) = TcpStream::connect(upstream_address) else {
        return;
    };
    if upstream.write_all(&request).is_err() {
        return;
    }
    let Ok(mut upstream_reader) = upstream.try_clone() else {
        return;
    };
    let Ok(mut client_writer) = client.try_clone() else {
        return;
    };
    let response = thread::spawn(move || {
        let _ = std::io::copy(&mut upstream_reader, &mut client_writer);
        let _ = client_writer.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut client, &mut upstream);
    let _ = upstream.shutdown(Shutdown::Write);
    let _ = response.join();
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

fn drain(mut reader: impl Read + Send + 'static, output: Arc<Mutex<Vec<u8>>>) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => output.lock().unwrap().extend_from_slice(&buffer[..read]),
            }
        }
    });
}

fn drain_terminal(
    mut reader: impl Read + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
    frames: Arc<Mutex<VecDeque<String>>>,
) {
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
    });
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

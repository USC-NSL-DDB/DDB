mod support;

use std::{
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use support::{
    build_real_loop_example, libfaketime_path, real_test_guard, session_id_by_tag,
    AttachSessionSpec, BinarySessionSpec, DdbProcess,
};

const DEBUGGER_PAUSE: Duration = Duration::from_millis(350);
const ACTION_STOP_PAUSE: Duration = Duration::from_millis(100);
const MAX_COMPENSATED_DELTA_NS: i128 = 150_000_000;
const FAKETIME_INITIAL_VALUE: &str = "-00000000000000000000.000000000";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResumeAction {
    Continue,
    Next,
    Step,
    Finish,
}

impl ResumeAction {
    fn operation(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Next => "next",
            Self::Step => "step",
            Self::Finish => "finish",
        }
    }

    fn command(self, token: u64, sid: u64, gtid: u64) -> String {
        match self {
            Self::Continue => {
                format!("{token}-record-time-and-continue --session {sid}")
            }
            Self::Next | Self::Step | Self::Finish => format!(
                "{token}-record-time-and-{} --thread {gtid}",
                self.operation()
            ),
        }
    }
}

const RESUME_ACTIONS: [ResumeAction; 4] = [
    ResumeAction::Continue,
    ResumeAction::Next,
    ResumeAction::Step,
    ResumeAction::Finish,
];

struct Debuggee(Child);

impl Debuggee {
    fn pid(&self) -> u64 {
        u64::from(self.0.id())
    }
}

impl Drop for Debuggee {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn assert_launch_resume_actions_hide_debugger_pauses(backend: &str) {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let report_dir = tempfile::tempdir().expect("clock report tempdir should be created");
    let report_path = report_dir.path().join(format!("{backend}-launch-clock"));
    let binary_path = example
        .binary_path
        .to_str()
        .expect("fixture binary path should be valid utf-8");
    let source_path = example
        .source_path
        .to_str()
        .expect("fixture source path should be valid utf-8");
    let tag = format!("real-{backend}-faketime-launch");
    let report_arg = report_path
        .to_str()
        .expect("clock report path should be valid utf-8")
        .to_string();
    let libfaketime = libfaketime_path();

    let mut ddb = DdbProcess::spawn_faketime_binary_sessions(
        backend,
        &[BinarySessionSpec {
            tag: &tag,
            alias: &tag,
            hash: "grp-real-faketime-launch",
            pid: 9_201,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path,
            binary_args: vec![
                "--sleep-ms".to_string(),
                "10".to_string(),
                "--max-iterations".to_string(),
                "100000".to_string(),
                "--clock-report".to_string(),
                report_arg,
            ],
            stop_at_entry: true,
        }],
        &libfaketime,
    );

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&sessions, &tag);
    ddb.send_cmd(&format!(
        "701-break-insert --session {sid} {}:{}",
        source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("701^done");

    ddb.send_cmd(&format!("702-exec-continue --session {sid}"));
    let sid_needle = format!("session-id=\"{sid}\"");
    ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_needle.as_str(),
    ]);
    let gtid = resolve_single_thread_gtid(&mut ddb, sid);

    let mut stopped_count = 2;
    let mut breakpoint_hit_count = if backend == "gdb" { 2 } else { 1 };
    for (index, action) in RESUME_ACTIONS.into_iter().enumerate() {
        remove_clock_report(&report_path);
        thread::sleep(DEBUGGER_PAUSE);

        let token = 710 + (index as u64) * 10;
        let command = action.command(token, sid, gtid);
        ddb.send_cmd(&command);
        let resumed = if action == ResumeAction::Continue {
            ddb.wait_for_stdout_line(&format!("{token}^running"))
        } else {
            command
        };

        if action != ResumeAction::Continue {
            stopped_count += 1;
            ddb.wait_for_stdout_count("*stopped", stopped_count);
            thread::sleep(ACTION_STOP_PAUSE);

            let followup_token = token + 1;
            ddb.send_cmd(&format!(
                "{followup_token}-record-time-and-continue --session {sid}"
            ));
            ddb.wait_for_stdout_line(&format!("{followup_token}^running"));
        }

        stopped_count += 1;
        breakpoint_hit_count += 1;
        ddb.wait_for_stdout_count("*stopped", stopped_count);
        ddb.wait_for_stdout_count("reason=\"breakpoint-hit\"", breakpoint_hit_count);

        let report = read_clock_report(&report_path);
        assert_compensated_clock_delta(backend, action.operation(), &resumed, &report);
    }
}

fn assert_attach_pause_is_hidden_from_inferior_clock(backend: &str) {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let report_dir = tempfile::tempdir().expect("clock report tempdir should be created");
    let report_path = report_dir.path().join(format!("{backend}-attach-clock"));
    let libfaketime = libfaketime_path();
    let debuggee = spawn_faketime_debuggee(
        &example.binary_path,
        &report_path,
        &libfaketime,
        FAKETIME_INITIAL_VALUE,
        Some("1"),
    );
    let tag = format!("real-{backend}-faketime-attach");
    let mut ddb = DdbProcess::spawn_attach_sessions(
        backend,
        &[AttachSessionSpec {
            tag: &tag,
            alias: &tag,
            hash: "grp-real-faketime-attach",
            pid: debuggee.pid(),
            ip: "127.0.0.1",
        }],
    );

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&sessions, &tag);
    let source_path = example
        .source_path
        .to_str()
        .expect("fixture source path should be valid utf-8");
    ddb.send_cmd(&format!(
        "801-break-insert --session {sid} {}:{}",
        source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("801^done");

    ddb.send_cmd(&format!("802-record-time-and-continue --session {sid}"));
    ddb.wait_for_stdout_line("802^running");
    ddb.wait_for_stdout_count("reason=\"breakpoint-hit\"", 1);

    remove_clock_report(&report_path);
    thread::sleep(DEBUGGER_PAUSE);
    ddb.send_cmd(&format!("803-record-time-and-continue --session {sid}"));
    let resumed = ddb.wait_for_stdout_line("803^running");
    ddb.wait_for_stdout_count("reason=\"breakpoint-hit\"", 2);

    let report = read_clock_report(&report_path);
    assert_compensated_clock_delta(backend, "attach continue", &resumed, &report);
}

fn assert_attach_configuration_failures_are_fail_closed(backend: &str) {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let libfaketime = libfaketime_path();
    let cases = [
        (
            "missing-no-cache",
            FAKETIME_INITIAL_VALUE,
            None,
            "FAKETIME_NO_CACHE=1",
        ),
        ("short-buffer", "-0", Some("1"), "buffer is too small"),
    ];

    for (index, (case, faketime, no_cache, expected_error)) in cases.into_iter().enumerate() {
        let report_dir = tempfile::tempdir().expect("clock report tempdir should be created");
        let report_path = report_dir.path().join(format!("{backend}-{case}-clock"));
        let debuggee = spawn_faketime_debuggee(
            &example.binary_path,
            &report_path,
            &libfaketime,
            faketime,
            no_cache,
        );
        let tag = format!("real-{backend}-faketime-{case}");
        let mut ddb = DdbProcess::spawn_attach_sessions(
            backend,
            &[AttachSessionSpec {
                tag: &tag,
                alias: &tag,
                hash: "grp-real-faketime-failure",
                pid: debuggee.pid(),
                ip: "127.0.0.1",
            }],
        );

        let sessions = ddb.wait_for_sessions_len(1);
        ddb.wait_for_stdout_count("*stopped", 1);
        let sid = session_id_by_tag(&sessions, &tag);
        remove_clock_report(&report_path);
        thread::sleep(DEBUGGER_PAUSE);

        let token = 850 + index as u64;
        ddb.send_cmd(&format!("{token}-record-time-and-continue --session {sid}"));
        let error = ddb.wait_for_stdout_line(&format!("{token}^error"));
        assert!(
            error.contains(expected_error),
            "{backend} {case} returned the wrong synchronization error: {error}"
        );

        thread::sleep(Duration::from_millis(200));
        assert!(
            !report_path.exists(),
            "{backend} resumed the inferior after the {case} synchronization failure"
        );
    }
}

fn spawn_faketime_debuggee(
    binary_path: &Path,
    report_path: &Path,
    libfaketime: &Path,
    faketime: &str,
    no_cache: Option<&str>,
) -> Debuggee {
    let mut command = Command::new(binary_path);
    command
        .args(["--sleep-ms", "10", "--max-iterations", "100000"])
        .arg("--clock-report")
        .arg(report_path)
        .env("LD_PRELOAD", libfaketime)
        .env("FAKETIME", faketime)
        .env("FAKETIME_DONT_FAKE_MONOTONIC", "1")
        .env("FAKETIME_DISABLE_SHM", "1");
    match no_cache {
        Some(value) => {
            command.env("FAKETIME_NO_CACHE", value);
        }
        None => {
            command.env_remove("FAKETIME_NO_CACHE");
        }
    }
    Debuggee(
        command
            .spawn()
            .expect("real faketime attach fixture should spawn"),
    )
}

fn resolve_single_thread_gtid(ddb: &mut DdbProcess, sid: u64) -> u64 {
    ddb.send_cmd(&format!("704-thread-info --session {sid}"));
    let output = ddb.wait_for_stdout_line("704^done");
    let (_, threads) = output.split_once("threads=[").unwrap_or_else(|| {
        panic!("thread-info output should include a threads payload, got: {output}");
    });

    for (offset, _) in threads.match_indices("id=\"") {
        if offset == 0 {
            continue;
        }
        let prefix = threads.as_bytes()[offset - 1];
        if prefix != b'{' && prefix != b',' {
            continue;
        }
        let id = threads[offset + 4..]
            .split('"')
            .next()
            .expect("thread id should terminate with a quote");
        return id.parse::<u64>().unwrap_or_else(|error| {
            panic!("thread id should be a valid integer in `{output}`: {error}");
        });
    }

    panic!("thread-info output should include a thread id, got: {output}");
}

fn remove_clock_report(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "clock report {} should be removable: {error}",
            path.display()
        ),
    }
}

fn read_clock_report(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) => return contents,
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!(
                "clock report {} should be readable: {error}",
                path.display()
            ),
        }
    }
}

fn assert_compensated_clock_delta(backend: &str, action: &str, resumed: &str, report: &str) {
    let delta_ns = report
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("delta_ns="))
        .expect("clock report should begin with delta_ns")
        .parse::<i128>()
        .expect("clock delta should be an integer");
    assert!(
        delta_ns.abs() < MAX_COMPENSATED_DELTA_NS,
        "{backend} {action} exposed a debugger pause to the inferior clock: \
         observed delta was {delta_ns} ns after a {} ns debugger pause\n{resumed}\n{report}",
        DEBUGGER_PAUSE.as_nanos(),
    );
    assert!(
        report.contains("no_cache=1"),
        "{backend} {action} did not run with FAKETIME_NO_CACHE=1:\n{report}"
    );
    assert!(
        report.contains("libfaketime_loaded=true"),
        "{backend} {action} did not load libfaketime:\n{report}"
    );
}

#[test]
fn gdb_launch_actions_hide_debugger_pauses_from_inferior_clock() {
    assert_launch_resume_actions_hide_debugger_pauses("gdb");
}

#[test]
fn lldb_launch_actions_hide_debugger_pauses_from_inferior_clock() {
    assert_launch_resume_actions_hide_debugger_pauses("lldb");
}

#[test]
fn gdb_attach_hides_debugger_pause_from_inferior_clock() {
    assert_attach_pause_is_hidden_from_inferior_clock("gdb");
}

#[test]
fn lldb_attach_hides_debugger_pause_from_inferior_clock() {
    assert_attach_pause_is_hidden_from_inferior_clock("lldb");
}

#[test]
fn gdb_attach_configuration_failures_do_not_resume_inferior() {
    assert_attach_configuration_failures_are_fail_closed("gdb");
}

#[test]
fn lldb_attach_configuration_failures_do_not_resume_inferior() {
    assert_attach_configuration_failures_are_fail_closed("lldb");
}

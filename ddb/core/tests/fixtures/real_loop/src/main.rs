use std::{
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
fn allow_debugger_attach() {
    const PR_SET_PTRACER: i32 = 0x5961_6d61;
    const PR_SET_PTRACER_ANY: usize = usize::MAX;

    unsafe extern "C" {
        fn prctl(option: i32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> i32;
    }

    // The integration debugger is a sibling process under the test harness.
    // Permit that relationship under Linux Yama ptrace_scope=1.
    let _ = unsafe { prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY, 0, 0, 0) };
}

#[cfg(not(target_os = "linux"))]
fn allow_debugger_attach() {}

#[derive(Debug)]
struct DebugRequest {
    headers: [u64; 2],
    flags: u64,
}

#[inline(never)]
fn breakpoint_target(counter: u64) -> u64 {
    let request = DebugRequest {
        headers: [counter, counter.wrapping_add(10)],
        flags: 3,
    };
    std::hint::black_box(request.headers[1]);
    std::hint::black_box(counter.wrapping_add(request.flags - 2)) // BREAKPOINT_MARKER
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn realtime_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("fixture clock should be after the Unix epoch")
        .as_nanos() as i128
}

fn write_clock_delta(path: &Path, before_ns: i128, after_ns: i128) {
    let mappings = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let report = format!(
        "delta_ns={}\nfaketime={}\nno_cache={}\nld_preload={}\nlibfaketime_loaded={}",
        after_ns - before_ns,
        std::env::var("FAKETIME").unwrap_or_else(|_| "<missing>".to_string()),
        std::env::var("FAKETIME_NO_CACHE").unwrap_or_else(|_| "<missing>".to_string()),
        std::env::var("LD_PRELOAD").unwrap_or_else(|_| "<missing>".to_string()),
        mappings.contains("libfaketime"),
    );
    std::fs::write(path, report).expect("clock report should be written");
}

fn main() {
    allow_debugger_attach();
    let args = std::env::args().collect::<Vec<_>>();
    let sleep_ms = parse_arg(&args, "--sleep-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(25);
    let max_iterations = parse_arg(&args, "--max-iterations")
        .and_then(|value| value.parse::<u64>().ok());
    let clock_report = parse_arg(&args, "--clock-report").map(std::path::PathBuf::from);
    if let Some(pid_file) = parse_arg(&args, "--pid-file") {
        std::fs::write(pid_file, std::process::id().to_string())
            .expect("fixture PID file should be written");
    }

    let mut counter = 0u64;
    loop {
        let before_ns = realtime_nanos();
        counter = breakpoint_target(counter);
        if let Some(path) = clock_report.as_deref() {
            write_clock_delta(path, before_ns, realtime_nanos());
        }
        if let Some(max_iterations) = max_iterations {
            if counter >= max_iterations {
                break;
            }
        }
        thread::sleep(Duration::from_millis(sleep_ms));
    }
    println!("completed iterations={counter}");
}

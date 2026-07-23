use std::{thread, time::Duration};

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

#[inline(never)]
fn breakpoint_target(counter: u64) -> u64 {
    std::hint::black_box(counter.wrapping_add(1)) // BREAKPOINT_MARKER
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn main() {
    allow_debugger_attach();
    let args = std::env::args().collect::<Vec<_>>();
    let sleep_ms = parse_arg(&args, "--sleep-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(25);
    let max_iterations = parse_arg(&args, "--max-iterations")
        .and_then(|value| value.parse::<u64>().ok());

    let mut counter = 0u64;
    loop {
        counter = breakpoint_target(counter);
        if let Some(max_iterations) = max_iterations {
            if counter >= max_iterations {
                break;
            }
        }
        thread::sleep(Duration::from_millis(sleep_ms));
    }
    println!("completed iterations={counter}");
}

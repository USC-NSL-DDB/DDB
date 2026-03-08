use std::{thread, time::Duration};

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

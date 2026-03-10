#![crate_name = "DDB"]
#![allow(non_snake_case)]

use std::{
    fs,
    net::Ipv4Addr,
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteMeta {
    pub caller_comm_ip: u32,
    pub pid: u64,
    pub tid: u64,
    pub proclet_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CallerContext {
    pub pc: u64,
    pub sp: u64,
    pub fp: u64,
    pub lr: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BacktraceMeta {
    pub meta: RemoteMeta,
    pub ctx: CallerContext,
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn required_arg(args: &[String], flag: &str) -> String {
    parse_arg(args, flag).unwrap_or_else(|| panic!("missing required argument {flag}"))
}

fn parse_u64(args: &[String], flag: &str) -> u64 {
    required_arg(args, flag)
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("invalid value for {flag}"))
}

fn parse_usize(args: &[String], flag: &str) -> usize {
    required_arg(args, flag)
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("invalid value for {flag}"))
}

fn parse_ipv4_as_u32(args: &[String], flag: &str) -> Option<u32> {
    parse_arg(args, flag).map(|value| {
        value
            .parse::<Ipv4Addr>()
            .map(u32::from)
            .unwrap_or_else(|_| panic!("invalid IPv4 value for {flag}"))
    })
}

fn wait_for_parent_context(path: &str) -> CallerContext {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Path::new(path).exists() {
            return read_context(path);
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for parent context file {}", path);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_context(path: &str) -> CallerContext {
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read context file {}: {}", path, error));
    let mut context = CallerContext {
        pc: 0,
        sp: 0,
        fp: 0,
        lr: 0,
    };

    for line in contents.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .trim()
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("invalid context value in {}", path));
        match name.trim() {
            "pc" => context.pc = value,
            "sp" => context.sp = value,
            "fp" => context.fp = value,
            "lr" => context.lr = value,
            _ => {}
        }
    }

    context
}

#[cfg(target_arch = "x86_64")]
fn trap_here() {
    unsafe {
        std::arch::asm!("int3");
    }
}

#[cfg(target_arch = "aarch64")]
fn trap_here() {
    unsafe {
        std::arch::asm!("brk #0xf000");
    }
}

#[inline(never)]
fn park_at_context() -> ! {
    trap_here();
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

pub mod Backtrace {
    use super::{park_at_context, BacktraceMeta};

    #[inline(never)]
    pub fn extraction(meta_arg: BacktraceMeta, _self_ctx_path: &str) -> ! {
        let meta = meta_arg;
        std::hint::black_box(&meta);
        park_at_context()
    }
}

macro_rules! define_hops {
    ($(($name:ident, $idx:literal)),* $(,)?) => {
        $(
            #[inline(never)]
            fn $name(meta: BacktraceMeta, self_ctx_path: &str) -> ! {
                Backtrace::extraction(meta, self_ctx_path)
            }
        )*
    };
}

define_hops!(
    (hop_2, 2),
    (hop_3, 3),
    (hop_4, 4),
    (hop_5, 5),
    (hop_6, 6),
    (hop_7, 7),
    (hop_8, 8),
    (hop_9, 9),
    (hop_10, 10),
    (hop_11, 11),
    (hop_12, 12),
    (hop_13, 13),
    (hop_14, 14),
    (hop_15, 15),
    (hop_16, 16),
);

#[inline(never)]
fn root_stop(_self_ctx_path: &str) -> ! {
    park_at_context()
}

fn build_meta(args: &[String]) -> Option<BacktraceMeta> {
    let parent_ctx_path = parse_arg(args, "--parent-ctx-file")?;
    let parent_context = wait_for_parent_context(&parent_ctx_path);
    let caller_ip = parse_ipv4_as_u32(args, "--caller-ip")
        .unwrap_or_else(|| panic!("missing --caller-ip when parent context is configured"));
    let caller_pid = parse_u64(args, "--caller-pid");
    let caller_tid = parse_u64(args, "--caller-tid");

    Some(BacktraceMeta {
        meta: RemoteMeta {
            caller_comm_ip: caller_ip,
            pid: caller_pid,
            tid: caller_tid,
            proclet_id: 0,
        },
        ctx: parent_context,
    })
}

fn dispatch(role_index: usize, meta: Option<BacktraceMeta>, self_ctx_path: &str) -> ! {
    match (role_index, meta) {
        (1, _) => root_stop(self_ctx_path),
        (2, Some(meta)) => hop_2(meta, self_ctx_path),
        (3, Some(meta)) => hop_3(meta, self_ctx_path),
        (4, Some(meta)) => hop_4(meta, self_ctx_path),
        (5, Some(meta)) => hop_5(meta, self_ctx_path),
        (6, Some(meta)) => hop_6(meta, self_ctx_path),
        (7, Some(meta)) => hop_7(meta, self_ctx_path),
        (8, Some(meta)) => hop_8(meta, self_ctx_path),
        (9, Some(meta)) => hop_9(meta, self_ctx_path),
        (10, Some(meta)) => hop_10(meta, self_ctx_path),
        (11, Some(meta)) => hop_11(meta, self_ctx_path),
        (12, Some(meta)) => hop_12(meta, self_ctx_path),
        (13, Some(meta)) => hop_13(meta, self_ctx_path),
        (14, Some(meta)) => hop_14(meta, self_ctx_path),
        (15, Some(meta)) => hop_15(meta, self_ctx_path),
        (16, Some(meta)) => hop_16(meta, self_ctx_path),
        (_, None) => panic!("role {role_index} requires parent metadata"),
        _ => panic!("unsupported role index {role_index}"),
    }
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let _logical_pid = parse_u64(&args, "--logical-pid");
    let role_index = parse_usize(&args, "--role-index");
    let self_ctx_path = required_arg(&args, "--self-ctx-file");
    let meta = build_meta(&args);
    dispatch(role_index, meta, &self_ctx_path);
}

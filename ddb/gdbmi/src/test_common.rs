use camino::Utf8PathBuf;
use duct::cmd;
use lazy_static::lazy_static;
use std::{
    collections::HashSet,
    sync::{Mutex, MutexGuard, Once, PoisonError},
};

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .pretty()
            .init();

        color_eyre::install().unwrap();
    });
}

pub type Result = eyre::Result<()>;

lazy_static! {
    static ref RECORDED: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    static ref BUILT: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

fn crate_root() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sample_dir(name: &str) -> Utf8PathBuf {
    crate_root().join("samples").join(name)
}

fn sample_manifest(name: &str) -> Utf8PathBuf {
    sample_dir(name).join("Cargo.toml")
}

fn sample_binary(name: &str) -> Utf8PathBuf {
    crate_root()
        .join("target")
        .join("test-fixtures")
        .join(name)
        .join("debug")
        .join(name)
}

#[cfg(feature = "test_rr")]
fn sample_trace_dir(name: &str) -> Utf8PathBuf {
    crate_root().join("samples").join(".trace").join(name)
}

fn lock<'a>(mutex: &'a Mutex<HashSet<String>>) -> MutexGuard<'a, HashSet<String>> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn build(name: &str) -> String {
    let mut built = lock(&BUILT);
    if !built.contains(name) {
        let manifest = sample_manifest(name);
        let target_dir = crate_root().join("target").join("test-fixtures").join(name);
        cmd!(
            "cargo",
            "build",
            "--manifest-path",
            manifest.as_str(),
            "--target-dir",
            target_dir.as_str()
        )
        // These samples are debugger fixtures. Preserve their symbols even when
        // the parent build strips workspace debug information for CI speed.
        .env("CARGO_PROFILE_DEV_DEBUG", "2")
        .env("CARGO_PROFILE_DEV_OPT_LEVEL", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "none")
        .dir(crate_root())
        .stdin_null()
        .stdout_null()
        .stderr_null()
        .run()
        .expect("Failed to build sample");
        built.insert(name.to_owned());
    }

    sample_binary(name).into_string()
}

#[cfg(feature = "test_rr")]
pub fn record(name: &str) -> String {
    let trace_out = sample_trace_dir(name);

    let mut recorded = lock(&RECORDED);
    if !recorded.contains(name) {
        let bin = build(name);

        std::fs::create_dir_all(trace_out.parent().unwrap()).expect("Failed to create trace dir");
        let _result = std::fs::remove_dir_all(&trace_out);

        cmd!(
            "rr",
            "record",
            "--output-trace-dir",
            trace_out.as_str(),
            bin
        )
        .stdin_null()
        .stdout_null()
        .stderr_null()
        .run()
        .expect("Failed to record sample");

        recorded.insert(name.to_owned());
    }

    trace_out.into_string()
}

pub fn build_hello_world() -> String {
    build("hello_world")
}

pub fn hello_world_source() -> Utf8PathBuf {
    sample_dir("hello_world").join("src").join("main.rs")
}

#[cfg(feature = "test_rr")]
pub fn record_hello_world() -> String {
    record("hello_world")
}

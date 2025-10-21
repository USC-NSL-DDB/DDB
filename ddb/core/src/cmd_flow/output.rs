use std::any::Any;

use gdbmi::raw::{Dict, Value};
use tracing::{debug, error};

use crate::{
    dbg_parser::gdb_parser::MIFormatter,
    discovery::discovery_message_producer::ServiceMeta,
    state::{get_group_mgr, GroupId, STATES},
};

use super::{FinishedCmd, ParsedSessionResponse};

pub trait DynFormatter: Send + Sync {
    fn transform_dyn(&self, responses: FinishedCmd) -> Box<dyn Any>;
    fn format_dyn(&self, input: &Box<dyn Any>) -> String;
}

impl<T: Formatter> DynFormatter for T
where
    T: Send + Sync,
    T::Tranformed: 'static + Any, // Ensures type erasure compatibility
{
    fn transform_dyn(&self, responses: FinishedCmd) -> Box<dyn Any> {
        Box::new(self.transform(responses)) // Convert to Box<dyn Any>
    }

    fn format_dyn(&self, input: &Box<dyn Any>) -> String {
        input
            .downcast_ref::<T::Tranformed>() // Attempt to cast back
            .map(|t| self.format(t))
            .ok_or("Failed to downcast")
            .expect(format!("Formatter type {}", std::any::type_name::<T>()).as_str())
    }
}

pub trait Formatter {
    type Tranformed;

    // transform the responses, e.g. swap thread id with our own tracked global id
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed;
    // format the responses into a string. (ready to be printed)
    fn format(&self, input: &Self::Tranformed) -> String;
}

#[derive(Clone, Debug)]
pub struct NullFormatter;
impl Formatter for NullFormatter {
    type Tranformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        responses
    }

    #[inline]
    #[allow(unused_variables)]
    fn format(&self, input: &Self::Tranformed) -> String {
        "".to_string()
    }
}

#[derive(Clone)]
pub struct PlainFormatter;
impl Formatter for PlainFormatter {
    type Tranformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        responses
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        let r = input.get_responses().first().unwrap();
        MIFormatter::format(
            "^",
            r.get_message(),
            r.get_payload(),
            input.get_external_token(),
        )
    }
}

/// UnitFormatter outputs all responses in their original form.
#[derive(Clone)]
pub struct UnitFormatter;
impl Formatter for UnitFormatter {
    type Tranformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        responses
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        let formatted = input
            .get_responses()
            .iter()
            .map(|r| {
                MIFormatter::format(
                    "^",
                    r.get_message(),
                    r.get_payload(),
                    input.get_external_token(),
                )
            })
            .collect::<Vec<_>>();
        formatted.join("\n")
    }
}

/// handle `-thread-info` command response
pub struct ThreadInfoFormatter;
impl Formatter for ThreadInfoFormatter {
    // (token, transformed responses)
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        let mut all_thread_info = Vec::<Value>::new();

        for resp in responses.get_responses() {
            let sid = resp.get_sid();
            if let Some(payload) = resp.get_payload() {
                if let Value::List(threads) = &payload["threads"] {
                    for t in threads {
                        let mut t = t.expect_dict_ref().unwrap().clone();
                        let gtid = {
                            let tid = t["id"].expect_string_ref().unwrap();
                            let tid = tid.parse::<u64>().unwrap();
                            STATES.get_gtid(sid, tid).unwrap()
                        };
                        t.insert("id".into(), Value::String(gtid.to_string()));
                        all_thread_info.push(t.into());
                    }
                }
            }
        }

        (
            responses.get_external_token(),
            vec![
                ("threads".to_string(), all_thread_info.into()),
                (
                    "current-thread-id".to_string(),
                    STATES
                        .get_curr_gtid()
                        .map(|v| v.to_string())
                        .unwrap_or("".to_string())
                        .into(),
                ),
            ]
            .into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `-list-thread-groups` command response
pub struct ProcessInfoFormatter;
impl Formatter for ProcessInfoFormatter {
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        let mut all_process_info = Vec::<Value>::new();

        for resp in responses.get_responses() {
            let sid = resp.get_sid();
            if let Some(payload) = resp.get_payload() {
                if let Value::List(processes) = &payload["groups"] {
                    for p in processes {
                        let mut p = p.expect_dict_ref().unwrap().clone();
                        let gtgid = {
                            let tgid = p["id"].expect_string_ref().unwrap();
                            let gtgid = STATES.get_gtgid(sid, tgid).unwrap();
                            gtgid.to_string()
                        };
                        p.insert("id".into(), Value::String(gtgid));
                        all_process_info.push(p.into());
                    }
                }
            }
        }

        (
            responses.get_external_token(),
            vec![("groups".to_string(), all_process_info.into())].into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `info inferiors` command response
pub struct ProcessReadableFormatter;
impl Formatter for ProcessReadableFormatter {
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        let (token, pinfo) = ProcessInfoFormatter.transform(responses);
        let grps = pinfo["groups"].expect_list_ref().unwrap();
        let readable_pinfo: Vec<Value> = grps
            .iter()
            .map(|p| {
                let p = p.expect_dict_ref().unwrap();
                let id = p["id"].expect_string_ref().unwrap()[1..].to_string();
                let ptype = p["type"].expect_string_ref().unwrap();
                let pid = p["pid"].expect_string_ref().unwrap();
                let exec = p
                    .get("executable")
                    .unwrap_or(&Value::String("".to_string()))
                    .clone();

                Value::Dict(
                    vec![
                        ("id".to_string(), id.into()),
                        ("desc".to_string(), format!("{} {}", ptype, pid).into()),
                        ("executable".to_string(), exec),
                    ]
                    .into(),
                )
            })
            .collect();

        (
            token,
            vec![("groups".to_string(), readable_pinfo.into())].into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `info threads` command response
/// skip for now...

/// handle `thread-group-*` related async record
pub struct ThreadGroupNotifFormatter(u64); // gtgid
impl Formatter for ThreadGroupNotifFormatter {
    type Tranformed = (ParsedSessionResponse, Dict, Option<u64>);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        assert!(responses.get_responses().len() == 1);
        let resp = responses.get_responses().first().unwrap();
        let mut payload = resp.get_payload().unwrap().clone();
        payload.insert("id".into(), format!("{}", self.0).into());
        (resp.clone(), payload, responses.get_external_token())
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        // Example Output
        // =thread-group-added,id="i1"
        // =thread-group-removed,id="id"
        // =thread-group-started,id="id"
        // =thread-group-exited,id="id",exit-code="code"
        let resp = &input.0;
        let payload = &input.1;
        MIFormatter::format("=", resp.get_message(), Some(payload), input.2)
    }
}

impl ThreadGroupNotifFormatter {
    pub fn new(gtgid: u64) -> Self {
        Self(gtgid)
    }
}

/// handle `thread-created` async record
pub struct ThreadCreatedNotifFormatter {
    gtid: u64,
    gtgid: u64,
    sid: u64,
    service_meta: Option<ServiceMeta>,
}

impl ThreadCreatedNotifFormatter {
    pub fn new(gtid: u64, gtgid: u64, sid: u64, service_meta: Option<ServiceMeta>) -> Self {
        Self {
            gtid,
            gtgid,
            sid,
            service_meta,
        }
    }
}

#[allow(unused_variables)]
impl Formatter for ThreadCreatedNotifFormatter {
    type Tranformed = ();

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        ()
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        // Example Output
        // =thread-created,id="1",group-id="i1"
        let alias = self
            .service_meta
            .as_ref()
            .map(|meta| meta.alias.clone())
            .unwrap_or("UNKNOWN".to_string());
        let group_id = get_group_mgr()
            .get_group_id(self.sid)
            .unwrap_or("UNKNOWN".to_string());
        let payload: Dict = vec![
            ("id".to_string(), format!("{}", self.gtid).into()),
            ("group-id".to_string(), format!("i{}", self.gtgid).into()),
            ("session-id".to_string(), format!("{}", self.sid).into()),
            ("session-alias".to_string(), format!("{}", alias).into()),
            ("group-id".to_string(), format!("{}", group_id).into()),
        ]
        .into();
        MIFormatter::format("=", "thread-created", Some(&payload), None)
    }
}

/// handle `thread-exited` async record
pub struct ThreadExitedNotifFormatter {
    gtid: u64,
    gtgid: u64,
    sid: u64,
}

impl ThreadExitedNotifFormatter {
    #[allow(unused)]
    pub fn new(gtid: u64, gtgid: u64, sid: u64) -> Self {
        Self { gtid, gtgid, sid }
    }
}

#[allow(unused_variables)]
impl Formatter for ThreadExitedNotifFormatter {
    type Tranformed = ();

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        ()
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        // Example Output
        // =thread-exited,id="1",group-id="i1",session-id="1"
        let payload: Dict = vec![
            ("id".to_string(), format!("{}", self.gtid).into()),
            ("group-id".to_string(), format!("i{}", self.gtgid).into()),
            ("session-id".to_string(), format!("{}", self.sid).into()),
        ]
        .into();
        MIFormatter::format("=", "thread-exited", Some(&payload), None)
    }
}

/// handle `running` async record
pub struct RunningAsyncRecordFormatter {
    all_running: bool,
}

impl RunningAsyncRecordFormatter {
    pub fn new(all_running: bool) -> Self {
        Self { all_running }
    }

    #[inline]
    fn iter_over_threads(&self, responses: &FinishedCmd) -> String {
        let sid = responses.get_sid();
        STATES
            .get_gtids_by_sid(sid)
            .iter()
            .fold(format!(""), |acc, gtid| {
                acc + format!("*running,thread-id=\"{}\"\n", gtid).as_str()
            })
            .trim()
            .to_string()
    }
}

impl Formatter for RunningAsyncRecordFormatter {
    type Tranformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        responses
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        let payload = input
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .unwrap();

        // Example Output
        // *running,thread-id="all"
        // *running,thread-id="2"

        if self.all_running {
            return self.iter_over_threads(input);
        } else {
            let tid = {
                let tid = payload["thread-id"].expect_string_ref().unwrap();
                tid.parse::<u64>().unwrap()
            };
            format!(
                "*running,thread-id=\"{}\"",
                STATES.get_gtid(input.get_sid(), tid).unwrap()
            )
        }
    }
}

/// handle `stopped` async record
pub struct StopAsyncRecordFormatter;

impl Formatter for StopAsyncRecordFormatter {
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        assert!(responses.get_responses().len() == 1);
        let resp = responses.get_responses().first().unwrap();
        let mut payload = resp.get_payload().unwrap().clone();
        let sid = resp.get_sid();

        let gtid = {
            let tid = match &payload["thread-id"] {
                Value::String(s) => s.parse::<u64>().unwrap(),
                _ => unreachable!(),
            };
            STATES.get_gtid(sid, tid).unwrap()
        };
        payload.insert("thread-id".into(), gtid.to_string().into());
        payload.insert("session-id".into(), sid.to_string().into());

        let mut new_stopped_threads = Vec::new();
        match &payload["stopped-threads"] {
            Value::List(threads) => {
                for t in threads {
                    if let Value::String(tid) = t {
                        let tid = tid.parse::<u64>().unwrap();
                        new_stopped_threads.push(Value::String(
                            STATES.get_gtid(sid, tid).unwrap().to_string(),
                        ));
                    }
                }
            }
            Value::String(s) if s == "all" => {
                for gtid in STATES.get_gtids_by_sid(sid) {
                    new_stopped_threads.push(Value::String(gtid.to_string()));
                }
            }
            _ => error!("Unknown stopped-threads format"),
        }
        payload.insert("stopped-threads".into(), Value::List(new_stopped_threads));
        (responses.get_external_token().clone(), payload)
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        // Example Output
        // https://github.com/USC-NSL/distributed-debugger/issues/24#issuecomment-1938140846
        MIFormatter::format("*", "stopped", Some(&input.1), input.0)
    }
}

/// handle generic `stopped` async record (not thread-related)
pub struct GenericStopAsyncRecordFormatter;
impl Formatter for GenericStopAsyncRecordFormatter {
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        assert!(responses.get_responses().len() == 1);
        let resp = responses.get_responses().first().unwrap();
        let payload = resp.get_payload().unwrap().clone();
        (responses.get_external_token(), payload)
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        MIFormatter::format("*", "stopped", Some(&input.1), input.0)
    }
}

/// handle `-stack-list-frames` command response
// pub struct StackListFramesFormatter;

/// handle `-thread-select`
pub struct ThreadSelectFormatter(u64); // gtid
impl ThreadSelectFormatter {
    #[allow(unused)]
    pub fn new(gtid: u64) -> Self {
        Self(gtid)
    }
}

impl Formatter for ThreadSelectFormatter {
    type Tranformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Tranformed {
        let mut payload = responses
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .unwrap()
            .clone();
        payload.insert("new-thread-id".into(), Value::String(self.0.to_string()));
        (responses.get_external_token(), payload)
    }

    #[inline]
    fn format(&self, input: &Self::Tranformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// static dispatched version of the emit based on the formatter.
/// this is useful when the formatter is known at compile time.
#[inline]
pub fn emit_static<T: Formatter>(finished: FinishedCmd, formatter: T) {
    let transformed = formatter.transform(finished);
    let formatted = formatter.format(&transformed);
    println!("{}", formatted);
    debug!("output: {}", formatted);
}

/// dynamic dispatched version of the emit based on the formatter.
/// If possible, we can figure out a way to combine these two functions into one.
#[inline]
pub fn emit(finished: FinishedCmd, formatter: Box<dyn DynFormatter>) {
    let transformed = formatter.transform_dyn(finished);
    let formatted = formatter.format_dyn(&transformed);
    println!("{}", formatted);
    debug!("output: {}", formatted);
}

use std::str;

#[derive(Debug, Clone)]
pub enum AsyncRecordType {
    Exec,
    Status,
    Notify,
}

#[derive(Debug, Clone)]
pub enum StreamRecordType {
    Console,
    Target,
    Log,
}

#[derive(Debug, Clone)]
pub enum RecordType {
    // Out-of-band-record
    Async(AsyncRecordType, AsyncOutput),
    Stream(StreamRecordType, StreamOutput),

    // Result record
    Result(ResultOutput)
}

// ------ ResultOutput ------- //
#[derive(Debug, Clone)]
pub enum ResultClass {
    Done,
    Running,
    Connected,
    Error,
    Exit
}

pub struct ResultOutput {
    r'class: ResultClass
    results: Option<&[VariableResult]>,
}

// ------ AsyncOutput ------- //
#[derive(Debug, Clone)]
pub enum AsyncClass {
    Stopped,
} 

pub struct AsyncOutput {
    r'class : AsyncClass,
    results: Option<&[VariableResult]>
}

pub struct VariableResult {
    var: &str,
    // MARK: is this really str? Would generic be better?
    // TODO: implementation is not done!!!
    val: &str,
}

// ------ StreamOutput ------- //
pub struct StreamOutput {
    content: &str
}

// ------ Record ------- //
pub struct Record<'a> {
    r'type: RecordType
    token: Option<&'a str>,
    raw_output: &'a str,
}

// impl Record {
//     pub fn new(raw: &str) -> Self {
//         todo!()
//     }
// }
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

// #[derive(Debug, Clone)]
pub enum RecordType<'a> {
    // Out-of-band-record
    Async(AsyncRecordType, AsyncOutput<'a>),
    Stream(StreamRecordType, StreamOutput),

    // Result record
    Result(ResultOutput<'a>)
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

pub struct ResultOutput<'a> {
    r#class: ResultClass,
    results: Option<&'a [VariableResult]>,
}

// ------ AsyncOutput ------- //
#[derive(Debug, Clone)]
pub enum AsyncClass {
    Stopped,
} 

pub struct AsyncOutput<'a> {
    r#class : AsyncClass,
    results: Option<&'a [VariableResult]>
}

pub struct VariableResult {
    var: String, 
    // MARK: is this really str? Would generic be better?
    // TODO: implementation is not done!!!
    val: String,
}

// ------ StreamOutput ------- //
pub struct StreamOutput {
    content: String
}

// ------ Record ------- //
pub struct Record<'a> {
    r#type: RecordType<'a>,
    token: Option<&'a str>,
    raw_output: &'a str,
}

// impl Record {
//     pub fn new(raw: &str) -> Self {
//         todo!()
//     }
// }
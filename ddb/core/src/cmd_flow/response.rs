use gdbmi::raw::Dict;
use serde::Serialize;

/// Parsed response produced by one debugger session.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedSessionResponse {
    sid: u64,
    message: String,
    payload: Option<Dict>,
}

impl ParsedSessionResponse {
    pub(crate) fn new(sid: u64, message: String, payload: Option<Dict>) -> Self {
        Self {
            sid,
            message,
            payload,
        }
    }

    pub fn get_sid(&self) -> u64 {
        self.sid
    }

    pub fn get_message(&self) -> &String {
        &self.message
    }

    pub fn get_payload(&self) -> Option<&Dict> {
        self.payload.as_ref()
    }

    pub fn get_payload_mut(&mut self) -> Option<&mut Dict> {
        self.payload.as_mut()
    }
}

/// Aggregated completion for a command sent to one or more sessions.
#[derive(Debug, Serialize, Clone)]
pub struct FinishedCmd {
    external_token: Option<u64>,
    sid: u64,
    responses: Vec<ParsedSessionResponse>,
}

impl FinishedCmd {
    pub fn new(
        external_token: Option<u64>,
        sid: u64,
        responses: Vec<ParsedSessionResponse>,
    ) -> Self {
        Self {
            external_token,
            sid,
            responses,
        }
    }

    pub fn set_external_token(&mut self, external_token: u64) {
        self.external_token = Some(external_token);
    }

    pub fn get_external_token(&self) -> Option<u64> {
        self.external_token
    }

    pub fn get_sid(&self) -> u64 {
        self.sid
    }

    pub fn get_responses(&self) -> &Vec<ParsedSessionResponse> {
        &self.responses
    }

    pub fn get_responses_mut(&mut self) -> &mut Vec<ParsedSessionResponse> {
        &mut self.responses
    }
}

/// Lightweight command-runtime diagnostics used by the API status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRuntimeStatus {
    pub sid: u64,
    pub in_flight: usize,
    pub queued: usize,
    pub closed: bool,
}

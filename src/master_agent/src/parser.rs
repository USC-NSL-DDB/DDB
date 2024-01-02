use std::{str, fmt};

use crate::output::{Record, RecordType};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Nothing to parse.
    Empty,
    Misformed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Empty => write!(f, "nothing to parse"),
            Error::Misformed => write!(f, "data is misformed"),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

pub fn RecordParser(raw: &str) -> Result<Record> {
    let splitted_raw = raw.trim().split(" ").collect::<Vec<&str>>();
    if splitted_raw.is_empty() { return Err(Error::Empty); }
    
    let r#type: RecordType;

    for seg in splitted_raw {
        if seg.as_bytes()[0].is_ascii_digit() {
            // TOKEN
        } else {
            if seg == "*" {
                // r#type = RecordType::Async((), ())
            }
        }
    }


    Ok(())
}

// TODO!
// pub fn OutputParser(raw: &str) {

// }
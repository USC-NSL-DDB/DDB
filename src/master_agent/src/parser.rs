use std::{str, fmt, num::ParseIntError};

use crate::output::{Record, RecordType};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Nothing to parse.
    Empty,
    Misformed,
    WrongToken(ParseIntError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Empty => write!(f, "nothing to parse"),
            Error::Misformed => write!(f, "data is misformed"),
            Error::WrongToken(e) => write!(f, "failed to parse the token: {}", e)
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

fn parse_token(line: &str) -> (Option<&str>, &str) {
    // output uses ascii and ascii only (double-check needed)
    // Otherwise, direct operation on bytes can be problematic.
    if line.as_bytes()[0].is_ascii_digit() {
        let mut idx = 0;
        for (i, c) in line.char_indices() {
            if !c.is_ascii_digit() {
                return (Some(&line[..i]), &line[i..]);
            }
        }
    }
    (None, line)
}

pub fn RecordParser(raw: &str) -> Result<Record> {
    if raw.trim().is_empty() { return Err(Error::Empty); }

    // TODO: a special case where we might need to handle (gdb) prompt line

    let (token, rest)= parse_token(&raw);
    let (prefix, noprefix) = rest.as_bytes()
                                                .split_first()
                                                .map(| (f, r) | {
                                                    // Shouldn't be reachable for panic
                                                    let r = str::from_utf8(r).expect("Misformed output");
                                                    (*f as char, r.trim())
                                                })
                                                .ok_or(Error::Misformed)?;

    if let Some(token) = token {
        // Result record
        if prefix == '^' {
            let id: usize = token.trim().parse().map_err(|e| Error::WrongToken(e))?;

        }
    } else {

    }

    // let splitted_raw = raw.trim().split(" ").collect::<Vec<&str>>();
    // if splitted_raw.is_empty() { return Err(Error::Empty); }
    
    // let r#type: RecordType;

    // for seg in splitted_raw {
    //     if seg.as_bytes()[0].is_ascii_digit() {
    //         // TOKEN
    //     } else {
    //         if seg == "*" {
    //             // r#type = RecordType::Async((), ())
    //         }
    //     }
    // }


    todo!()
    // Ok(())
}

// TODO!
// pub fn OutputParser(raw: &str) {

// }

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn token_parse() {
        let line = "hello, world";
        let line_with_token = "12345^done";

        assert_eq!(parse_token(&line), None);
        assert_eq!(parse_token(&line_with_token), Some("12345"));
    }
}
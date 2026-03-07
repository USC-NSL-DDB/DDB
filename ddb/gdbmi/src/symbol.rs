use crate::{raw, Error};
use camino::Utf8PathBuf;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub line: u32,
    pub name: String,
    pub function_type: String,
    pub description: String,
}

pub(crate) fn from_symbol_info_functions_payload(
    mut payload: raw::Dict,
) -> Result<HashMap<Utf8PathBuf, Vec<Function>>, Error> {
    let raw = payload
        .remove_expect("symbols")?
        .expect_dict()?
        .remove_expect("debug")?
        .expect_list()?;

    let mut files = HashMap::new();

    for group in raw {
        let mut group = group.expect_dict()?;
        let filename = match group
            .remove("fullname")
            .or_else(|| group.remove("filename"))
        {
            Some(path) => path.expect_path()?,
            None => return Err(Error::ExpectedDifferentPayload),
        };

        let mut symbols = Vec::new();
        let raw_symbols = group.remove_expect("symbols")?.expect_list()?;
        for raw in raw_symbols {
            let mut raw = raw.expect_dict()?;
            let line = raw.remove_expect("line")?.expect_number()?;
            let name = raw.remove_expect("name")?.expect_string()?;
            let symbol_type = raw.remove_expect("type")?.expect_string()?;
            let description = raw.remove_expect("description")?.expect_string()?;

            symbols.push(Function {
                line,
                name,
                function_type: symbol_type,
                description,
            });
        }

        files
            .entry(filename)
            .or_insert_with(Vec::new)
            .extend(symbols);
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{Dict, Value};

    #[test]
    fn symbol_info_prefers_fullname_and_merges_groups() {
        let payload = Dict::from(vec![(
            "symbols".to_owned(),
            Dict::from(vec![(
                "debug".to_owned(),
                Value::List(vec![
                    Value::Dict(Dict::from(vec![
                        ("filename".to_owned(), Value::from("src/main.rs")),
                        (
                            "fullname".to_owned(),
                            Value::from("/tmp/hello_world/src/main.rs"),
                        ),
                        (
                            "symbols".to_owned(),
                            Value::List(vec![Value::Dict(Dict::from(vec![
                                ("line".to_owned(), Value::from("10")),
                                ("name".to_owned(), Value::from("hello_world::HelloMsg::say")),
                                ("type".to_owned(), Value::from("fn ()")),
                                (
                                    "description".to_owned(),
                                    Value::from("static fn hello_world::HelloMsg::say();"),
                                ),
                            ]))]),
                        ),
                    ])),
                    Value::Dict(Dict::from(vec![
                        ("filename".to_owned(), Value::from("src/main.rs")),
                        (
                            "fullname".to_owned(),
                            Value::from("/tmp/hello_world/src/main.rs"),
                        ),
                        (
                            "symbols".to_owned(),
                            Value::List(vec![Value::Dict(Dict::from(vec![
                                ("line".to_owned(), Value::from("15")),
                                ("name".to_owned(), Value::from("hello_world::main")),
                                ("type".to_owned(), Value::from("fn ()")),
                                (
                                    "description".to_owned(),
                                    Value::from("static fn hello_world::main();"),
                                ),
                            ]))]),
                        ),
                    ])),
                ]),
            )])
            .into(),
        )]);

        let parsed = from_symbol_info_functions_payload(payload).expect("payload should parse");
        let symbols = parsed
            .get(&Utf8PathBuf::from("/tmp/hello_world/src/main.rs"))
            .expect("symbols should be keyed by fullname");

        assert_eq!(2, symbols.len());
        assert_eq!("hello_world::HelloMsg::say", symbols[0].name);
        assert_eq!("hello_world::main", symbols[1].name);
    }
}

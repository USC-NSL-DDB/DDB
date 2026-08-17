//! Stable well-known Protobuf types used by the public API.

use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};

pub use pbjson_types::Duration;
pub use prost_wkt_types::Timestamp;

/// Protobuf `google.protobuf.FieldMask` with its canonical ProtoJSON mapping.
///
/// `pbjson-types` 0.9 serializes this well-known type as a regular object. The
/// Protobuf JSON specification instead requires one comma-separated string with
/// each path converted between snake_case and lowerCamelCase. Keeping the
/// implementation here makes HTTP and gRPC use the same binary DTO without
/// special-casing individual routes.
#[derive(Clone, PartialEq, Eq, Hash, prost::Message)]
pub struct FieldMask {
    /// The set of field paths in their canonical Protobuf snake_case form.
    #[prost(string, repeated, tag = "1")]
    pub paths: Vec<String>,
}

impl Serialize for FieldMask {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut encoded = String::new();
        for (index, path) in self.paths.iter().enumerate() {
            if !is_valid_full_name(path) {
                return Err(S::Error::custom(format!(
                    "google.protobuf.FieldMask contains invalid path {path:?}"
                )));
            }
            let camel_case = json_camel_case(path);
            if json_snake_case(&camel_case) != *path {
                return Err(S::Error::custom(format!(
                    "google.protobuf.FieldMask contains irreversible path {path:?}"
                )));
            }
            if index != 0 {
                encoded.push(',');
            }
            encoded.push_str(&camel_case);
        }
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for FieldMask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let encoded = encoded.trim();
        if encoded.is_empty() {
            return Ok(Self::default());
        }

        let mut paths = Vec::new();
        for path in encoded.split(',') {
            let snake_case = json_snake_case(path);
            if path.contains('_') || !is_valid_full_name(&snake_case) {
                return Err(D::Error::custom(format!(
                    "google.protobuf.FieldMask contains invalid JSON path {path:?}"
                )));
            }
            paths.push(snake_case);
        }
        Ok(Self { paths })
    }
}

fn json_camel_case(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let mut was_underscore = false;
    for mut byte in value.bytes() {
        if byte != b'_' {
            if was_underscore && byte.is_ascii_lowercase() {
                byte = byte.to_ascii_uppercase();
            }
            output.push(byte);
        }
        was_underscore = byte == b'_';
    }
    String::from_utf8(output).expect("validated Protobuf identifiers are ASCII")
}

fn json_snake_case(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    for mut byte in value.bytes() {
        if byte.is_ascii_uppercase() {
            output.push(b'_');
            byte = byte.to_ascii_lowercase();
        }
        output.push(byte);
    }
    String::from_utf8(output).expect("ASCII case conversion preserves UTF-8")
}

fn is_valid_full_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            let mut bytes = part.bytes();
            matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic() || first == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_matches_protobuf_json_rules() {
        assert_eq!(json_camel_case("foo_bar.baz_qux"), "fooBar.bazQux");
        assert_eq!(json_snake_case("fooBar.bazQux"), "foo_bar.baz_qux");
        assert!(is_valid_full_name("foo_bar.baz"));
        assert!(!is_valid_full_name("foo..bar"));
        assert!(!is_valid_full_name("1foo"));
    }
}

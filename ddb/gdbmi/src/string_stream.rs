pub(crate) struct StringStream {
    raw_text: String,
    index: usize,
}

impl StringStream {
    pub(crate) fn new(raw_text: String) -> Self {
        Self { raw_text, index: 0 }
    }

    /// Read count characters starting at self.index, and return those characters as a string
    pub(crate) fn read(&mut self, count: usize) -> &str {
        let new_index = self.index + count;
        let buf = if new_index > self.raw_text.len() {
            &self.raw_text[self.index..]
        } else {
            &self.raw_text[self.index..new_index]
        };
        self.index = new_index;
        buf
    }

    pub(crate) fn seek_back(&mut self, offset: usize) {
        self.index = self.index.saturating_sub(offset);
    }

    /// Advance the index past specific chars
    ///
    /// Return substring that was advanced past
    pub(crate) fn advance_past_chars(&mut self, chars: &[char]) -> &str {
        let start_index = self.index;
        loop {
            let current_char = self.raw_text.as_bytes()[self.index] as char;
            self.index += 1;
            if chars.contains(&current_char) || self.index == self.raw_text.len() {
                break;
            }
        }
        &self.raw_text[start_index..self.index - 1]
    }

    /// Decode an MI C string as bytes first so both literal UTF-8 and GDB's
    /// octal-escaped UTF-8 produce the same string. The opening quote was read
    /// by the caller. Invalid or truncated input must not panic or invent paths.
    pub(crate) fn advance_past_string_with_gdb_escapes(
        &mut self,
    ) -> Result<String, crate::parser::Error> {
        use crate::parser::Error::InvalidCString;
        use std::convert::TryFrom;

        let bytes = self.raw_text.as_bytes();
        let mut buf = Vec::new();
        while let Some(&byte) = bytes.get(self.index) {
            self.index += 1;
            match byte {
                b'\\' => {
                    let &escaped = bytes
                        .get(self.index)
                        .ok_or(InvalidCString("trailing backslash"))?;
                    self.index += 1;
                    let decoded = match escaped {
                        b'a' => 7,
                        b'b' => 8,
                        b'e' => 27,
                        b'f' => 12,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'v' => 11,
                        b'\\' | b'"' | b'\'' | b'?' => escaped,
                        b'0'..=b'7' => {
                            let mut value = u16::from(escaped - b'0');
                            for _ in 0..2 {
                                let Some(&digit @ b'0'..=b'7') = bytes.get(self.index) else {
                                    break;
                                };
                                value = value * 8 + u16::from(digit - b'0');
                                self.index += 1;
                            }
                            u8::try_from(value)
                                .map_err(|_| InvalidCString("octal escape exceeds one byte"))?
                        }
                        b'x' => {
                            let start = self.index;
                            let mut value = 0u8;
                            while let Some(&digit) = bytes.get(self.index) {
                                let digit = match digit {
                                    b'0'..=b'9' => digit - b'0',
                                    b'a'..=b'f' => digit - b'a' + 10,
                                    b'A'..=b'F' => digit - b'A' + 10,
                                    _ => break,
                                };
                                value = value
                                    .checked_mul(16)
                                    .and_then(|value| value.checked_add(digit))
                                    .ok_or(InvalidCString("hex escape exceeds one byte"))?;
                                self.index += 1;
                            }
                            if self.index == start {
                                return Err(InvalidCString("empty hex escape"));
                            }
                            value
                        }
                        _ => return Err(InvalidCString("unknown escape")),
                    };
                    buf.push(decoded);
                }
                b'"' => {
                    return String::from_utf8(buf)
                        .map_err(|_| InvalidCString("decoded bytes are not UTF-8"))
                }
                byte => buf.push(byte),
            }
        }
        Err(InvalidCString("missing closing quote"))
    }
}

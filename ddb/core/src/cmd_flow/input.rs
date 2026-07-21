use super::{router::Target, session_runtime::CompletionConsistency};
use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone)]
pub struct Command {
    pub external_token: Option<u64>,
    pub internal_token: u64,
    pub raw_cmd: String,
    pub consistency: CompletionConsistency,
}

impl Command {
    pub fn new(external_token: Option<u64>, internal_token: u64, raw_cmd: String) -> Self {
        Self {
            external_token,
            internal_token,
            raw_cmd,
            consistency: CompletionConsistency::StateConsistent,
        }
    }

    pub fn with_consistency(mut self, consistency: CompletionConsistency) -> Self {
        self.consistency = consistency;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ParsedInputCmd {
    pub external_token: Option<u64>,
    pub prefix: String,
    pub args: String,
    pub target: Target,
}

impl ParsedInputCmd {
    #[inline]
    pub fn full_cmd(&self) -> String {
        format!("{} {}", self.prefix, self.args).trim().to_string()
    }

    #[inline]
    pub fn with_target(self, target: Target) -> Self {
        ParsedInputCmd { target, ..self }
    }

    #[inline]
    pub fn with_default_target(self, target: Target) -> Self {
        if matches!(self.target, Target::Unspecified) {
            self.with_target(target)
        } else {
            self
        }
    }

    #[inline]
    pub fn with_prefix(self, prefix: &str) -> Self {
        ParsedInputCmd {
            prefix: prefix.to_string(),
            ..self
        }
    }

    #[inline]
    pub fn to_command(self, internal_token: u64) -> (Target, Command) {
        let target = self.target.clone();
        (
            target,
            Command::new(self.external_token, internal_token, self.full_cmd()),
        )
    }
}

pub struct InputCmdParser(String);

impl InputCmdParser {
    /// Extracts an optional external MI token without allocating runtime state.
    /// Correlation tokens are assigned later by the execution boundary.
    #[inline]
    pub fn prepare_token(&self) -> Result<(Option<u64>, String)> {
        let command = self.0.trim();
        if let Some(index) = command.find('-') {
            let (token, rest) = command.split_at(index);

            // Ensure the token is numeric
            let ext_token: Option<u64> = {
                if !token.chars().all(|c| c.is_ascii_digit()) {
                    bail!("Invalid token: {}", token);
                }
                (!token.is_empty())
                    .then(|| token.parse::<u64>().ok())
                    .flatten()
            };

            let raw_cmd = rest.trim().to_string();
            return Ok((ext_token, raw_cmd));
        }
        bail!("Invalid command: {}", command);
    }

    // Read in a command string which is expected to be already stripped out of token.
    // It does 3 things:
    // - 1. Extracts out the prefix, which is expected to be the command type, e.g. `-thread-select`.
    // - 2. Based on the command, it determines the routing target. `--all` can be recognized by DDB.
    // - 3. Extracts out the rest of the arguments, stripping/swaping out the custom args that gdb cannot recognize.
    // Note: The routing target can be overwritten by the selected operation.
    // Returns:
    //   - Target, Command Prefix, Rest of the Command (stripped/swapped out of custom args)
    #[inline]
    fn prepare_cmd(&self, raw_cmd: String) -> Result<(Target, String, String)> {
        let parts = raw_cmd.splitn(2, char::is_whitespace).collect::<Vec<_>>();
        let prefix = {
            let _prefix = *parts.first().ok_or(anyhow!(
                "command prefix is missing for command: {}",
                raw_cmd
            ))?;
            if _prefix.is_empty() {
                bail!("Empty command prefix");
            }
            _prefix.to_string()
        };

        if parts.len() == 1 {
            // no arguments following the command prefix
            return Ok((Target::default(), prefix, "".to_string()));
        }

        let rest = parts[1].split_whitespace().collect::<Vec<_>>();
        if rest.last().is_some_and(|s| *s == "--all") {
            // --all for broadcast
            return Ok((Target::Broadcast, prefix, rest[..rest.len() - 1].join(" ")));
        }

        if let Some(index) = rest.iter().position(|s| *s == "--thread") {
            // --thread for targeting a specific thread
            if index < rest.len() - 1 {
                let gtid = rest[index + 1].parse::<u64>().context(format!(
                    "Invalid gtid provided for --thread flag. Command: {}",
                    raw_cmd
                ))?;
                let target = Target::Thread(gtid);
                return Ok((target, prefix, rest.join(" ")));
            }
        }

        if let Some(index) = rest.iter().position(|s| *s == "--session") {
            // --session for targeting a specific session
            if index < rest.len() - 1 {
                let sid = rest[index + 1].parse::<u64>().context(format!(
                    "invalid sid when use --session flag. Command: {}",
                    raw_cmd
                ))?;
                let target = Target::Session(sid);
                let mut rest = rest.clone();
                // remove the `--session` and its argument from the rest.
                // underlying debugger doesn't have session concept.
                rest.remove(index);
                rest.remove(index); // the next element is shifted after the first remove
                return Ok((target, prefix, rest.join(" ").trim().to_string()));
            }
        }

        if let Some(index) = rest.iter().position(|s| *s == "--group") {
            // --group for targeting a specific group
            if index < rest.len() - 1 {
                let gid = rest[index + 1].parse::<u64>().context(format!(
                    "invalid gid when use --group flag. Command: {}",
                    raw_cmd
                ))?;
                let target = Target::Group(gid);
                let mut rest = rest.clone();
                // remove the `--group` and its argument from the rest.
                // underlying debugger doesn't have group concept.
                rest.remove(index);
                rest.remove(index); // the next element is shifted after the first remove
                return Ok((target, prefix, rest.join(" ").trim().to_string()));
            }
        }

        if let Some(index) = rest.iter().position(|s| *s == "--multiple") {
            // --multiple for multiple targets (mixed of sessions and groups, no threads yet)
            // Syntax example: --multiple g1,g2,s1,s2
            // where s=session, g=group, comma separated, but no spaces
            if index < rest.len() - 1 {
                let mut target_list = Vec::new();
                for ele in rest[index + 1].split(',') {
                    if ele.starts_with('s') {
                        let sid = ele[1..].parse::<u64>().context(format!(
                            "invalid sid when use --multiple flag. Command: {}",
                            raw_cmd
                        ))?;
                        target_list.push(Target::Session(sid));
                    } else if ele.starts_with('g') {
                        let gid = ele[1..].parse::<u64>().context(format!(
                            "invalid gid when use --multiple flag. Command: {}",
                            raw_cmd
                        ))?;
                        target_list.push(Target::Group(gid));
                    } else {
                        bail!(
                            "Invalid target {} in --multiple flag. Command: {}",
                            ele,
                            raw_cmd
                        );
                    }
                }
                let target = Target::Multiple(target_list);
                let mut rest = rest.clone();
                // remove the `--group` and its argument from the rest.
                // underlying debugger doesn't have group concept.
                rest.remove(index);
                rest.remove(index); // the next element is shifted after the first remove
                return Ok((target, prefix, rest.join(" ").trim().to_string()));
            }
        }

        Ok((Target::default(), prefix, rest.join(" ")))
    }

    #[inline]
    pub fn parse(&self) -> Result<ParsedInputCmd> {
        let (ext_token, raw_cmd) = self.prepare_token()?;
        let (target, prefix, args) = self.prepare_cmd(raw_cmd)?;
        // println!("target: {:?}, prefix: {:?}, args: {:?}", target, prefix, args);
        Ok(ParsedInputCmd {
            external_token: ext_token,
            prefix,
            args,
            target,
        })
    }
}

impl TryInto<ParsedInputCmd> for &str {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<ParsedInputCmd> {
        Ok(InputCmdParser(self.to_string()).parse()?)
    }
}

impl TryInto<ParsedInputCmd> for String {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<ParsedInputCmd> {
        Ok(InputCmdParser(self).parse()?)
    }
}

impl TryInto<ParsedInputCmd> for InputCmdParser {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<ParsedInputCmd> {
        self.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, ParsedInputCmd};

    #[test]
    fn parses_target_prefix_and_external_token_without_execution_state() {
        let parsed: ParsedInputCmd = "123-exec-interrupt --session 1".try_into().unwrap();

        assert_eq!(parsed.external_token, Some(123));
        assert_eq!(parsed.prefix, "-exec-interrupt");
        assert_eq!(parsed.args, "");
        assert_eq!(parsed.target, super::Target::Session(1));
    }

    #[test]
    fn parses_command_arguments() {
        let parsed: ParsedInputCmd = "567-switch-context reg1=1 reg2=2".try_into().unwrap();

        assert_eq!(parsed.external_token, Some(567));
        assert_eq!(parsed.prefix, "-switch-context");
        assert_eq!(parsed.args, "reg1=1 reg2=2");
    }

    #[test]
    fn parses_quoted_variable_commands() {
        let parsed: ParsedInputCmd =
            r#"-var-create --frame 1 var_1008_epfd @ "epfd""#.try_into().unwrap();

        assert_eq!(parsed.prefix, "-var-create");
    }

    #[test]
    fn parsing_is_deterministic_and_does_not_allocate_execution_tokens() {
        let first: ParsedInputCmd = "-thread-info".try_into().unwrap();
        let second: ParsedInputCmd = "-thread-info".try_into().unwrap();

        assert_eq!(first.external_token, second.external_token);
        assert_eq!(first.prefix, second.prefix);
        assert_eq!(first.args, second.args);
        assert_eq!(first.target, second.target);
    }

    #[test]
    fn command_conversion_accepts_the_boundary_allocated_token() {
        let parsed: ParsedInputCmd = "777-exec-continue".try_into().unwrap();
        let (target, command) = parsed.to_command(200);

        assert_eq!(command.external_token, Some(777));
        assert_eq!(command.internal_token, 200);
        assert_eq!(command.raw_cmd, "-exec-continue");
        assert_eq!(
            command.consistency,
            crate::cmd_flow::session_runtime::CompletionConsistency::StateConsistent
        );
        assert!(matches!(target, super::Target::Unspecified));
    }

    #[test]
    fn thread_target_parsing_does_not_require_live_state() {
        let parsed: ParsedInputCmd = "-stack-list-frames --thread 9001".try_into().unwrap();
        assert_eq!(parsed.target, super::Target::Thread(9001));
        assert_eq!(parsed.args, "--thread 9001");
    }

    #[test]
    fn command_completion_consistency_is_explicit() {
        let command = Command::new(None, 200, "-thread-info".to_string()).with_consistency(
            crate::cmd_flow::session_runtime::CompletionConsistency::ProtocolComplete,
        );

        assert_eq!(
            command.consistency,
            crate::cmd_flow::session_runtime::CompletionConsistency::ProtocolComplete
        );
    }
}

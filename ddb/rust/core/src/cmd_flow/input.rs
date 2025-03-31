use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use dashmap::DashMap;
use tracing::{debug, error};

use super::{
    framework_adapter::FrameworkCommandAdapter,
    handler::*,
    router::{Router, Target},
    DynFormatter, OutgoingCmd, OutputSource,
};
use crate::{cmd_flow::get_router, handlers_map, state::STATES};

#[derive(Debug, Clone)]
pub struct Command<F: DynFormatter + 'static> {
    pub external_token: Option<u64>,
    pub internal_token: u64,
    pub raw_cmd: String,
    pub formatter: F,
}

impl<F> Command<F>
where
    F: DynFormatter + 'static,
{
    /// Prepares the command to be sent by creating an OutgoingCmd and formatting the raw command string
    /// This function consumes `self`.
    ///
    /// # Arguments
    ///
    /// * `num_target` - The number of target recipients
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// * `OutgoingCmd` - The prepared outgoing command with tokens and formatting
    /// * `String` - The formatted command string with internal token prepended
    #[inline]
    pub fn prepare_to_send(self, num_target: u32, out_src: OutputSource) -> (OutgoingCmd, String) {
        let cmd = self.internal_cmd();
        (
            OutgoingCmd::new(
                self.internal_token,
                self.external_token,
                cmd.clone(),
                num_target,
                out_src,
                self.formatter,
            ),
            cmd,
        )
    }

    pub fn new(
        external_token: Option<u64>,
        internal_token: u64,
        raw_cmd: String,
        formatter: F,
    ) -> Self {
        Command {
            external_token,
            internal_token,
            raw_cmd,
            formatter,
        }
    }

    pub fn internal_cmd(&self) -> String {
        let cmd = format!("{}{}", self.internal_token, self.raw_cmd);
        if cmd.ends_with('\n') {
            cmd
        } else {
            format!("{}\n", cmd)
        }
    }

    pub fn external_cmd(&self) -> String {
        let cmd = format!(
            "{}{}",
            self.external_token
                .map(|t| t.to_string())
                .unwrap_or("".to_string()),
            self.raw_cmd
        );
        if cmd.ends_with('\n') {
            cmd
        } else {
            format!("{}\n", cmd)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedInputCmd {
    pub external_token: Option<u64>,
    pub internal_token: u64,
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
    pub fn to_command<F: DynFormatter + 'static>(self, formatter: F) -> (Target, Command<F>) {
        let raw_cmd = self.full_cmd();
        (
            self.target,
            Command::new(self.external_token, self.internal_token, raw_cmd, formatter),
        )
    }
}

pub struct InputCmdParser(String);

impl InputCmdParser {
    // outputs (external_token, internal_token, raw_cmd)
    #[inline]
    pub fn prepare_token(&self) -> Result<(Option<u64>, u64, String)> {
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

            let internal_token = crate::common::counter::next_token();
            let raw_cmd = rest.trim().to_string();
            return Ok((ext_token, internal_token, raw_cmd));
        }
        bail!("Invalid command: {}", command);
    }

    // Read in a command string which is expected to be already stripped out of token.
    // It does 3 things:
    // - 1. Extracts out the prefix, which is expected to be the command type, e.g. `-thread-select`.
    // - 2. Based on the command, it determines the routing target. `--all` can be recognized by DDB.
    // - 3. Extracts out the rest of the arguments, stripping/swaping out the custom args that gdb cannot recognize.
    // Note: The routing target can be overwritten by the specific handler.
    // Returns:
    //   - Target, Command Prefix, Rest of the Command (stripped/swapped out of custom args)
    #[inline]
    fn prepare_cmd(&self, raw_cmd: String) -> (Target, String, String) {
        let parts = raw_cmd.splitn(2, char::is_whitespace).collect::<Vec<_>>();
        let prefix = {
            let _prefix = *parts.first().expect("command prefix is present");
            if _prefix.is_empty() {
                panic!("Empty command prefix");
            }
            _prefix.to_string()
        };

        if parts.len() == 1 {
            // no arguments following the command prefix
            return (Target::default(), prefix, "".to_string());
        }

        let rest = parts[1].split_whitespace().collect::<Vec<_>>();
        // println!("rest: {:?}", rest);
        if rest.last().is_some_and(|s| *s == "--all") {
            // --all for broadcast
            return (Target::Broadcast, prefix, rest[..rest.len() - 1].join(" "));
        }
        
        if let Some(index) = rest.iter().position(|s| *s == "--thread") {
            // --thread for targeting a specific thread
            if index < rest.len() - 1 {
                let gtid = rest[index + 1]
                    .parse::<u64>()
                    .expect("valid gtid when use --thread flag");
                let (_, tid) = STATES.get_ltid_by_gtid(gtid).unwrap().into();
                let target = Target::Thread(gtid);
                return (
                    target,
                    prefix,
                    format!(
                        "{} {} {}",
                        rest[..=index].join(" "),
                        tid,
                        rest[index + 2..].join(" ")
                    )
                    .trim()
                    .to_string(),
                );
            }
        }

        if let Some(index) = rest.iter().position(|s| *s == "--session") {
            // --session for targeting a specific session
            if index < rest.len() - 1 {
                let sid = rest[index + 1]
                    .parse::<u64>()
                    .expect("valid sid when use --session flag");
                let target = Target::Session(sid);
                let mut rest = rest.clone();
                rest.remove(index);
                rest.remove(index); // the next element is shifted after the first remove
                return (target, prefix, rest.join(" ").trim().to_string());
            }
        }

        if let Some(gtid) = STATES.get_curr_gtid() {
            // if there is a current global thread selected, use it as the target
            return (Target::Thread(gtid), prefix, rest.join(" "));
        }

        (Target::default(), prefix, rest.join(" "))
    }

    #[inline]
    pub fn parse(&self) -> Result<ParsedInputCmd> {
        let (ext_token, internal_token, raw_cmd) = self.prepare_token()?;
        let (target, prefix, args) = self.prepare_cmd(raw_cmd);
        // println!("target: {:?}, prefix: {:?}, args: {:?}", target, prefix, args);
        Ok(ParsedInputCmd {
            external_token: ext_token,
            internal_token,
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

pub struct CmdHandler {
    tx: flume::Sender<String>,
    rx: flume::Receiver<String>,
    handlers: DashMap<String, Box<dyn Handler>>,
    default_handler: DefaultHandler,
    adapter: Arc<dyn FrameworkCommandAdapter>,
    worker_join_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl CmdHandler {
    pub fn new(router: Arc<Router>, adapter: Arc<dyn FrameworkCommandAdapter>) -> Arc<Self> {
        let (tx, rx) = flume::unbounded::<String>();

        let handlers = handlers_map! {
            "-break-insert" => BreakInsertHandler::new(router.clone()),
            "-thread-info" => ThreadInfoHandler::new(router.clone()),
            "-exec-continue" => ContinueHandler::new(router.clone()),
            "-exec-interrupt" => InterruptHandler::new(router.clone()),
            "-file-list-lines" => ListHandler::new(router.clone()),
            "-thread-select" => ThreadSelectHandler::new(router.clone()),
            "-bt-remote" => DistributeBacktraceHandler::new(router.clone(), adapter.clone()),
            "-list-thread-groups" => ListGroupsHandler::new(router.clone()),
            "-exec-next" => ExecNextHandler::new(router.clone()),
            "-exec-step" => ExecStepHandler::new(router.clone()),
            "-exec-finish" => ExecFinishHandler::new(router.clone()),
        };

        Arc::new(CmdHandler {
            tx,
            rx,
            handlers,
            default_handler: DefaultHandler::new(router.clone()),
            adapter,
            worker_join_handles: Mutex::new(Vec::new()),
        })
    }

    #[inline]
    pub async fn input(&self, cmd: &str) {
        if let Err(e) = self.tx.send_async(cmd.to_string()).await {
            error!("Failed to send input command: {}", e);
        }
    }

    #[allow(unused)]
    pub fn register_handler<H: Handler + 'static>(&self, prefix: &str, handler: H) {
        self.handlers.insert(prefix.to_string(), Box::new(handler));
    }

    pub fn start(self: Arc<Self>, num_workers: u32) {
        let handles: Vec<_> = (0..num_workers)
            .map(|_| {
                let handler = Arc::clone(&self);
                tokio::spawn(async move {
                    handler.worker().await;
                })
            })
            .collect();
        *self.worker_join_handles.lock().unwrap() = handles;
    }

    async fn worker(&self) {
        loop {
            match self.rx.recv_async().await {
                Ok(cmd) => {
                    debug!("received cmd: {}", cmd);
                    self.handle_cmd(cmd).await;
                }
                Err(e) => {
                    error!("Failed to receive input command: {}", e);
                }
            }
        }
    }

    #[inline]
    // handle a raw command string, directly from user inputs
    pub async fn handle_cmd(&self, cmd: String) {
        let cmd = cmd.trim();
        if cmd.starts_with(&":") {
            get_router().handle_internal_cmd(&cmd[1..]);
            return
        }

        let parsed: Result<ParsedInputCmd> = cmd.try_into();
        match parsed {
            Ok(parsed) => {
                let handler = self.handlers.get(parsed.prefix.as_str());
                match handler {
                    Some(handler) => {
                        handler.process_cmd(parsed).await;
                    }
                    None => {
                        self.default_handler.process_cmd(parsed).await;
                    }
                }
            }
            Err(e) => {
                error!("Failed to parse input command: {}", e);
            }
        }
    }

    pub fn stop(&self) {
        for handle in self.worker_join_handles.lock().unwrap().iter() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ParsedInputCmd;

    #[test]
    pub fn test_simple_input_parser() {
        let intr_cmd: ParsedInputCmd = "-exec-interrupt --session 1".try_into().unwrap();
        assert_eq!(intr_cmd.prefix, "-exec-interrupt");
        assert_eq!(intr_cmd.args, "");
        assert_eq!(intr_cmd.target, super::Target::Session(1));
        assert_eq!(intr_cmd.external_token, None);
        // internal token is generated incrementally starting from 1
        assert_ne!(intr_cmd.internal_token, 0);
    }

    #[test]
    pub fn test_input_parser_token() {
        let cmd: ParsedInputCmd = "123-exec-interrupt".try_into().unwrap();
        assert_eq!(cmd.external_token, Some(123));
        assert_ne!(cmd.internal_token, 0);
        assert_eq!(cmd.args, "");
        assert_eq!(cmd.prefix, "-exec-interrupt");
        assert_eq!(cmd.target, super::Target::default());
    }

    #[test]
    pub fn test_input_parser_args() {
        let cmd: ParsedInputCmd = "567-switch-context reg1=1 reg2=2".try_into().unwrap();
        assert_eq!(cmd.external_token, Some(567));
        assert_ne!(cmd.internal_token, 0);
        assert_eq!(cmd.prefix, "-switch-context");
        assert_eq!(cmd.args, "reg1=1 reg2=2");
    }

    #[test]
    pub fn test_input_parser_var() {
        let cmd: ParsedInputCmd = r#"-var-create --frame 1 var_1008_epfd @ "epfd""#.try_into().unwrap();
        println!("cmd: {:?}", cmd);
        // assert_eq!(cmd.external_token, Some(567));
        // assert_ne!(cmd.internal_token, 0);
        // assert_eq!(cmd.prefix, "-switch-context");
        // assert_eq!(cmd.args, "reg1=1 reg2=2");
    }
}

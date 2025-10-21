pub mod gdb_cmd;

pub use gdb_cmd::*;

pub trait DbgCmdGenerator {
    fn generate(&self) -> String;
}

pub struct DbgCmdListBuilder<T>
where
    T: DbgCmdGenerator,
{
    cmds: Vec<T>,
}

impl<T> DbgCmdListBuilder<T>
where
    T: DbgCmdGenerator,
{
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }

    pub fn add<U: Into<T>>(&mut self, cmd: U) -> &Self {
        self.cmds.push(cmd.into());
        self
    }

    pub fn build(self) -> Vec<String> {
        self.cmds.iter().map(|cmd| cmd.generate()).collect()
    }
}

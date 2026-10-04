//! Frontend source identities. Local files are never represented as DDB handles.

use anyhow::{Context, Result};
use ddb_api_client::v2;
use std::borrow::Cow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalRoot {
    pub name: String,
    pub debugger_path: Option<String>,
    pub session_tags: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceId {
    Ddb { instance: String, reference: String },
    Local { root: String, path: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Root {
    Ddb {
        instance: String,
        root: v2::SourceRoot,
    },
    Local(LocalRoot),
}

impl Root {
    pub fn name(&self) -> &str {
        match self {
            Self::Ddb { root, .. } => &root.name,
            Self::Local(root) => &root.name,
        }
    }
    pub fn id(&self) -> SourceId {
        match self {
            Self::Ddb { instance, root } => SourceId::Ddb {
                instance: instance.clone(),
                reference: root.root_reference.clone(),
            },
            Self::Local(root) => SourceId::Local {
                root: root.name.clone(),
                path: String::new(),
            },
        }
    }
    pub fn host(&self) -> &'static str {
        if matches!(self, Self::Local(_)) {
            "Local"
        } else {
            "DDB"
        }
    }
    pub fn owners<'a>(&'a self, sessions: &'a [v2::Session]) -> Cow<'a, [v2::SourceOwner]> {
        match self {
            Self::Ddb { root, .. } => Cow::Borrowed(&root.owners),
            Self::Local(root) => tagged_owners(&root.session_tags, sessions),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalSource {
    pub root: LocalRoot,
    pub path: String,
    /// Explicit frame/catalog mapping of an otherwise unbound local root.
    pub owner_hint: Vec<v2::SourceOwner>,
    pub owner_instance: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Source {
    Ddb {
        instance: String,
        file: v2::SourceFile,
    },
    Local(LocalSource),
}

impl Source {
    pub fn id(&self) -> SourceId {
        match self {
            Self::Ddb { instance, file } => SourceId::Ddb {
                instance: instance.clone(),
                reference: file.source_reference.clone(),
            },
            Self::Local(source) => SourceId::Local {
                root: source.root.name.clone(),
                path: source.path.clone(),
            },
        }
    }
    pub fn path(&self) -> Cow<'_, str> {
        match self {
            Self::Ddb { file, .. } => Cow::Borrowed(file.path.as_deref().unwrap_or(&file.name)),
            Self::Local(source) => Cow::Owned(format!("{}/{}", source.root.name, source.path)),
        }
    }
    pub fn debugger_path(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Ddb { file, .. } => file
                .debugger_path
                .as_deref()
                .filter(|path| !path.is_empty())
                .map(Cow::Borrowed),
            Self::Local(source) => source.root.debugger_path.as_ref().map(|prefix| {
                Cow::Owned(format!("{}/{}", prefix.trim_end_matches('/'), source.path))
            }),
        }
    }
    pub fn owners<'a>(&'a self, sessions: &'a [v2::Session]) -> Cow<'a, [v2::SourceOwner]> {
        match self {
            Self::Ddb { file, .. } => Cow::Borrowed(&file.owners),
            Self::Local(source) if source.root.session_tags.is_empty() => {
                Cow::Borrowed(&source.owner_hint)
            }
            Self::Local(source) => tagged_owners(&source.root.session_tags, sessions),
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
            Self::Ddb { file, .. } if file.origin == v2::SourceOrigin::Debugger as i32 => "debug",
            Self::Ddb { .. } => "file",
        }
    }
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Ddb { file, .. } => file.unavailable_reason.as_deref(),
            Self::Local(_) => None,
        }
    }
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
    pub fn invalidate_server(&mut self) {
        if let Self::Local(source) = self {
            source.owner_hint.clear();
            source.owner_instance = None;
        }
    }
}

fn tagged_owners<'a>(tags: &[String], sessions: &'a [v2::Session]) -> Cow<'a, [v2::SourceOwner]> {
    Cow::Owned(
        sessions
            .iter()
            .filter(|session| tags.contains(&session.tag))
            .map(|session| v2::SourceOwner {
                session_id: session.session_id.clone(),
                group_id: session.group_id.clone(),
            })
            .collect(),
    )
}

#[derive(Clone, Debug)]
pub enum Address {
    Resolve {
        instance: Option<String>,
        target: Option<v2::Target>,
        location: v2::SourceLocation,
    },
    Export {
        root: Root,
        path: String,
    },
    File(Source),
}

impl Address {
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            Self::File(Source::Local(_))
                | Self::Export {
                    root: Root::Local(_),
                    ..
                }
        )
    }
    pub fn id(&self) -> Option<SourceId> {
        match self {
            Self::File(source) => Some(source.id()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Content {
    pub source: Source,
    pub start_line: u32,
    pub content: String,
    pub line_count: u32,
    pub total_lines: u32,
}

impl Content {
    pub fn from_ddb(instance: String, content: v2::SourceContent) -> Result<Self> {
        let file = content
            .source
            .context("ReadSource omitted source identity")?;
        anyhow::ensure!(
            !instance.is_empty() && !file.source_reference.is_empty(),
            "DDB omitted a source identity or server instance"
        );
        Ok(Self {
            source: Source::Ddb { instance, file },
            start_line: content.start_line,
            content: content.content,
            line_count: content.line_count,
            total_lines: content.total_lines,
        })
    }
}

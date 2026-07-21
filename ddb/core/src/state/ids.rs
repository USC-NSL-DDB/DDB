use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

macro_rules! domain_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[repr(transparent)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            #[inline]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            #[inline]
            pub const fn value(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            #[inline]
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            #[inline]
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = std::num::ParseIntError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse::<u64>().map(Self)
            }
        }
    };
}

domain_id!(GroupId);
domain_id!(GlobalThreadId);
domain_id!(GlobalThreadGroupId);

/// Identity of the discovered service a session belongs to.
///
/// This is the domain's own record of service membership; boundary layers
/// (discovery, static configuration) construct it when admitting a session so
/// the state model never depends on transport message shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceIdentity {
    pub hash: String,
    pub alias: String,
}

impl ServiceIdentity {
    pub fn new(hash: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            alias: alias.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_ids_preserve_numeric_wire_representation() {
        assert_eq!(serde_json::to_string(&GroupId::new(7)).unwrap(), "7");
        assert_eq!(
            serde_json::from_str::<GroupId>("9").unwrap(),
            GroupId::new(9)
        );
    }

    #[test]
    fn distinct_id_types_require_explicit_boundary_conversion() {
        let thread = GlobalThreadId::new(11);
        let raw: u64 = thread.into();
        let group = GroupId::from(raw);

        assert_eq!(thread.value(), 11);
        assert_eq!(group.value(), 11);
    }
}

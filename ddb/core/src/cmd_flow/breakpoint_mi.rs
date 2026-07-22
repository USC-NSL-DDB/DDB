//! MI wire rendering for breakpoint records.
//!
//! Consumes the domain's published [`BreakpointSnapshot`] so the wire mapping
//! never reaches into aggregate internals.

use std::collections::HashMap;

use gdbmi::raw::{Dict, Value};

use crate::state::{BreakpointSnapshot, SubBreakpointSnapshot};

pub(crate) fn bkpt_payload(snapshot: &BreakpointSnapshot) -> Dict {
    let subbkpts: Vec<Value> = snapshot
        .subbkpts
        .iter()
        .map(|sub_breakpoint| {
            let (id, kind, target_id) = match sub_breakpoint {
                SubBreakpointSnapshot::Group {
                    id, target_group, ..
                } => (*id, "group", *target_group),
                SubBreakpointSnapshot::Session {
                    id, target_session, ..
                } => (*id, "session", *target_session),
            };
            HashMap::from([
                ("id", id.to_string().into()),
                ("type", kind.to_string().into()),
                ("target_id", target_id.to_string().into()),
            ])
            .into()
        })
        .collect();

    let mut payload = HashMap::new();
    payload.insert(
        "bkpt",
        HashMap::from([
            ("id", snapshot.id.to_string().into()),
            (
                "enabled",
                if snapshot.enabled { "y" } else { "n" }.to_string().into(),
            ),
            ("fullname", snapshot.location.src.clone().into()),
            ("line", snapshot.location.line.to_string().into()),
        ])
        .into(),
    );
    payload.insert("subbkpt", subbkpts.into());
    Dict::from(payload)
}

pub(crate) fn bkpt_deleted_payload(breakpoint_id: u64) -> Dict {
    HashMap::from([(
        "bkpt",
        HashMap::from([("id", breakpoint_id.to_string().into())]).into(),
    )])
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BkptLoc, GroupId, GroupSubBkpt, RuntimeModel, SubBkptType};

    #[test]
    fn breakpoint_payload_preserves_the_established_wire_shape() {
        let model = RuntimeModel::new();
        let breakpoint_id = model.add_breakpoint(BkptLoc::new("src/worker.rs", 42));
        model.add_sub_breakpoint(
            breakpoint_id,
            SubBkptType::Group(GroupSubBkpt::new(GroupId::new(3))),
        );
        let snapshot = BreakpointSnapshot::from(&model.breakpoint(breakpoint_id).unwrap());

        let payload = bkpt_payload(&snapshot);

        let bkpt = payload["bkpt"].expect_dict_ref().unwrap();
        assert_eq!(
            bkpt["id"].expect_string_ref().unwrap(),
            breakpoint_id.to_string()
        );
        assert_eq!(bkpt["enabled"].expect_string_ref().unwrap(), "y");
        assert_eq!(
            bkpt["fullname"].expect_string_ref().unwrap(),
            "src/worker.rs"
        );
        assert_eq!(bkpt["line"].expect_string_ref().unwrap(), "42");
        let sub = payload["subbkpt"].expect_list_ref().unwrap()[0]
            .expect_dict_ref()
            .unwrap();
        assert_eq!(sub["type"].expect_string_ref().unwrap(), "group");
        assert_eq!(sub["target_id"].expect_string_ref().unwrap(), "3");
    }

    #[test]
    fn deleted_payload_carries_only_the_breakpoint_id() {
        let payload = bkpt_deleted_payload(9);
        let bkpt = payload["bkpt"].expect_dict_ref().unwrap();
        assert_eq!(bkpt["id"].expect_string_ref().unwrap(), "9");
    }
}

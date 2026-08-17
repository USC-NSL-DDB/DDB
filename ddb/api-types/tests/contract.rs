use std::collections::BTreeSet;

use ddb_api_types::{v2::*, wkt::FieldMask, V2_FILE_DESCRIPTOR_SET};
use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorSet};
use serde_json::json;

#[test]
fn binary_cursor_matches_golden_and_tolerates_unknown_fields() {
    let cursor = Cursor {
        server_instance_id: "srv_opaque/01".to_owned(),
        sequence: 9_007_199_254_740_993,
    };
    let golden = [
        0x0a, 0x0d, 0x73, 0x72, 0x76, 0x5f, 0x6f, 0x70, 0x61, 0x71, 0x75, 0x65, 0x2f, 0x30, 0x31,
        0x10, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x10,
    ];

    assert_eq!(cursor.encode_to_vec(), golden);
    assert_eq!(Cursor::decode(golden.as_slice()).unwrap(), cursor);

    let mut with_unknown_field = golden.to_vec();
    // Field 99, varint value 1. Additive binary fields must be tolerated.
    with_unknown_field.extend_from_slice(&[0x98, 0x06, 0x01]);
    assert_eq!(
        Cursor::decode(with_unknown_field.as_slice()).unwrap(),
        cursor
    );
}

#[test]
fn protojson_uses_canonical_large_integer_bytes_and_optional_mappings() {
    let cursor = Cursor {
        server_instance_id: "srv_opaque/01".to_owned(),
        sequence: 9_007_199_254_740_993,
    };
    assert_eq!(
        serde_json::to_value(&cursor).unwrap(),
        json!({
            "serverInstanceId": "srv_opaque/01",
            "sequence": "9007199254740993"
        })
    );

    let memory = MemoryBlock {
        address: "0xfeed".to_owned(),
        data: vec![0x00, 0x01, 0xfe],
        unreadable_bytes: 9_007_199_254_740_993,
    };
    assert_eq!(
        serde_json::to_value(&memory).unwrap(),
        json!({
            "address": "0xfeed",
            "data": "AAH+",
            "unreadableBytes": "9007199254740993"
        })
    );

    let absent = PageRequest {
        page_size: 0,
        page_token: None,
    };
    let present_empty = PageRequest {
        page_size: 0,
        page_token: Some(String::new()),
    };
    assert_eq!(serde_json::to_value(&absent).unwrap(), json!({}));
    assert_eq!(
        serde_json::to_value(&present_empty).unwrap(),
        json!({"pageToken": ""})
    );

    let absent_enabled = BreakpointSpec::default();
    let explicitly_disabled = BreakpointSpec {
        enabled: Some(false),
        ..Default::default()
    };
    assert_eq!(serde_json::to_value(&absent_enabled).unwrap(), json!({}));
    assert_eq!(
        serde_json::to_value(&explicitly_disabled).unwrap(),
        json!({"enabled": false})
    );
    assert!(absent_enabled.encode_to_vec().is_empty());
    assert_eq!(explicitly_disabled.encode_to_vec(), [0x20, 0x00]);
    assert_eq!(
        BreakpointSpec::decode([0x20, 0x00].as_slice())
            .unwrap()
            .enabled,
        Some(false)
    );
}

#[test]
fn protojson_maps_well_known_timestamps_and_ignores_additive_fields() {
    let context = RequestContext {
        client_request_id: Some("client:opaque".to_owned()),
        idempotency_key: None,
        deadline: Some(ddb_api_types::wkt::Timestamp {
            seconds: 1_704_067_200,
            nanos: 123_000_000,
        }),
    };
    assert_eq!(
        serde_json::to_value(&context).unwrap(),
        json!({
            "clientRequestId": "client:opaque",
            "deadline": "2024-01-01T00:00:00.123Z"
        })
    );

    let decoded: Cursor = serde_json::from_value(json!({
        "serverInstanceId": "srv:new",
        "sequence": "42",
        "futureField": {
            "nested": true
        }
    }))
    .unwrap();
    assert_eq!(
        decoded,
        Cursor {
            server_instance_id: "srv:new".to_owned(),
            sequence: 42,
        }
    );
}

#[test]
fn field_mask_uses_the_canonical_protojson_string_mapping() {
    let mask = FieldMask {
        paths: vec!["enabled".to_owned(), "some_field.nested_value".to_owned()],
    };
    assert_eq!(
        serde_json::to_value(&mask).unwrap(),
        json!("enabled,someField.nestedValue")
    );
    assert_eq!(
        serde_json::from_value::<FieldMask>(json!("enabled,condition")).unwrap(),
        FieldMask {
            paths: vec!["enabled".to_owned(), "condition".to_owned()]
        }
    );

    let request = UpdateBreakpointRequest {
        update_mask: Some(mask.clone()),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"updateMask": "enabled,someField.nestedValue"})
    );

    let binary = mask.encode_to_vec();
    assert_eq!(FieldMask::decode(binary.as_slice()).unwrap(), mask);
    assert!(serde_json::from_value::<FieldMask>(json!("some_field")).is_err());
    assert!(serde_json::to_value(&FieldMask {
        paths: vec!["irreversible_0".to_owned()]
    })
    .is_err());
}

#[test]
fn enum_compatibility_is_explicit_in_binary_and_protojson() {
    let error = DdbError {
        code: DdbErrorCode::ReplayGap as i32,
        message: "rehydrate".to_owned(),
        request_id: "req:1".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({
            "code": "DDB_ERROR_CODE_REPLAY_GAP",
            "message": "rehydrate",
            "requestId": "req:1"
        })
    );

    // A future numeric enum value is retained by the Protobuf representation.
    let future_binary = DdbError::decode([0x08, 0x7b].as_slice()).unwrap();
    assert_eq!(future_binary.code, 123);
    assert!(DdbErrorCode::try_from(future_binary.code).is_err());
    assert_eq!(future_binary.encode_to_vec(), [0x08, 0x7b]);

    // ProtoJSON cannot preserve an unknown enum name, so the documented
    // compatibility behavior maps it to UNSPECIFIED instead of rejecting the
    // entire additive response.
    let future_json: DdbError = serde_json::from_value(json!({
        "code": "DDB_ERROR_CODE_ADDED_LATER",
        "message": "future",
        "requestId": "req:future"
    }))
    .unwrap();
    assert_eq!(future_json.code, DdbErrorCode::Unspecified as i32);
}

#[test]
fn descriptor_contains_all_v2_services_and_no_unbounded_well_known_values() {
    let descriptors = FileDescriptorSet::decode(V2_FILE_DESCRIPTOR_SET).unwrap();
    let ddb_files = descriptors
        .file
        .iter()
        .filter(|file| file.package.as_deref() == Some("ddb.api.v2"))
        .collect::<Vec<_>>();

    let file_names = ddb_files
        .iter()
        .filter_map(|file| file.name.as_deref())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        file_names,
        BTreeSet::from([
            "ddb/api/v2/common.proto",
            "ddb/api/v2/debugger_service.proto",
            "ddb/api/v2/event_service.proto",
            "ddb/api/v2/extension.proto",
            "ddb/api/v2/resources.proto",
        ])
    );

    let services = ddb_files
        .iter()
        .flat_map(|file| file.service.iter())
        .filter_map(|service| service.name.as_deref())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        services,
        BTreeSet::from([
            "DdbAdminService",
            "DdbEventService",
            "DebuggerControlService",
            "DebuggerService",
        ])
    );

    for file in ddb_files {
        assert!(
            file.source_code_info.is_none(),
            "{} unexpectedly contains non-reproducible source info",
            file.name.as_deref().unwrap_or("<unnamed>")
        );
        assert_no_unbounded_well_known_values(&file.message_type);
    }
}

fn assert_no_unbounded_well_known_values(messages: &[DescriptorProto]) {
    const FORBIDDEN: &[&str] = &[
        ".google.protobuf.Any",
        ".google.protobuf.Struct",
        ".google.protobuf.Value",
    ];

    for message in messages {
        for field in &message.field {
            if let Some(type_name) = field.type_name.as_deref() {
                assert!(
                    !FORBIDDEN.contains(&type_name),
                    "{} uses forbidden unbounded type {}",
                    field.name.as_deref().unwrap_or("<unnamed>"),
                    type_name
                );
            }
        }
        assert_no_unbounded_well_known_values(&message.nested_type);
    }
}

#![no_main]

use ddb_api_types::v2::{
    Cursor, DdbError, DynamicValue, ExtensionDescriptor, ExtensionPayload, GetSnapshotRequest,
    GetSnapshotResponse, Operation, OutputEvent, StateEvent,
};
use libfuzzer_sys::fuzz_target;
use serde::{de::DeserializeOwned, Serialize};

fn decode_and_round_trip<T>(input: &[u8])
where
    T: DeserializeOwned + Serialize,
{
    if let Ok(message) = serde_json::from_slice::<T>(input) {
        let encoded = serde_json::to_vec(&message).expect("decoded ProtoJSON must serialize");
        let decoded = serde_json::from_slice::<T>(&encoded);
        assert!(decoded.is_ok(), "serialized ProtoJSON must decode");
    }
}

fuzz_target!(|input: &[u8]| {
    decode_and_round_trip::<Cursor>(input);
    decode_and_round_trip::<DdbError>(input);
    decode_and_round_trip::<DynamicValue>(input);
    decode_and_round_trip::<ExtensionDescriptor>(input);
    decode_and_round_trip::<ExtensionPayload>(input);
    decode_and_round_trip::<GetSnapshotRequest>(input);
    decode_and_round_trip::<GetSnapshotResponse>(input);
    decode_and_round_trip::<Operation>(input);
    decode_and_round_trip::<StateEvent>(input);
    decode_and_round_trip::<OutputEvent>(input);
});

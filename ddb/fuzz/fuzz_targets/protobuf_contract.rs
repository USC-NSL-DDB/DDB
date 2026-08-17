#![no_main]

use ddb_api_types::v2::{
    Cursor, DdbError, DynamicValue, ExtensionDescriptor, ExtensionPayload, GetSnapshotRequest,
    GetSnapshotResponse, Operation, OutputEvent, StateEvent,
};
use libfuzzer_sys::fuzz_target;
use prost::Message;

fn decode_and_round_trip<T>(input: &[u8])
where
    T: Message + Default,
{
    if let Ok(message) = T::decode(input) {
        let encoded = message.encode_to_vec();
        let decoded = T::decode(encoded.as_slice());
        assert!(
            decoded.is_ok(),
            "a successfully decoded message must re-encode"
        );
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

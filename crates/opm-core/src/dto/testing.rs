//! Helpers shared by the DTO unit tests.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Serialises `value`, parses it back and asserts it is unchanged.
///
/// Returns the intermediate JSON so tests can also assert the wire format.
pub(crate) fn roundtrip<T>(value: &T) -> Value
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let text = serde_json::to_string(value).expect("DTO serialises to JSON");
    let back: T = serde_json::from_str(&text).expect("DTO parses back from its own JSON");
    assert_eq!(&back, value, "JSON round trip changed the value: {text}");
    serde_json::from_str(&text).expect("serialised DTO is valid JSON")
}

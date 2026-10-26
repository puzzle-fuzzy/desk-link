use serde::{Deserialize, Deserializer, Serializer, de::Error as _, ser::Error as _};

const INVALID_IDENTITY: &str = "画面标识必须是非零 u64 的规范十进制字符串。";

pub(crate) fn parse(value: &str) -> Result<u64, String> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 20
        || !(b'1'..=b'9').contains(&bytes[0])
        || !bytes.iter().all(u8::is_ascii_digit)
    {
        return Err(INVALID_IDENTITY.to_owned());
    }
    value
        .parse::<u64>()
        .map_err(|_| INVALID_IDENTITY.to_owned())
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).map_err(D::Error::custom)
}

pub(crate) fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    if *value == 0 {
        return Err(S::Error::custom(INVALID_IDENTITY));
    }
    serializer.serialize_str(&value.to_string())
}

pub(crate) fn serialize_optional<S: Serializer>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(0) => Err(S::Error::custom(INVALID_IDENTITY)),
        Some(value) => serializer.serialize_some(&value.to_string()),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize)]
    struct Identity {
        #[serde(deserialize_with = "deserialize", serialize_with = "serialize")]
        value: u64,
    }

    #[test]
    fn full_u64_identities_round_trip_as_exact_strings_across_the_js_integer_boundary() {
        for value in [
            1,
            (1_u64 << 53) - 1,
            1_u64 << 53,
            (1_u64 << 53) + 1,
            u64::MAX,
        ] {
            let json = serde_json::to_value(Identity { value }).unwrap();
            assert_eq!(json["value"], value.to_string());
            assert_eq!(
                serde_json::from_value::<Identity>(json).unwrap().value,
                value
            );
        }
    }

    #[test]
    fn noncanonical_and_numeric_json_identities_are_rejected_without_rounding() {
        for text in [
            "",
            "0",
            "00",
            "01",
            "-1",
            "+1",
            "1.0",
            "1e2",
            " 1",
            "1 ",
            "18446744073709551616",
        ] {
            assert!(parse(text).is_err(), "unexpected identity {text}");
            assert!(serde_json::from_value::<Identity>(serde_json::json!({"value":text})).is_err());
        }
        for value in [
            serde_json::json!(1),
            serde_json::json!(9007199254740992_u64),
            serde_json::Value::Null,
        ] {
            assert!(
                serde_json::from_value::<Identity>(serde_json::json!({"value":value})).is_err()
            );
        }
        assert!(serde_json::to_value(Identity { value: 0 }).is_err());
    }
}

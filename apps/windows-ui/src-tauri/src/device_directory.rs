use desklink_transport::DIRECTORY_ACCESS_CODE_BYTES;

const PUBLIC_ID_BASE: u64 = 100_000_000_000;
const PUBLIC_ID_RANGE: u64 = 900_000_000_000;
const ACCESS_CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

pub fn public_device_id(device_id: [u8; 16]) -> u64 {
    let seed = u64::from_be_bytes(device_id[..8].try_into().expect("fixed device ID prefix"));
    PUBLIC_ID_BASE + seed % PUBLIC_ID_RANGE
}

pub fn format_device_id(device_id: u64) -> String {
    let digits = format!("{device_id:012}");
    format!(
        "{} {} {} {}",
        &digits[0..3],
        &digits[3..6],
        &digits[6..9],
        &digits[9..12]
    )
}

pub fn parse_device_id(value: &str) -> Result<u64, &'static str> {
    let digits = value
        .chars()
        .filter(|character| !matches!(character, ' ' | '-'))
        .collect::<String>();
    if digits.len() != 12 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("设备 ID 应为 12 位数字。");
    }
    let device_id = digits.parse::<u64>().map_err(|_| "设备 ID 无效。")?;
    if !(PUBLIC_ID_BASE..PUBLIC_ID_BASE + PUBLIC_ID_RANGE).contains(&device_id) {
        return Err("设备 ID 无效。");
    }
    Ok(device_id)
}

/// Parses a controller-entered access password into the final directory code.
///
/// Generated temporary pairing codes keep the display-alphabet normalization
/// (grouping separators removed, uppercased) so the pairing screen and typed
/// input agree. User-chosen fixed passwords of 8–64 arbitrary characters pass
/// through verbatim, and both cases derive the wire code with the shared
/// domain-separated digest so hosts and controllers always agree regardless
/// of which characters (including `0`, `1`, `I`, `O` or non-ASCII) are used.
pub fn parse_access_code(value: &str) -> Result<[u8; DIRECTORY_ACCESS_CODE_BYTES], &'static str> {
    let trimmed = value.trim();
    let normalized = trimmed
        .chars()
        .filter(|character| !matches!(character, ' ' | '-'))
        .flat_map(char::to_uppercase)
        .collect::<String>();
    let password_bytes = if normalized.len() == DIRECTORY_ACCESS_CODE_BYTES
        && normalized
            .as_bytes()
            .iter()
            .all(|byte| ACCESS_CODE_ALPHABET.contains(byte))
    {
        normalized.as_bytes()
    } else {
        trimmed.as_bytes()
    };
    let code = desklink_crypto::PairingCode::derive_from_password(password_bytes)
        .map_err(|_| "访问密码需为 8 到 64 个字符，可以使用任意字符。")?;
    Ok(*code.as_bytes())
}

/// True when a fixed password would be reinterpreted by temporary-code
/// normalization (separators removed and uppercased), which would make a
/// Windows controller derive a different directory code than the host
/// registered. Setting such a password is rejected so every typed password
/// resolves to exactly one directory code on every platform.
pub fn fixed_password_conflicts_with_temporary_normalization(value: &str) -> bool {
    let trimmed = value.trim();
    let normalized = trimmed
        .chars()
        .filter(|character| !matches!(character, ' ' | '-'))
        .flat_map(char::to_uppercase)
        .collect::<String>();
    normalized.len() == DIRECTORY_ACCESS_CODE_BYTES
        && normalized
            .as_bytes()
            .iter()
            .all(|byte| ACCESS_CODE_ALPHABET.contains(byte))
        && normalized.as_bytes() != trimmed.as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_id_is_stable_and_human_grouped() {
        let id = public_device_id([7; 16]);
        assert_eq!(id, public_device_id([7; 16]));
        assert_eq!(format_device_id(id).replace(' ', "").len(), 12);
    }

    #[test]
    fn parsers_accept_grouping_but_normalize_temporary_codes() {
        assert_eq!(parse_device_id("123 456 789 012"), Ok(123_456_789_012));
        assert_eq!(parse_device_id("123-456-789-012"), Ok(123_456_789_012));
        assert_eq!(
            parse_access_code("ab2d-ef3g"),
            parse_access_code("AB2DEF3G")
        );
    }

    #[test]
    fn access_codes_accept_user_chosen_passwords_and_derive_the_wire_code() {
        let expected =
            desklink_crypto::PairingCode::derive_from_password(b"My0Passw0rd-2026!").unwrap();
        assert_eq!(
            parse_access_code("My0Passw0rd-2026!"),
            Ok(*expected.as_bytes())
        );
        // Confusable characters are ordinary password characters now.
        assert_eq!(parse_access_code("ABCD0FG1"), {
            let code = desklink_crypto::PairingCode::derive_from_password(b"ABCD0FG1").unwrap();
            Ok(*code.as_bytes())
        });
        // Non-ASCII passwords derive exactly like the Apple implementation.
        assert_eq!(parse_access_code("correct horse:电池 🐎!"), {
            let code = desklink_crypto::PairingCode::derive_from_password(
                "correct horse:电池 🐎!".as_bytes(),
            )
            .unwrap();
            Ok(*code.as_bytes())
        });
        // Custom passwords are case sensitive; only temporary codes uppercase.
        assert_ne!(
            parse_access_code("MyPassword2026"),
            parse_access_code("mypassword2026")
        );
        // Length bounds follow the shared crypto rule (8..=64 bytes).
        assert!(parse_access_code("short").is_err());
        assert!(parse_access_code(&"a".repeat(65)).is_err());
    }

    #[test]
    fn fixed_passwords_that_normalize_like_temporary_codes_are_flagged() {
        // An already-canonical temporary-shaped password is used verbatim.
        assert!(!fixed_password_conflicts_with_temporary_normalization(
            "AB2DEF3G"
        ));
        assert!(!fixed_password_conflicts_with_temporary_normalization(
            "My0Passw0rd-2026!"
        ));
        // Lowercase or separated variants would be reinterpreted and are
        // rejected when the user picks them as fixed passwords.
        assert!(fixed_password_conflicts_with_temporary_normalization(
            "ab2def3g"
        ));
        assert!(fixed_password_conflicts_with_temporary_normalization(
            "ab2-def3g"
        ));
    }
}

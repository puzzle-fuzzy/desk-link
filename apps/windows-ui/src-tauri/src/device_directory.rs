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

pub fn parse_access_code(value: &str) -> Result<[u8; DIRECTORY_ACCESS_CODE_BYTES], &'static str> {
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, ' ' | '-'))
        .flat_map(char::to_uppercase)
        .collect::<String>();
    let bytes = normalized.as_bytes();
    if bytes.len() != DIRECTORY_ACCESS_CODE_BYTES
        || !bytes.iter().all(|byte| ACCESS_CODE_ALPHABET.contains(byte))
    {
        return Err("临时密码应为 8 位大写字母或数字。");
    }
    bytes.try_into().map_err(|_| "临时密码无效。")
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
    fn parsers_accept_grouping_but_reject_ambiguous_codes() {
        assert_eq!(parse_device_id("123 456 789 012"), Ok(123_456_789_012));
        assert_eq!(parse_device_id("123-456-789-012"), Ok(123_456_789_012));
        assert_eq!(parse_access_code("ABCD-EFGH"), Ok(*b"ABCDEFGH"));
        assert!(parse_access_code("ABCD0FGH").is_err());
    }
}

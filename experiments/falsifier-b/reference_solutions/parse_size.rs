pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() { return None; }
    let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    if digits_end == 0 { return None; }
    let n: u64 = t[..digits_end].parse().ok()?;
    let rest = t[digits_end..].trim();
    let mult: u64 = match rest.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1024, "mib" => 1024u64.pow(2), "gib" => 1024u64.pow(3), "tib" => 1024u64.pow(4),
        "kb" => 1000, "mb" => 1000u64.pow(2), "gb" => 1000u64.pow(3), "tb" => 1000u64.pow(4),
        _ => return None,
    };
    n.checked_mul(mult)
}

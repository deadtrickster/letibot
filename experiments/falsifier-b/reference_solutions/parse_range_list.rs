pub fn parse_range_list(s: &str) -> Option<Vec<u32>> {
    let t = s.trim();
    if t.is_empty() { return Some(Vec::new()); }
    let mut out = Vec::new();
    for item in t.split(',') {
        let item = item.trim();
        if item.is_empty() { return None; }
        let parts: Vec<&str> = item.split('-').collect();
        match parts.len() {
            1 => {
                let n: u32 = parse_num(parts[0])?;
                out.push(n);
            }
            2 => {
                let a: u32 = parse_num(parts[0])?;
                let b: u32 = parse_num(parts[1])?;
                if a > b { return None; }
                for v in a..=b { out.push(v); }
            }
            _ => return None,
        }
    }
    Some(out)
}
fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { return None; }
    s.parse().ok()
}

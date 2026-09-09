pub fn merge_intervals(intervals: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = intervals.to_vec();
    v.sort();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => { if e > last.1 { last.1 = e; } }
            _ => out.push((s, e)),
        }
    }
    out
}

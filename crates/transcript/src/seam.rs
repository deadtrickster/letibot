//! The spill seam: `[... N bytes, L lines omitted; read_spill hash=H offset=O length=N returns them ...]`.

/// What a spill took out of the middle of a result, and where `read_spill` finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seam {
    pub bytes: usize,
    pub lines: usize,
    pub hash: String,
    /// The first omitted byte in the full output.
    pub offset: usize,
}

const OPEN: &str = "[... ";
const CLOSE: &str = " returns them ...]";

impl Seam {
    /// The line the model reads. Everything `read_spill` needs is in it, so one call follows.
    pub fn render(&self) -> String {
        format!(
            "{OPEN}{} bytes, {} lines omitted; read_spill hash={} offset={} length={}{CLOSE}",
            self.bytes, self.lines, self.hash, self.offset, self.bytes
        )
    }

    /// The seam back from a line of a result, or `None` for any other line.
    pub fn parse(line: &str) -> Option<Seam> {
        let body = line.trim().strip_prefix(OPEN)?.strip_suffix(CLOSE)?;
        let (bytes, rest) = body.split_once(" bytes, ")?;
        let (lines, rest) = rest.split_once(" lines omitted; read_spill hash=")?;
        let (hash, rest) = rest.split_once(" offset=")?;
        let (offset, length) = rest.split_once(" length=")?;
        let seam = Seam {
            bytes: bytes.parse().ok()?,
            lines: lines.parse().ok()?,
            hash: hash.to_string(),
            offset: offset.parse().ok()?,
        };
        (length.parse::<usize>().ok()? == seam.bytes && !seam.hash.is_empty()).then_some(seam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seam_reads_back_as_itself_and_nothing_else_does() {
        let s = Seam {
            bytes: 150_123,
            lines: 3_011,
            hash: "0123456789abcdef".into(),
            offset: 25_000,
        };
        assert_eq!(Seam::parse(&s.render()), Some(s.clone()));
        assert_eq!(Seam::parse(&format!("  {}  ", s.render())), Some(s));
        assert_eq!(Seam::parse("[... ordinary text ...]"), None);
        assert_eq!(Seam::parse("line 42"), None);
    }
}

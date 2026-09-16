//! OSC 133 semantic prompt zones and prompt jumps.
//!
//! The upstream reference (`packages/tui/src/tui-alt-screen.ts`) stamps rendered
//! transcript rows with OSC 133 zone markers and implements previous/next
//! semantic prompt navigation by finding rows that begin a prompt (`A`). This
//! module provides the equivalent, terminal-independent primitive: parse the
//! markers, index the prompt rows, and answer prompt jumps. Painting and
//! viewport ownership stay with the embedding application.

/// One OSC 133 shell-integration zone.
///
/// * `PromptStart` (`A`) — the shell is about to print a prompt.
/// * `CommandStart` (`B`) — the user submitted a command; the prompt is done.
/// * `OutputStart` (`C`) — command output begins (also used for prompt redraws
///   and `D`-less submissions in practice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PromptZone {
    /// OSC 133 `A`: prompt start.
    PromptStart,
    /// OSC 133 `B`: command start.
    CommandStart,
    /// OSC 133 `C`: command output start.
    OutputStart,
}

impl PromptZone {
    /// The single-letter OSC 133 parameter for this zone.
    #[must_use]
    pub fn marker(self) -> char {
        match self {
            Self::PromptStart => 'A',
            Self::CommandStart => 'B',
            Self::OutputStart => 'C',
        }
    }

    /// Parse a single OSC 133 zone letter.
    #[must_use]
    pub fn from_marker(marker: char) -> Option<Self> {
        match marker.to_ascii_uppercase() {
            'A' => Some(Self::PromptStart),
            'B' => Some(Self::CommandStart),
            'C' => Some(Self::OutputStart),
            _ => None,
        }
    }
}

/// Byte length of one OSC 133 sequence beginning at `bytes[0]`, if any.
///
/// Both terminator forms are accepted: `BEL` (`0x07`) and `ST` (`ESC \`).
fn zone_sequence_len(bytes: &[u8]) -> Option<(usize, PromptZone)> {
    const PREFIX: &[u8] = b"\x1b]133;";
    if !bytes.starts_with(PREFIX) || bytes.len() <= PREFIX.len() + 1 {
        return None;
    }
    let zone = PromptZone::from_marker(bytes[PREFIX.len()] as char)?;
    let rest = &bytes[PREFIX.len() + 1..];
    if rest.first() == Some(&0x07) {
        return Some((PREFIX.len() + 2, zone));
    }
    if rest.starts_with(b"\x1b\\") {
        return Some((PREFIX.len() + 3, zone));
    }
    None
}

/// Every OSC 133 zone marker that prefixes `line`, in wire order.
///
/// Markers are only recognized at the line start, matching the upstream
/// `OSC133_ZONE_PREFIX` anchor. A line may carry several markers (for example
/// `A` immediately followed by `B`).
#[must_use]
pub fn zone_markers(line: &str) -> Vec<PromptZone> {
    let mut zones = Vec::new();
    let mut rest = line.as_bytes();
    while let Some((len, zone)) = zone_sequence_len(rest) {
        zones.push(zone);
        rest = &rest[len..];
    }
    zones
}

/// Remove every leading OSC 133 zone marker from `line`.
#[must_use]
pub fn strip_zone_markers(line: &str) -> &str {
    let mut rest = line;
    loop {
        match zone_sequence_len(rest.as_bytes()) {
            Some((len, _)) => rest = &rest[len..],
            None => return rest,
        }
    }
}

/// Prompt-zone index over a rendered transcript.
///
/// Rows are matched positionally; the caller keeps the row table. Constructing
/// the index is `O(rows)` and jump queries are `O(1)` per recorded boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptZones {
    zones: Vec<Vec<PromptZone>>,
    prompt_rows: Vec<usize>,
}

impl PromptZones {
    /// Index the zone markers of each rendered row.
    #[must_use]
    pub fn scan<I, S>(rows: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut zones = Vec::new();
        let mut prompt_rows = Vec::new();
        for (row, line) in rows.into_iter().enumerate() {
            let markers = zone_markers(line.as_ref());
            if markers.contains(&PromptZone::PromptStart) {
                prompt_rows.push(row);
            }
            zones.push(markers);
        }
        Self { zones, prompt_rows }
    }

    /// Build an index from trusted semantic row boundaries without putting OSC
    /// bytes in rendered text. Out-of-range boundaries are ignored.
    #[must_use]
    pub fn from_boundaries(
        row_count: usize,
        boundaries: impl IntoIterator<Item = (usize, PromptZone)>,
    ) -> Self {
        let mut zones = vec![Vec::new(); row_count];
        for (row, zone) in boundaries {
            if let Some(markers) = zones.get_mut(row) {
                if !markers.contains(&zone) {
                    markers.push(zone);
                }
            }
        }
        let prompt_rows = zones
            .iter()
            .enumerate()
            .filter_map(|(row, markers)| markers.contains(&PromptZone::PromptStart).then_some(row))
            .collect();
        Self { zones, prompt_rows }
    }

    /// Rows that begin a prompt (`A` zones), in ascending order.
    #[must_use]
    pub fn prompt_rows(&self) -> &[usize] {
        &self.prompt_rows
    }

    /// Zones stamped on `row`.
    #[must_use]
    pub fn zones_at(&self, row: usize) -> &[PromptZone] {
        self.zones.get(row).map_or(&[], Vec::as_slice)
    }

    /// Whether `row` carries the given zone.
    #[must_use]
    pub fn has_zone(&self, row: usize, zone: PromptZone) -> bool {
        self.zones_at(row).contains(&zone)
    }

    /// The last row at or before `row` that begins a prompt.
    #[must_use]
    pub fn prompt_at_or_before(&self, row: usize) -> Option<usize> {
        self.prompt_rows
            .iter()
            .rev()
            .copied()
            .find(|candidate| *candidate <= row)
    }

    /// The row of the previous semantic prompt strictly before `row`.
    #[must_use]
    pub fn previous_prompt(&self, row: usize) -> Option<usize> {
        self.prompt_rows
            .iter()
            .rev()
            .copied()
            .find(|candidate| *candidate < row)
    }

    /// The row of the next semantic prompt strictly after `row`.
    #[must_use]
    pub fn next_prompt(&self, row: usize) -> Option<usize> {
        self.prompt_rows
            .iter()
            .copied()
            .find(|candidate| *candidate > row)
    }

    /// Target row for a prompt jump from `row`.
    ///
    /// Returns `None` when there is no prompt in the requested direction, which
    /// callers treat as "leave the viewport where it is".
    #[must_use]
    pub fn jump(&self, row: usize, forward: bool) -> Option<usize> {
        if forward {
            self.next_prompt(row)
        } else {
            self.previous_prompt(row)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_BEL: &str = "\x1b]133;A\x07";
    const B_ST: &str = "\x1b]133;B\x1b\\";
    const C_BEL: &str = "\x1b]133;C\x07";

    #[test]
    fn markers_are_parsed_at_line_start_only() {
        assert_eq!(
            zone_markers(&format!("{A_BEL}$ ls")),
            vec![PromptZone::PromptStart]
        );
        assert_eq!(zone_markers(B_ST), vec![PromptZone::CommandStart]);
        assert_eq!(
            zone_markers(&format!("{A_BEL}{B_ST}ready")),
            vec![PromptZone::PromptStart, PromptZone::CommandStart]
        );
        // Mid-line and malformed sequences are literal content.
        assert!(zone_markers("text \x1b]133;A\x07").is_empty());
        assert!(zone_markers("\x1b]133;D\x07").is_empty());
        assert!(zone_markers("\x1b]133;A").is_empty());
    }

    #[test]
    fn stripping_removes_repeated_markers_without_touching_content() {
        assert_eq!(strip_zone_markers(&format!("{A_BEL}{C_BEL}hello")), "hello");
        assert_eq!(strip_zone_markers("plain"), "plain");
        assert_eq!(strip_zone_markers(""), "");
    }

    #[test]
    fn scan_indexes_zone_rows_and_prompt_rows() {
        let rows = [
            format!("{A_BEL}{B_ST}prompt> hello"),
            "world".to_owned(),
            format!("{C_BEL}output"),
            format!("{A_BEL}{B_ST}prompt> again"),
        ];
        let zones = PromptZones::scan(&rows);
        assert_eq!(zones.prompt_rows(), &[0, 3]);
        assert!(zones.has_zone(0, PromptZone::CommandStart));
        assert!(!zones.has_zone(1, PromptZone::OutputStart));
        assert!(zones.has_zone(2, PromptZone::OutputStart));
    }

    #[test]
    fn prompt_jumps_walk_semantic_prompts_in_both_directions() {
        let rows = [
            format!("{A_BEL}one"),
            "body".to_owned(),
            format!("{A_BEL}two"),
            "body".to_owned(),
            format!("{A_BEL}three"),
        ];
        let zones = PromptZones::scan(&rows);
        assert_eq!(zones.jump(3, false), Some(2));
        assert_eq!(zones.jump(3, true), Some(4));
        assert_eq!(zones.jump(0, false), None);
        assert_eq!(zones.jump(4, true), None);
        // A row that is itself a prompt never jumps to itself.
        assert_eq!(zones.jump(2, false), Some(0));
        assert_eq!(zones.jump(2, true), Some(4));
        assert_eq!(zones.prompt_at_or_before(3), Some(2));
        assert_eq!(zones.prompt_at_or_before(0), Some(0));
    }

    #[test]
    fn empty_transcript_indexes_no_prompts() {
        let zones = PromptZones::scan(Vec::<String>::new());
        assert!(zones.prompt_rows().is_empty());
        assert_eq!(zones.jump(0, true), None);
        assert_eq!(zones.jump(0, false), None);
    }
}

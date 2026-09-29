//! Reusable unified-diff representation and parser.

/// A parsed unified diff. Prefix characters are always preserved, so colour is
/// never the sole carrier of meaning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnifiedDiff {
    pub lines: Vec<DiffLine>,
}

impl UnifiedDiff {
    /// Parse complete or partial unified-diff text without rejecting unknown
    /// headers. Unrecognized lines remain visible as metadata.
    pub fn parse(source: &str) -> Self {
        let mut lines = Vec::new();
        let mut old_line = None;
        let mut new_line = None;

        for raw in source.split_terminator('\n') {
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            let (kind, old_number, new_number) = if raw.starts_with("@@") {
                if let Some((old, new)) = parse_hunk_header(raw) {
                    old_line = Some(old);
                    new_line = Some(new);
                }
                (DiffLineKind::HunkHeader, None, None)
            } else if raw.starts_with("diff --git ")
                || raw.starts_with("index ")
                || raw.starts_with("--- ")
                || raw.starts_with("+++ ")
                || raw.starts_with("rename from ")
                || raw.starts_with("rename to ")
                || raw.starts_with("similarity index ")
                || raw.starts_with("new file mode ")
                || raw.starts_with("deleted file mode ")
            {
                (DiffLineKind::FileHeader, None, None)
            } else if raw.starts_with("Binary files ")
                || raw.starts_with("GIT binary patch")
                || raw.starts_with("literal ")
            {
                (DiffLineKind::Binary, None, None)
            } else if raw.starts_with('+') {
                let number = new_line;
                new_line = new_line.map(|line| line.saturating_add(1));
                (DiffLineKind::Addition, None, number)
            } else if raw.starts_with('-') {
                let number = old_line;
                old_line = old_line.map(|line| line.saturating_add(1));
                (DiffLineKind::Removal, number, None)
            } else if raw.starts_with(' ') {
                let old_number = old_line;
                let new_number = new_line;
                old_line = old_line.map(|line| line.saturating_add(1));
                new_line = new_line.map(|line| line.saturating_add(1));
                (DiffLineKind::Context, old_number, new_number)
            } else if raw.starts_with("\\ No newline at end of file") {
                (DiffLineKind::Metadata, None, None)
            } else if raw.is_empty() {
                (DiffLineKind::Context, old_line, new_line)
            } else {
                (DiffLineKind::Metadata, None, None)
            };
            lines.push(DiffLine {
                kind,
                text: raw.to_owned(),
                old_number,
                new_number,
            });
        }

        Self { lines }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Exact copyable diff text (apart from normalized line endings).
    pub fn plain_text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    pub old_number: Option<u64>,
    pub new_number: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    FileHeader,
    HunkHeader,
    Addition,
    Removal,
    Context,
    Binary,
    Metadata,
}

/// Unified-diff layout policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffRenderOptions {
    pub line_numbers: bool,
    /// Wrap long rows with a hanging prefix. When false, rows are clipped.
    pub wrap: bool,
}

fn parse_hunk_header(header: &str) -> Option<(u64, u64)> {
    // @@ -old,count +new,count @@ optional heading
    let mut parts = header.split_whitespace();
    (parts.next()? == "@@").then_some(())?;
    let old = parse_range(parts.next()?, '-')?;
    let new = parse_range(parts.next()?, '+')?;
    Some((old, new))
}

fn parse_range(value: &str, prefix: char) -> Option<u64> {
    value.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

#[cfg(test)]
mod tests;

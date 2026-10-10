//! Rich presentation of Octet-owned fact reports, without changing their CLI text.
//!
//! The built-in producers use `Label: value` or `Label  value` rows. Only this
//! opt-in path interprets that grammar; arbitrary text/extension output keeps its
//! literal renderer. Every source fragment is escaped before Markdown parsing.

use super::terminal_text::sanitize_for_terminal;

fn literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            // Backslash-escaping brackets/parentheses would introduce Pi's
            // \\[...\\] / \\(...\\) math delimiters. Entities are decoded as text
            // by CommonMark, never re-parsed as math or Markdown syntax.
            escaped.push_str(&format!("&#{};", u32::from(character)));
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn field(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(": ");
    let padding = line.find("  ");
    let separator = match (colon, padding) {
        (Some(colon), Some(padding)) => colon.min(padding),
        (Some(separator), None) | (None, Some(separator)) => separator,
        (None, None) => return None,
    };
    let label = line[..separator].trim();
    let value = line[separator + 2..].trim_start();
    (!label.is_empty()).then_some((label, value))
}

fn section(title: &str, label: &str) -> Option<&'static str> {
    match (title, label) {
        ("Status", "Provider") => Some("Model and connection"),
        ("Status", "Workspace") => Some("Session"),
        ("Status", "Model turns") => Some("Activity"),
        ("Status", "Extensions") => Some("Extensions"),
        ("Status", "Security model") => Some("Safety and permissions"),
        ("Settings", "Configured model") => Some("Launch and session"),
        ("Settings", "Theme") => Some("Display"),
        ("Settings", "Cache warming") => Some("Billable refresh policy"),
        ("Settings", "Editor padding") => Some("Editor layout"),
        ("Settings", "Project trust is deliberately not persisted here") => Some("Workspace trust"),
        ("Settings", "Change") => Some("Change a preference"),
        _ => None,
    }
}

fn heading(source: &mut String, text: &str) {
    source.push_str("\n## ");
    source.push_str(&literal(text));
    source.push_str("\n\n");
}

fn fact(source: &mut String, label: &str, value: &str) {
    source.push_str("- **");
    source.push_str(&literal(label));
    source.push_str(":** ");
    // Basis points are useful diagnostics, but not a human-first hit rate.
    let display_value = if label == "Cache hit rate" {
        value
            .strip_suffix(" bp")
            .and_then(|rate| rate.parse::<u16>().ok())
            .map(|rate| format!("{}.{:02}% ({rate} bp)", rate / 100, rate % 100))
    } else {
        None
    };
    source.push_str(&literal(display_value.as_deref().unwrap_or(value)));
    source.push('\n');
}

/// Render a built-in fact report as headings and labeled rows. Plain command
/// output and provider/session payloads remain unchanged.
pub(super) fn facts_markdown(title: &str, text: &str) -> String {
    let safe = sanitize_for_terminal(text);
    let mut source = String::with_capacity(safe.len());
    let mut table: Option<Vec<&str>> = None;
    for line in safe.lines() {
        let line = line.trim();
        if line.is_empty() {
            table = None;
            source.push('\n');
            continue;
        }
        if line == "octet settings" || (title == "Help" && line == "octet help") {
            continue; // The ordinary report already supplies its title.
        }
        if matches!(line, "Cache warming" | "Telemetry" | "Model cycling scope") {
            heading(&mut source, line);
            continue;
        }
        if line == "Slash commands:" {
            heading(&mut source, "Slash commands");
            continue;
        }
        if line.starts_with('/') {
            if let Some((command, description)) = line.split_once(" — ") {
                fact(&mut source, command, description);
                continue;
            }
        }
        // These two built-in accounting tables are wider than small panes.
        // Stack each record's named cells instead of clipping an ASCII table.
        if line.starts_with("Turn  Model") || line.starts_with("Assistant entry    Expected") {
            heading(
                &mut source,
                if title == "Cost" {
                    "Model calls"
                } else {
                    "Material misses"
                },
            );
            table = Some(columns(line));
            continue;
        }
        if let Some(headers) = &table {
            if line
                .chars()
                .all(|character| character == '─' || character.is_whitespace())
            {
                continue;
            }
            let cells = columns(line);
            let cost = headers.first() == Some(&"Turn");
            let total = cost && cells.first() == Some(&"Total") && cells.len() + 1 == headers.len();
            if cells.len() == headers.len() || total {
                source.push_str("\n### ");
                source.push_str(&literal(if total {
                    "Totals"
                } else if cost {
                    "Call"
                } else {
                    "Entry"
                }));
                if !total {
                    source.push(' ');
                    source.push_str(&literal(cells[0]));
                }
                if cost && !total {
                    source.push_str(" — ");
                    source.push_str(&literal(cells[1]));
                }
                source.push_str("\n\n");
                let header_start = if cost { 2 } else { 1 };
                let cell_start = if total { 1 } else { header_start };
                for (header, cell) in headers[header_start..].iter().zip(&cells[cell_start..]) {
                    let label = if total && *header == "Total µ$" {
                        "Total spend"
                    } else {
                        header
                    };
                    fact(&mut source, label, cell);
                }
                continue;
            }
            // Preserve every value if a producer row is not in the table grammar.
        }
        if line.starts_with("Cost guardrails limit ") {
            heading(&mut source, "Spend and cache");
            if let Some((limit, warning)) = line
                .strip_prefix("Cost guardrails limit ")
                .and_then(|value| value.split_once(" · turn warning "))
            {
                fact(&mut source, "Session cost limit", limit);
                fact(&mut source, "Turn cost warning", warning);
                continue;
            }
        }
        if let Some((label, value)) = field(line) {
            if let Some(section) = section(title, label) {
                heading(&mut source, section);
            }
            fact(&mut source, label, value);
        } else {
            source.push_str(&literal(line));
            source.push_str("  \n");
        }
    }
    source
}

fn columns(line: &str) -> Vec<&str> {
    line.split("  ")
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect()
}

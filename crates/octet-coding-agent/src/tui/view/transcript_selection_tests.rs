use super::*;

#[test]
fn bounded_copy_rows_match_full_receipt_at_scrolled_unicode_cells() {
    let content: Vec<_> = (0..120).map(|row| format!("row-{row:03} 界 é")).collect();
    let text = content.join("\n");
    let mut painted = vec![String::new()];
    painted.extend(content.iter().map(|line| format!("  {line}")));
    painted.push(String::new());
    let geometry = SurfaceGeometry {
        leading_rows: 1,
        trailing_rows: 1,
        content_left: 2,
        ..Default::default()
    };
    let full = CopyRows::build(&painted, &text, geometry);
    let bounded =
        visible_copy_rows(&painted, &text, geometry, 90..96, &mut Default::default()).unwrap();
    assert_eq!(bounded.first_row, 89);
    assert_eq!(bounded.rows.len(), 6);
    for row in 89..95 {
        for col in 0..18 {
            assert_eq!(bounded.offset_for(row, col), full.offset_for(row, col));
        }
        let offset = full.offset_for(row, 1).unwrap();
        assert_eq!(
            bounded.row_for_offset(offset, false),
            full.row_for_offset(offset, false)
        );
    }
    assert!(bounded.offset_for(88, 0).is_none());
    assert!(bounded.offset_for(95, 0).is_none());
    assert!(visible_copy_rows(&painted, &text, geometry, 0..1, &mut Default::default()).is_none());
    assert!(
        visible_copy_rows(&painted, &text, geometry, 121..122, &mut Default::default()).is_none()
    );
}

#[test]
fn selection_decoration_preserves_plain_unicode_and_leaves_blank_rows_alone() {
    let row = CopyRow {
        text: "a界éz".into(),
        offset: 7,
        painted_cells: 2,
    };
    let mut line = "  a界éz".to_owned();
    row.decorate(&mut line, 8, 13);
    assert_eq!(strip_terminal_sequences(&line), "  a界éz");
    assert!(line.contains("\x1b[0;7m界é\x1b[0m"));
    let empty = CopyRow {
        text: String::new(),
        offset: 7,
        painted_cells: 2,
    };
    let mut blank = "  ".to_owned();
    empty.decorate(&mut blank, 0, 100);
    assert_eq!(blank, "  ");
}

#[test]
fn memoized_copy_rows_match_a_fresh_alignment_through_a_stream() {
    // A painter shaped like the transcript: bullets, styled rows, frame rows
    // that are not copy text, and prose re-wrapped as the answer grows.
    fn paint(text: &str) -> Vec<String> {
        let mut painted = vec![String::new()];
        for (index, line) in text.split('\n').enumerate() {
            // Headings paint a marker the copy text spells differently, so
            // the whole-row candidate misses and the line candidate matches.
            let (marker, body) = if let Some(body) = line.strip_prefix("- ") {
                ("• ", body)
            } else if let Some(body) = line.strip_prefix("## ") {
                ("* ", body)
            } else {
                ("", line)
            };
            let words = body.split(' ').collect::<Vec<_>>();
            for (chunk, words) in words.chunks(5).enumerate() {
                let lead = if chunk == 0 { marker } else { "  " };
                let row = format!("  {lead}{}", words.join(" "));
                painted.push(if index % 3 == 0 {
                    format!("\x1b[1m{row}\x1b[0m")
                } else {
                    row
                });
            }
            if index % 4 == 3 {
                painted.push("  ────────".into());
            }
        }
        painted.push(String::new());
        painted
    }
    let words = (0..400)
        .map(|word| match word % 37 {
            0 => "\n- item".to_owned(),
            11 => format!("\n## head{word}\n"),
            19 => "\nprose".to_owned(),
            // A later literal spelling of a painted heading row.
            29 => format!("* head{}", word - 18),
            _ => format!("w{word}"),
        })
        .collect::<Vec<_>>();
    let full = words.join(" ");
    let geometry = SurfaceGeometry {
        leading_rows: 1,
        trailing_rows: 1,
        content_left: 2,
        ..Default::default()
    };
    let mut memo = BlockCopyMemo::default();
    let mut end = 0;
    while end < full.len() {
        end = (end + 7).min(full.len());
        while !full.is_char_boundary(end) {
            end += 1;
        }
        let text = &full[..end];
        let painted = paint(text);
        let rows = painted.len();
        for visible in [rows.saturating_sub(12)..rows, 0..rows, 3..9] {
            let fresh = CopyRows::build_visible(&painted, text, geometry, visible.clone());
            let memoized =
                CopyRows::build_visible_with(&painted, text, geometry, visible.clone(), &mut memo);
            assert_eq!(
                format!("{memoized:?}"),
                format!("{fresh:?}"),
                "copy rows diverged at {end} bytes for rows {visible:?}"
            );
        }
    }
    // A changed inset discards every remembered row.
    let painted = paint(&full);
    let moved = SurfaceGeometry {
        content_left: 4,
        ..geometry
    };
    assert_eq!(
        format!(
            "{:?}",
            CopyRows::build_visible_with(&painted, &full, moved, 0..painted.len(), &mut memo)
        ),
        format!("{:?}", CopyRows::build(&painted, &full, moved))
    );
}

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
    let bounded = visible_copy_rows(&painted, &text, geometry, 90..96).unwrap();
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
    assert!(visible_copy_rows(&painted, &text, geometry, 0..1).is_none());
    assert!(visible_copy_rows(&painted, &text, geometry, 121..122).is_none());
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

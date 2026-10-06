use super::*;

#[test]
fn identity_row_retains_allocation_and_exact_reset_bytes() {
    let source =
        "  \x1b[1mverified\x1b[0m result with \x1b]8;;https://example.test\x07code\x1b]8;;\x07";
    let mut row = String::with_capacity(source.len() + PI_LINE_RESET.len());
    row.push_str(source);
    let allocation = row.as_ptr();
    normalize_pi_row(&mut row);
    assert_eq!(
        row.as_ptr(),
        allocation,
        "no normalization temporary or row clone"
    );
    assert_eq!(row, format!("{source}{PI_LINE_RESET}"));
}

#[test]
fn owned_row_normalization_matches_original_for_unicode_and_escape_payloads() {
    for text in [
        "",
        "plain",
        "界 e\u{301} 👩‍💻 🇨🇦",
        "ำຳ\t",
        "\t\x1b[31m界\x1b[0m",
        "\x1b]0;a\tb\x1b\\x\t",
        "\x1b]8;;https://example.test/ำ\x07label\x1b]8;;\x07",
        "\x1bPຳ\t\x1b\\tail",
        "\x1b[31",
        "\x1b]unterminatedำ\t",
        "\0\r\n\x7f",
    ] {
        let expected = format!(
            "{}{}",
            crate::utils::normalize_terminal_output(text),
            PI_LINE_RESET
        );
        let mut actual = text.to_owned();
        normalize_pi_row(&mut actual);
        assert_eq!(actual, expected, "{text:?}");
    }
}

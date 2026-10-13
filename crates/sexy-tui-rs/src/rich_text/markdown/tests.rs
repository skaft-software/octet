//! Unit tests for `crate::rich_text::markdown`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::markdown`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn parses_required_markdown_constructs() {
    let document = parse(
        "# Heading\n\nParagraph with **strong**, *emphasis*, ~~old~~, `code`, and [docs](https://example.com).\n\n> quote\n\n1. first\n   - nested\n2. second\n\n```rust\nfn main() {}\n```\n\n---",
    );
    assert!(matches!(
        document.blocks[0],
        Block::Heading { level: 1, .. }
    ));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, Block::BlockQuote(_))));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, Block::List(_))));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, Block::CodeBlock(_))));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, Block::Divider)));
    let plain = document.plain_text();
    for text in [
        "Heading", "strong", "emphasis", "old", "code", "quote", "nested",
    ] {
        assert!(plain.contains(text), "missing {text:?}: {plain}");
    }
    assert!(!plain.contains("**"));
}

#[test]
fn superscript_and_subscript_stay_inline_and_unstyled() {
    // Not enabled by `parser_options`; this guards the wrapper if they are.
    let source = "x ^sup^ y ~sub~ z";
    let options = parser_options() | Options::ENABLE_SUPERSCRIPT | Options::ENABLE_SUBSCRIPT;
    let parser = Parser::new_ext(source, options).into_offset_iter();
    let document = Builder::new(source, &[]).build(parser);
    assert_eq!(document.blocks.len(), 1);
    assert_eq!(document.plain_text(), "x sup y sub z\n");
}

#[test]
fn tight_list_inline_runs_stay_in_one_paragraph() {
    let document = parse("- before **strong** `code` after");
    let Block::List(list) = &document.blocks[0] else {
        panic!("expected list");
    };
    assert_eq!(list.items[0].blocks.len(), 1);
    assert_eq!(document.plain_text(), "- before strong code after\n");
}

#[test]
fn parses_tables_tasks_autolinks_and_escaped_markers() {
    let document = parse(
        "- [x] done\n- [ ] todo\n\n| Key | Value |\n| --- | ---: |\n| a | 1 |\n\nhttps://example.com and \\*literal\\*",
    );
    let list = document
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::List(list) => Some(list),
            _ => None,
        })
        .unwrap();
    assert_eq!(list.items[0].task, Some(true));
    assert_eq!(list.items[1].task, Some(false));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, Block::Table(_))));
    let plain = document.plain_text();
    assert!(plain.contains("https://example.com"));
    assert!(plain.contains("*literal*"));
}

#[test]
fn html_like_and_malformed_markdown_remain_visible() {
    let source = "<thinking>visible</thinking>\n\n**unfinished `code [link](x";
    let plain = parse(source).plain_text();
    assert!(plain.contains("thinking"));
    assert!(plain.contains("visible"));
    assert!(plain.contains("unfinished"));
}

use super::*;

#[test]
fn keyboard_enhancement_uses_only_selective_disambiguation_flags() {
    let flags = keyboard_enhancement_flags();
    assert!(flags.contains(event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    assert!(flags.contains(event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS));
    assert!(!flags.contains(event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
}

#[test]
fn restoration_guards_are_idempotent_and_clear_their_state() {
    mark_raw_active();
    mark_keyboard_enhancement_active();
    force_restore();
    force_restore();
    assert!(!RAW_ACTIVE.load(Ordering::SeqCst));
    assert!(!KEYBOARD_ENHANCEMENT_ACTIVE.load(Ordering::SeqCst));
}

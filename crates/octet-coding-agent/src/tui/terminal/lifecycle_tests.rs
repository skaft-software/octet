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
    let _keyboard_mode = lock_keyboard_mode();
    mark_raw_active();
    mark_keyboard_enhancement_active();
    mark_focus_reporting_active();
    mark_remote_keyboard_enhancement_active(); // primary
    mark_alt_keyboard_enhancement_active();
    // Ending a lease started on the primary screen must not pop the new
    // alternate screen's own keyboard stack.
    assert!(!take_remote_keyboard_enhancement_active());
    assert!(REMOTE_KEYBOARD_ON_PRIMARY.load(Ordering::SeqCst));
    mark_remote_keyboard_enhancement_active(); // alternate
    assert!(take_remote_keyboard_enhancement_active());
    assert!(REMOTE_KEYBOARD_ON_PRIMARY.load(Ordering::SeqCst));
    mark_remote_keyboard_enhancement_active();
    force_restore();
    force_restore();
    assert!(!RAW_ACTIVE.load(Ordering::SeqCst));
    assert!(!KEYBOARD_ENHANCEMENT_ACTIVE.load(Ordering::SeqCst));
    assert!(!ALT_KEYBOARD_ENHANCEMENT_ACTIVE.load(Ordering::SeqCst));
    assert!(!REMOTE_KEYBOARD_ON_PRIMARY.load(Ordering::SeqCst));
    assert!(!REMOTE_KEYBOARD_ON_ALT.load(Ordering::SeqCst));
    assert!(!FOCUS_REPORTING_ACTIVE.load(Ordering::SeqCst));
}

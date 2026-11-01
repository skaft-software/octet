#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatus {
    Idle,
    Working,
    Blocked(BlockedKind),
    Done,
    Error,
}

impl ProgramStatus {
    pub fn sequence(self, app: &str) -> Option<String> {
        if app.is_empty()
            || app.len() > 32
            || !app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        {
            return None;
        }
        let state = match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Error => "error",
            Self::Blocked(BlockedKind::Permission) => "blocked:kind=permission",
            Self::Blocked(BlockedKind::Question) => "blocked:kind=question",
            Self::Blocked(BlockedKind::Auth) => "blocked:kind=auth",
        };
        Some(format!("\x1b]7501;state={state}:app={app}\x1b\\"))
    }

    pub fn tern_progress_sequence(self) -> &'static str {
        match self {
            Self::Working => "\x1b]9;4;3\x1b\\",
            Self::Blocked(_) => "\x1b]9;4;4\x1b\\",
            Self::Error => "\x1b]9;4;2\x1b\\",
            Self::Idle | Self::Done => "\x1b]9;4;0\x1b\\",
        }
    }

    pub fn is_transient(self) -> bool {
        matches!(self, Self::Working | Self::Blocked(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wire_and_no_private_text_or_fabricated_progress() {
        for (status, body) in [
            (ProgramStatus::Idle, "idle"),
            (ProgramStatus::Working, "working"),
            (ProgramStatus::Done, "done"),
            (ProgramStatus::Error, "error"),
            (
                ProgramStatus::Blocked(BlockedKind::Permission),
                "blocked:kind=permission",
            ),
            (
                ProgramStatus::Blocked(BlockedKind::Question),
                "blocked:kind=question",
            ),
            (
                ProgramStatus::Blocked(BlockedKind::Auth),
                "blocked:kind=auth",
            ),
        ] {
            let wire = status.sequence("octet").unwrap();
            assert_eq!(wire, format!("\x1b]7501;state={body}:app=octet\x1b\\"));
            assert!(wire.len() < 4096);
            assert!(!wire.contains("progress="));
            assert!(!wire.contains("title="));
            assert!(!wire.contains("msg="));
        }
        assert_eq!(
            ProgramStatus::Working.tern_progress_sequence(),
            "\x1b]9;4;3\x1b\\"
        );
    }

    #[test]
    fn identity_is_bounded_and_cannot_inject_pairs_or_controls() {
        for app in [
            "",
            "bad:state=done",
            "bad;",
            "\x07",
            "\x1b",
            "\u{85}",
            "🦀",
            &"x".repeat(33),
        ] {
            assert!(ProgramStatus::Idle.sequence(app).is_none(), "{app:?}");
        }
        assert!(ProgramStatus::Idle.sequence(&"x".repeat(32)).is_some());
        assert!(ProgramStatus::Idle.sequence("app-1.2_+test").is_some());
    }
}

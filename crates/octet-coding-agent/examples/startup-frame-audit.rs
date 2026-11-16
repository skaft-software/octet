//! Inspect every stdout syscall captured by scripts/bench-startup-handoff.py.
//! This models terminal cells, not emulator title-bar paint or sync support.

use serde::Deserialize;

#[derive(Deserialize)]
struct Write {
    ms: f64,
    data: Vec<u8>,
}

fn audit(writes: &[Write], allow_initializing: bool) -> anyhow::Result<usize> {
    let mut parser = vt100::Parser::new(24, 80, 0);
    let mut entered = false;
    let mut ready = false;
    let mut paints = 0;
    for write in writes {
        parser.process(&write.data);
        // The backend emits complete bounded startup frames in one syscall.
        // Inspect *each* physical write, not merely the final combined screen.
        let visual = write.data.windows(4).any(|s| s == b"\x1b[2J")
            || write.data.windows(8).any(|s| s == b"\x1b[?1049h");
        let begin = write.data.windows(8).any(|s| s == b"\x1b[?2026h");
        let end = write.data.windows(8).any(|s| s == b"\x1b[?2026l");
        if !entered && visual && !begin {
            anyhow::bail!(
                "{}ms: screen cleared/entered before the first transaction",
                write.ms
            );
        }
        if begin {
            entered = true;
            paints += 1;
            anyhow::ensure!(end, "{}ms: incomplete startup write", write.ms);
            let text = parser.screen().contents();
            let resolved = text.contains("startup-probe");
            anyhow::ensure!(
                resolved
                    || (allow_initializing
                        && !ready
                        && text.contains('›')
                        && !text.contains("octet")),
                "{}ms: unresolved/partial frame: {text:?}",
                write.ms
            );
            anyhow::ensure!(
                !parser.screen().hide_cursor(),
                "{}ms: editable cursor hidden",
                write.ms
            );
            ready |= resolved;
        }
        // The first edit proves application input handling, and ends the
        // startup audit before exit deliberately restores the primary screen.
        if ready && parser.screen().contents().contains("edit-probe") {
            return Ok(paints);
        }
    }
    anyhow::bail!("capture ended before a ready frame and application-handled edit")
}

fn main() -> anyhow::Result<()> {
    let mut allow_initializing = false;
    for path in std::env::args().skip(1) {
        if path == "--allow-initializing" {
            allow_initializing = true;
            continue;
        }
        let writes: Vec<Write> = serde_json::from_slice(&std::fs::read(&path)?)?;
        println!(
            "{path}: PASS {} complete frames through edit",
            audit(&writes, allow_initializing)?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_unsynchronized_clear_and_empty_first_frame() {
        for data in [b"\x1b[2J".as_slice(), b"\x1b[?2026h\x1b[2J\x1b[?2026l"] {
            assert!(audit(
                &[Write {
                    ms: 1.0,
                    data: data.to_vec()
                }],
                false
            )
            .is_err());
        }
    }

    #[test]
    fn accepts_complete_first_frame_and_edit() {
        let writes = [
            Write {
                ms: 1.0,
                data: b"\x1b[?2026h\x1b[2J\x1b[Hstartup-probe\x1b[?25h\x1b[?2026l".to_vec(),
            },
            Write {
                ms: 2.0,
                data: b"\x1b[?2026h\x1b[2;1Hedit-probe\x1b[?2026l".to_vec(),
            },
        ];
        assert_eq!(audit(&writes, false).unwrap(), 2);
    }
}

//! Terminal restoration regressions for screen-local keyboard ownership.

use super::*;

#[test]
fn cleanup_leaves_alternate_screen_and_all_requested_modes() -> io::Result<()> {
    let mut bytes = Vec::new();
    cleanup(&mut bytes, false)?;
    assert_eq!(bytes, LEAVE_SCREEN);
    assert!(!String::from_utf8_lossy(ENTER_SCREEN).contains(";r"));
    Ok(())
}

#[test]
fn keyboard_push_and_pop_preserve_the_parent_screen_stack() -> io::Result<()> {
    let restoration = AtomicU8::new(INACTIVE);
    let mut bytes = Vec::new();
    enter_screen(&mut bytes, &restoration, true)?;
    cleanup_owned(&mut bytes, &restoration)?;
    let mut alternate = false;
    let mut main_stack = vec![1];
    let mut alternate_stack = vec![0];
    for command in bytes.split(|byte| *byte == 0x1b) {
        match command {
            b"[?1049h" => alternate = true,
            b"[?1049l" => alternate = false,
            b"[>5u" | b"[<1u" => {
                let stack = if alternate {
                    &mut alternate_stack
                } else {
                    &mut main_stack
                };
                if command == b"[>5u" {
                    stack.push(5);
                } else {
                    assert!(
                        stack.len() > 1,
                        "keyboard pop must own a push on this screen"
                    );
                    assert_eq!(stack.pop(), Some(5));
                }
            }
            _ => {}
        }
    }
    assert!(!alternate);
    assert_eq!(main_stack, vec![1]);
    assert_eq!(alternate_stack, vec![0]);
    assert!(bytes.windows(5).any(|window| window == b"\x1b[>5u"));
    // Panic restoration followed by guard drop must not pop a second time.
    let restored_length = bytes.len();
    cleanup_owned(&mut bytes, &restoration)?;
    assert_eq!(bytes.len(), restored_length);
    Ok(())
}

#[test]
fn admission_without_screen_ownership_emits_no_cleanup() -> io::Result<()> {
    let restoration = AtomicU8::new(INACTIVE);
    let mut bytes = Vec::new();
    cleanup_owned(&mut bytes, &restoration)?;
    assert!(bytes.is_empty());
    Ok(())
}

struct FailedScreenFlush(Vec<u8>);

impl io::Write for FailedScreenFlush {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("screen switch flush failed"))
    }
}

#[test]
fn failed_screen_admission_never_pops_the_parent_keyboard_stack() -> io::Result<()> {
    let restoration = AtomicU8::new(INACTIVE);
    let mut writer = FailedScreenFlush(Vec::new());
    assert!(enter_screen(&mut writer, &restoration, true).is_err());
    let mut cleanup_bytes = Vec::new();
    cleanup_owned(&mut cleanup_bytes, &restoration)?;
    assert_eq!(cleanup_bytes, LEAVE_SCREEN);
    assert_eq!(writer.0, ENTER_SCREEN);
    Ok(())
}

//! Interactive demo of the DECSTBM scroll region architecture.
//!
//! Run with: cargo run -p norn-tui --example demo
//!
//! Press Enter to add lines. Watch the fixed panel stay pinned at the
//! bottom while content scrolls within the scroll region above it.
//! Press q or Ctrl+C to exit.

use std::io::Write as _;
use std::time::Duration;

use termina::{Event, PlatformTerminal, Terminal};

fn main() {
    if let Err(e) = run() {
        eprintln!("demo error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = PlatformTerminal::new()?;
    terminal.enter_raw_mode()?;
    terminal.set_panic_hook(|handle| {
        let _ = write!(handle, "\x1b[r\x1b[?25h\x1b[?7h");
        let _ = handle.flush();
    });

    let dims = terminal.get_dimensions()?;
    let rows = dims.rows;
    let cols = dims.cols;
    let panel_rows: u16 = 3;
    let scroll_bottom = rows.saturating_sub(panel_rows);

    // Clear screen, set DECSTBM, position cursor at top
    write!(terminal, "\x1b[2J\x1b[1;{scroll_bottom}r\x1b[1;1H")?;
    terminal.flush()?;

    // Draw the fixed panel
    draw_panel(&mut terminal, scroll_bottom, cols, 0)?;

    // Position cursor in the scroll region for writing content
    write!(terminal, "\x1b[1;1H")?;

    // Write a few initial lines
    let mut line_count: u32 = 0;
    for _ in 0..5 {
        line_count += 1;
        write!(
            terminal,
            "\x1b[32m{line_count:>4}\x1b[0m  Content line in the scroll region\r\n"
        )?;
    }
    terminal.flush()?;

    // Redraw panel (cursor-addressed, doesn't affect scroll region)
    draw_panel(&mut terminal, scroll_bottom, cols, line_count)?;

    // Position cursor at the end of scroll region content for next writes
    write!(
        terminal,
        "\x1b[{};1H",
        line_count.min(u32::from(scroll_bottom)) + 1
    )?;
    terminal.flush()?;

    loop {
        if terminal.poll(
            |e| matches!(e, Event::Key(_)),
            Some(Duration::from_millis(50)),
        )? {
            let event = terminal.read(|e| matches!(e, Event::Key(_)))?;
            if let Event::Key(key) = event {
                use termina::event::{KeyCode, KeyEventKind, Modifiers};
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(Modifiers::CONTROL) => break,
                    KeyCode::Char('q') => break,
                    KeyCode::Enter => {
                        // Add 3 new lines. Once the scroll region fills,
                        // older lines scroll off the top into native
                        // scrollback while the panel stays pinned.
                        for _ in 0..3 {
                            line_count += 1;
                            write!(
                                terminal,
                                "\x1b[32m{line_count:>4}\x1b[0m  Content line in the scroll region\r\n"
                            )?;
                        }
                        terminal.flush()?;

                        // Redraw the panel to update the line count
                        draw_panel(&mut terminal, scroll_bottom, cols, line_count)?;
                    }
                    _ => {}
                }
            }
        }
    }

    // Cleanup
    write!(terminal, "\x1b[r\x1b[?25h\x1b[?7h")?;
    terminal.flush()?;
    terminal.enter_cooked_mode()?;
    println!("\nDemo exited cleanly. Terminal restored.");
    Ok(())
}

fn draw_panel(
    terminal: &mut PlatformTerminal,
    scroll_bottom: u16,
    cols: u16,
    line_count: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let indicator_row = scroll_bottom + 1;
    let input_row = scroll_bottom + 2;
    let status_row = scroll_bottom + 3;

    // Save cursor so we can restore after drawing the panel
    write!(terminal, "\x1b7")?;

    // Streaming indicator
    write!(terminal, "\x1b[{indicator_row};1H\x1b[2K")?;
    write!(terminal, "\x1b[33m● generating... 0s\x1b[0m")?;

    // Input placeholder
    write!(terminal, "\x1b[{input_row};1H\x1b[2K")?;
    write!(
        terminal,
        "\x1b[36m>\x1b[0m Press Enter to add lines, q to quit"
    )?;

    // Status bar (dim, full width)
    write!(terminal, "\x1b[{status_row};1H\x1b[2K")?;
    let left = format!("claude-opus-4  demo  ({line_count} lines)");
    let right = "q quit  Enter add lines";
    let gap = usize::from(cols).saturating_sub(left.len() + right.len());
    write!(terminal, "\x1b[2m{left}{}{right}\x1b[0m", " ".repeat(gap))?;

    // Restore cursor to where it was in the scroll region
    write!(terminal, "\x1b8")?;
    terminal.flush()?;
    Ok(())
}

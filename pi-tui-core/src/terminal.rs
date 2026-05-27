//! Terminal abstraction for raw I/O.
//! Mirrors packages/tui/src/terminal.ts (Terminal interface, ProcessTerminal class)

use std::io::{self, Write};

use crossterm::terminal;

/// Terminal abstraction for raw I/O using crossterm.
///
/// Wraps terminal size detection, raw mode, cursor visibility control,
/// and raw output writing.
pub struct Terminal {
    columns: u16,
    rows: u16,
}

impl Terminal {
    /// Create a new `Terminal`, detecting the current terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal size cannot be determined.
    pub fn new() -> io::Result<Self> {
        let (columns, rows) = terminal::size()?;
        Ok(Self { columns, rows })
    }

    /// Enable raw mode and refresh the detected terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if raw mode cannot be enabled or size cannot be read.
    pub fn start(&mut self) -> io::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        let (cols, rows) = terminal::size()?;
        self.columns = cols;
        self.rows = rows;
        Ok(())
    }

    /// Disable raw mode and restore the terminal to its previous state.
    ///
    /// # Errors
    ///
    /// Returns an error if raw mode cannot be disabled.
    pub fn stop(&mut self) -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Write raw string data to stdout and flush.
    ///
    /// # Errors
    ///
    /// Returns an error if writing or flushing fails.
    pub fn write(&self, data: &str) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle.write_all(data.as_bytes())?;
        handle.flush()?;
        Ok(())
    }

    /// Return the terminal width in columns.
    pub fn columns(&self) -> u16 {
        self.columns
    }

    /// Return the terminal height in rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Hide the cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if the ANSI escape cannot be written.
    pub fn hide_cursor(&self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Hide)?;
        Ok(())
    }

    /// Show the cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if the ANSI escape cannot be written.
    pub fn show_cursor(&self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Show)?;
        Ok(())
    }

    /// Clear the current line (from cursor to end of line).
    ///
    /// # Errors
    ///
    /// Returns an error if the ANSI escape cannot be written.
    pub fn clear_line(&self) -> io::Result<()> {
        crossterm::execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_size_detection() {
        let term = Terminal::new().expect("terminal size should be detectable");
        assert!(term.columns() > 0, "columns must be > 0");
        assert!(term.rows() > 0, "rows must be > 0");
    }

    #[test]
    fn test_write_does_not_panic() {
        let term = Terminal::new().expect("terminal size should be detectable");
        // Writing to stdout in tests is OK; at minimum this should not crash.
        let result = term.write("hello from pi-tui-core\n");
        assert!(result.is_ok());
    }

    #[test]
    fn test_terminal_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let term = Terminal::new().unwrap();
        assert_send(&term);
    }
}

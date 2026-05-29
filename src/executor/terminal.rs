use std::io::{self, Write};

use console::{style, Term};

// Column at which the trailing status word ("done"/"failed") is printed. Tracks
// the terminal width when stderr is a TTY, otherwise falls back to 80. Space is
// reserved on the right for the " done (NNNNN.NNN ms)" suffix, and the result is
// clamped so the dot-leader is neither cramped nor absurdly long.
fn leader_width() -> u16 {
    const DEFAULT: u16 = 80;
    const RESERVE: u16 = 22;
    match Term::stderr().size_checked() {
        Some((_rows, cols)) => cols.saturating_sub(RESERVE).clamp(20, DEFAULT),
        None => DEFAULT,
    }
}

// Progress/status output goes to stderr so that stdout carries only the
// benchmark results (tables or --format json/csv), keeping the tool pipeable.
pub fn start_msg(command: &str, message: &str) {
    let length :u16 = leader_width();
    let msg_length :u16 = (command.len() + 1 + message.len()).try_into().unwrap();
    let m = format!("{} {} ", style(command).bold().dim(), message);
    io::stderr().write_all(m.as_bytes()).unwrap();
    for _ in msg_length..=length {
        io::stderr().write_all(".".as_bytes()).unwrap();
    }
    io::stderr().write_all(" ".as_bytes()).unwrap();
    io::stderr().flush().unwrap();
}

pub fn done_msg(duration_ms: f64) {
    let m_done = format!("{} ({:.3} ms)\n", style("done").green(), duration_ms);
    io::stderr().write_all(m_done.as_bytes()).unwrap();
    io::stderr().flush().unwrap();
}

pub fn err_msg(error: &str) {
    let m_err = format!("{}\n", style("failed").red());
    let error = format!("{}\n", style(error).red());
    io::stderr().write_all(m_err.as_bytes()).unwrap();
    io::stderr().write_all(error.as_bytes()).unwrap();
    io::stderr().flush().unwrap();
}

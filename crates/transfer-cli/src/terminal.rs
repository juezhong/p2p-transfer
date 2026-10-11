//! Single owner of interactive terminal output on Unix.
//! The editor, background progress and task notices share one mutex, so a
//! status repaint cannot erase a partially typed command. No TTY -> ordinary
//! line-oriented output (CI, pipes and Windows fallback).
use std::{
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex, OnceLock},
};
#[cfg(unix)]
use std::{io::Read, time::Duration};

#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc::UnboundedSender;
use crate::completion::CompletionQuery;

#[derive(Default)]
struct Screen {
    prompt: String,
    input: String,
    cursor: usize, // UTF-8 byte boundary
    active: bool,
    status_row: bool,
    progress: Option<String>,
    history: Vec<String>,
    history_index: Option<usize>,
}

static SCREEN: OnceLock<Arc<Mutex<Screen>>> = OnceLock::new();

fn shared() -> &'static Arc<Mutex<Screen>> {
    SCREEN.get_or_init(|| Arc::new(Mutex::new(Screen::default())))
}

#[cfg(unix)]
fn width(text: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}
#[cfg(not(unix))]
fn width(text: &str) -> usize { text.chars().count() }

fn redraw(screen: &mut Screen) {
    if !screen.active { return; }
    let mut out = io::stdout().lock();
    if screen.status_row {
        let status = screen.progress.as_deref().unwrap_or("");
        let _ = write!(out, "\r\x1b[1A\r\x1b[2K{status}\r\n\x1b[2K");
    } else if let Some(status) = &screen.progress {
        screen.status_row = true;
        let _ = write!(out, "\r\x1b[2K{status}\r\n\x1b[2K");
    } else {
        let _ = write!(out, "\r\x1b[2K");
    }
    let _ = write!(out, "{}{}", screen.prompt, screen.input);
    let tail = width(&screen.input[screen.cursor..]);
    if tail != 0 { let _ = write!(out, "\x1b[{tail}D"); }
    let _ = out.flush();
}

pub fn progress(text: Option<String>) {
    if !io::stdout().is_terminal() {
        if let Some(line) = text { std::println!("{line}"); }
        return;
    }
    if let Ok(mut screen) = shared().lock() {
        screen.progress = text;
        redraw(&mut screen);
    }
}

/// A task log is printed above the command, then the command and the progress
/// line are restored. This also guards against messages racing a UTF-8 edit.
pub fn log(text: String) {
    if !io::stdout().is_terminal() {
        std::println!("{text}");
        return;
    }
    if let Ok(mut screen) = shared().lock() {
        if screen.active {
            let mut out = io::stdout().lock();
            if screen.status_row {
                let _ = write!(out, "\r\x1b[1A\r\x1b[2K\r\n\x1b[2K");
            } else {
                let _ = write!(out, "\r\x1b[2K");
            }
            let _ = writeln!(out, "{text}");
            let _ = out.flush();
            drop(out);
            // We are now on a fresh empty line; recreate the two-row layout.
            screen.status_row = false;
            redraw(&mut screen);
        } else {
            std::println!("{text}");
        }
    } else {
        std::println!("{text}");
    }
}

pub fn error(text: String) {
    if io::stdout().is_terminal() { log(text); }
    else { std::eprintln!("{text}"); }
}

#[cfg(unix)]
struct RawMode {
    fd: i32,
    original: libc::termios,
}
#[cfg(unix)]
impl RawMode {
    fn begin() -> io::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: POSIX tcgetattr writes the termios struct for a valid fd.
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: initialized by successful tcgetattr above.
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        // SAFETY: cfmakeraw and tcsetattr only mutate a local termios value
        // and the controlling terminal associated with this fd.
        unsafe {
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { fd, original })
    }
}
#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: original state came from tcgetattr on this same fd.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original); }
    }
}

#[cfg(unix)]
fn update_line(f: impl FnOnce(&mut Screen)) {
    if let Ok(mut state) = shared().lock() {
        f(&mut state);
        redraw(&mut state);
    }
}

#[cfg(unix)]
fn insert_text(state: &mut Screen, text: &str) {
    state.input.insert_str(state.cursor, text);
    state.cursor += text.len();
}

#[cfg(unix)]
fn history_move(up: bool) {
    update_line(|s| {
        let size = s.history.len();
        if size == 0 { return; }
        s.history_index = if up {
            Some(s.history_index.unwrap_or(size).saturating_sub(1))
        } else {
            s.history_index.and_then(|index| (index + 1 < size).then_some(index + 1))
        };
        s.input = s.history_index.map(|index| s.history[index].clone())
            .unwrap_or_default();
        s.cursor = s.input.len();
    });
}

/// True when this process owns an interactive Unix TTY and can draw a
/// persistent progress row above the editable input line.
#[cfg(unix)]
pub fn supported() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}
#[cfg(not(unix))]
pub fn supported() -> bool { false }

#[cfg(unix)]
pub fn read_line(prompt: &str, requests: &UnboundedSender<CompletionQuery>) -> io::Result<String> {
    let _raw = RawMode::begin()?;
    update_line(|s| {
        s.prompt = prompt.to_owned();
        s.input.clear();
        s.cursor = 0;
        s.active = true;
        s.status_row = false;
        s.history_index = None;
    });
    let mut stdin = io::stdin().lock();
    let mut byte = [0_u8; 1];
    let result = loop {
        stdin.read_exact(&mut byte)?;
        match byte[0] {
            b'\r' | b'\n' => {
                let mut s = shared().lock().map_err(|_| io::Error::other("editor poisoned"))?;
                let text = s.input.clone();
                if !text.trim().is_empty() { s.history.push(text.clone()); }
                s.active = false;
                s.status_row = false;
                s.progress = None;
                let _ = write!(io::stdout(), "\r\n");
                let _ = io::stdout().flush();
                break text;
            }
            3 => { // Ctrl-C in raw mode: cancel one task, keep the Control link.
                let mut s = shared().lock().map_err(|_| io::Error::other("editor poisoned"))?;
                s.active = false;
                s.status_row = false;
                s.progress = None;
                let _ = write!(io::stdout(), "\r\n");
                let _ = io::stdout().flush();
                break "cancel".to_owned();
            }
            4 => { // Ctrl-D
                let empty = shared().lock().is_ok_and(|s| s.input.is_empty());
                if empty {
                    update_line(|s| { s.active = false; s.status_row = false; });
                    break "quit".to_owned();
                }
            }
            8 | 127 => update_line(|s| {
                if s.cursor > 0 {
                    let start = s.input[..s.cursor].char_indices().last()
                        .map_or(0, |(i, _)| i);
                    s.input.drain(start..s.cursor);
                    s.cursor = start;
                }
            }),
            b'\t' => {
                let (line, pos) = {
                    let guard = shared().lock().map_err(|_| io::Error::other("editor poisoned"))?;
                    (guard.input.clone(), guard.cursor)
                };
                let (tx, rx) = std::sync::mpsc::channel();
                if requests.send(CompletionQuery { line, pos, reply: tx }).is_ok() {
                    if let Ok((start, pairs)) = rx.recv_timeout(Duration::from_secs(4)) {
                        if pairs.len() == 1 {
                            update_line(|s| {
                                if start <= s.cursor && s.input.is_char_boundary(start) {
                                    s.input.replace_range(start..s.cursor, &pairs[0].replacement);
                                    s.cursor = start + pairs[0].replacement.len();
                                }
                            });
                        } else if pairs.len() > 1 {
                            log(format!("可选路径：{}", pairs.iter().take(12)
                                .map(|pair| pair.display.as_str()).collect::<Vec<_>>().join("  ")));
                        }
                    }
                }
            }
            27 => {
                let mut sequence = [0_u8; 2];
                stdin.read_exact(&mut sequence)?;
                if sequence[0] == b'[' {
                    match sequence[1] {
                        b'A' => history_move(true),
                        b'B' => history_move(false),
                        b'C' => update_line(|s| {
                            if let Some(ch) = s.input[s.cursor..].chars().next() {
                                s.cursor += ch.len_utf8();
                            }
                        }),
                        b'D' => update_line(|s| {
                            if s.cursor != 0 {
                                s.cursor = s.input[..s.cursor].char_indices()
                                    .last().map_or(0, |(i, _)| i);
                            }
                        }),
                        _ => {}
                    }
                }
            }
            value if value < 32 => {},
            b => {
                let len = if b < 0x80 { 1 } else if b & 0xE0 == 0xC0 { 2 }
                    else if b & 0xF0 == 0xE0 { 3 } else if b & 0xF8 == 0xF0 { 4 }
                    else { 1 };
                let mut bytes = [0_u8; 4];
                bytes[0] = b;
                if len > 1 { stdin.read_exact(&mut bytes[1..len])?; }
                if let Ok(text) = std::str::from_utf8(&bytes[..len]) {
                    update_line(|s| insert_text(s, text));
                }
            }
        }
    };
    Ok(result)
}

#[cfg(not(unix))]
pub fn read_line(_prompt: &str, _requests: &UnboundedSender<CompletionQuery>) -> io::Result<String> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no Unix raw terminal"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn unicode_display_width_and_cursor_boundaries() {
        assert_eq!(width("中文a"), 5);
        let mut screen = Screen::default();
        #[cfg(unix)]
        {
            insert_text(&mut screen, "中文 abc");
            assert_eq!(screen.cursor, screen.input.len());
            let i = screen.input[..screen.cursor].char_indices()
                .last().map_or(0, |(i, _)| i);
            screen.input.drain(i..screen.cursor);
            assert_eq!(screen.input, "中文 ab");
        }
    }
}

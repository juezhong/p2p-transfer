//! UTF-8 path completion for the persistent shell.
//! Queries run on the async session task; the terminal reader never blocks
//! the Tokio runtime and remote listings still respect each peer's SharedRoot.

use std::{path::Path, sync::mpsc, time::Duration};

use rustyline::{
    completion::{Completer, Pair},
    error::ReadlineError,
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
    Context, Helper,
};
use tokio::sync::mpsc::UnboundedSender;
use transfer_core::{
    rpc::list_typed_in_root,
    sdk_quic::list_typed_via_sdk,
    secure_io::SharedRoot,
};

use crate::{shell::{next_id, relative_string, root_relative}, CliResult};

pub struct CompletionQuery {
    pub line: String,
    pub pos: usize,
    pub reply: mpsc::Sender<(usize, Vec<Pair>)>,
}

pub struct ShellCompleter {
    pub requests: UnboundedSender<CompletionQuery>,
}

impl Helper for ShellCompleter {}
impl Highlighter for ShellCompleter {}
impl Validator for ShellCompleter {}
impl Hinter for ShellCompleter {
    type Hint = String;
}
impl Completer for ShellCompleter {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _: &Context<'_>)
        -> Result<(usize, Vec<Pair>), ReadlineError>
    {
        let (reply, received) = mpsc::channel();
        if self.requests.send(CompletionQuery {
            line: line.to_owned(), pos, reply,
        }).is_err() {
            return Ok((pos, Vec::new()));
        }
        Ok(received.recv_timeout(Duration::from_secs(4))
            .unwrap_or((pos, Vec::new())))
    }
}

#[derive(Debug)]
struct Part {
    start: usize,
    value: String,
}

/// Parse partial shell words, including unfinished quotes, without splitting
/// UTF-8 characters or treating spaces inside a quoted filename as separators.
fn partial_words(line: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut begin = None;
    let mut value = String::new();
    let mut quote = None;
    let mut escape = false;

    for (index, ch) in line.char_indices() {
        if begin.is_none() && !ch.is_whitespace() {
            begin = Some(index);
        }
        if escape {
            value.push(ch);
            escape = false;
        } else if ch == '\\' && quote != Some('\'') {
            escape = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if ch == '"' || ch == '\'' {
            if quote.is_none() { quote = Some(ch); } else { value.push(ch); }
        } else if ch.is_whitespace() && quote.is_none() {
            if let Some(start) = begin.take() {
                parts.push(Part { start, value: std::mem::take(&mut value) });
            }
        } else {
            value.push(ch);
        }
    }
    if escape { value.push('\\'); }
    if let Some(start) = begin {
        parts.push(Part { start, value });
    } else if line.chars().last().is_some_and(char::is_whitespace) {
        parts.push(Part { start: line.len(), value: String::new() });
    }
    parts
}

fn role_for(cmd: &str, position: usize) -> Option<(bool, bool)> {
    // (remote, directory_only)
    match (cmd, position) {
        ("cd", 1) => Some((true, true)),
        ("lcd", 1) => Some((false, true)),
        ("ls", 1) => Some((true, false)),
        ("lls", 1) => Some((false, false)),
        ("get", 1) => Some((true, false)),
        ("get", 2) => Some((false, false)),
        ("put", 1) => Some((false, false)),
        ("put", 2) => Some((true, false)),
        _ => None,
    }
}

fn encode_argument(path: &str, original: &str) -> String {
    if original.starts_with('"') || original.starts_with('\'')
        || path.chars().any(char::is_whitespace)
    {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path.replace('\\', "\\\\").replace('"', "\\\"")
    }
}

pub async fn resolve(
    query: CompletionQuery,
    session: &p2p_sdk::verified_session::VerifiedManualSession,
    root: &SharedRoot,
    local_cwd: &Path,
    remote_cwd: &Path,
) {
    let result = complete(&query.line, query.pos, session, root, local_cwd, remote_cwd).await;
    let _ = query.reply.send(result);
}

async fn complete(
    line: &str,
    pos: usize,
    session: &p2p_sdk::verified_session::VerifiedManualSession,
    root: &SharedRoot,
    local_cwd: &Path,
    remote_cwd: &Path,
) -> (usize, Vec<Pair>) {
    let Some(prefix) = line.get(..pos) else { return (pos, Vec::new()) };
    let words = partial_words(prefix);
    let Some(current) = words.last() else { return (pos, Vec::new()) };
    if words.len() == 1 {
        let candidates = ["pwd", "ls", "cd", "lpwd", "lls", "lcd",
            "put", "get", "status", "cancel", "help", "quit", "exit"];
        return (current.start, candidates.into_iter()
            .filter(|s| s.starts_with(&current.value))
            .map(|s| Pair { display: s.to_owned(), replacement: s.to_owned() })
            .collect());
    }
    let cmd = words[0].value.as_str();
    let Some((remote, directory_only)) = role_for(cmd, words.len() - 1) else {
        return (current.start, Vec::new());
    };
    let (folder, fragment) = current.value.rsplit_once('/')
        .map_or(("", current.value.as_str()), |(a, b)| (a, b));
    let working = if remote { remote_cwd } else { local_cwd };
    let path = match root_relative(working, if folder.is_empty() { "." } else { folder }) {
        Ok(path) => path,
        Err(_) => return (current.start, Vec::new()),
    };
    let entries = if remote {
        match list_typed_via_sdk(session, match relative_string(&path) {
            Ok(path) => path,
            Err(_) => return (current.start, Vec::new()),
        }, next_id()).await {
            Ok(entries) => entries,
            Err(_) => return (current.start, Vec::new()),
        }
    } else {
        match local_entries(root, &path) {
            Ok(entries) => entries,
            Err(_) => return (current.start, Vec::new()),
        }
    };
    let mut choices = Vec::new();
    for entry in entries {
        if !entry.name.starts_with(fragment) || (directory_only && !entry.is_directory) {
            continue;
        }
        let mut value = if folder.is_empty() {
            entry.name.clone()
        } else {
            format!("{folder}/{}", entry.name)
        };
        if entry.is_directory { value.push('/'); }
        choices.push(Pair {
            display: value.clone(),
            replacement: encode_argument(&value, &prefix[current.start..]),
        });
    }
    choices.sort_by(|a, b| a.display.cmp(&b.display));
    (current.start, choices)
}

fn local_entries(root: &SharedRoot, path: &Path)
    -> CliResult<Vec<transfer_core::rpc::RemoteEntry>>
{
    list_typed_in_root(root, &relative_string(path)?)
        .map_err(|error| format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_chinese_quoted_paths_and_spaces() {
        let parts = partial_words("put \"中文 文件/子目");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].value, "中文 文件/子目");
        assert_eq!(parts[1].start, 4);
        let parts = partial_words("get 远端中文目录/视");
        assert_eq!(parts[1].value, "远端中文目录/视");
        let parts = partial_words("lcd ");
        assert_eq!(parts[1].value, "");
        assert_eq!(role_for("get", 1), Some((true, false)));
        assert_eq!(role_for("get", 2), Some((false, false)));
    }

    #[test]
    fn quotes_whitespace_without_corrupting_unicode() {
        assert_eq!(encode_argument("中文 目录/文件.txt", "中"), "\"中文 目录/文件.txt\"");
        assert_eq!(encode_argument("中文目录/文件.txt", "中"), "中文目录/文件.txt");
    }
}

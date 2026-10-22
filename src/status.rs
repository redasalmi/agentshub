//! Program status reports (OSC 7501), in which the CLIs say whether they are
//! working, waiting on the user or done. libghostty-vt doesn't handle them
//! yet, so they're picked out of the output before it gets there.
//! See https://superlogical.com/rex/docs/build/program-status.

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// What follows the ESC that starts a report.
const PREFIX: &[u8] = b"]7501;";

/// Reports can't be longer than this, so longer ones are dropped.
const MAX_LEN: usize = 4096;

/// The answer to a CLI asking whether reports are understood. Claude Code and
/// pi only send them once they get it.
pub const QUERY_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// At rest, waiting for the user's next instruction.
    Idle,
    Working,
    /// Can't go on until the user acts, for the reason given, if any.
    Blocked(Option<Need>),
    /// Finished, with a result the user hasn't seen yet.
    Done,
    /// Failed and stopped.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Permission,
    Question,
    Auth,
}

#[derive(Debug, PartialEq)]
pub enum Report {
    Query,
    /// `None` clears the status.
    Status(Option<Status>),
}

/// Finds the reports in output that comes in chunks, which may end partway
/// through one.
#[derive(Default)]
pub struct Scanner {
    state: State,
    body: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    /// After an ESC and this much of `PREFIX`.
    Prefix(usize),
    Body,
    /// After an ESC in the body, which ends it when a backslash follows.
    BodyEscape,
}

impl Scanner {
    /// Calls `f` with each report found and where in `bytes` it ends.
    pub fn scan(&mut self, mut bytes: &[u8], mut f: impl FnMut(Report, usize)) {
        let len = bytes.len();
        while !bytes.is_empty() {
            if let State::Ground = self.state {
                let Some(at) = bytes.iter().position(|&byte| byte == ESC) else {
                    return;
                };
                bytes = &bytes[at + 1..];
                self.state = State::Prefix(0);
                continue;
            }
            let byte = bytes[0];
            bytes = &bytes[1..];
            self.state = match self.state {
                State::Ground => unreachable!(),
                State::Prefix(matched) => self.prefix(matched, byte),
                State::Body => match byte {
                    BEL => {
                        emit(&self.body, len - bytes.len(), &mut f);
                        State::Ground
                    }
                    ESC => State::BodyEscape,
                    CAN | SUB => State::Ground,
                    _ if self.body.len() < MAX_LEN => {
                        self.body.push(byte);
                        State::Body
                    }
                    _ => State::Ground,
                },
                State::BodyEscape if byte == b'\\' => {
                    emit(&self.body, len - bytes.len(), &mut f);
                    State::Ground
                }
                // Another escape sequence cuts the report short.
                State::BodyEscape => self.prefix(0, byte),
            };
        }
    }

    fn prefix(&mut self, matched: usize, byte: u8) -> State {
        if byte == PREFIX[matched] {
            if matched + 1 < PREFIX.len() {
                State::Prefix(matched + 1)
            } else {
                self.body.clear();
                State::Body
            }
        } else if byte == ESC {
            State::Prefix(0)
        } else {
            State::Ground
        }
    }
}

fn emit(body: &[u8], end: usize, f: &mut impl FnMut(Report, usize)) {
    if let Some(report) = parse(body) {
        f(report, end);
    }
}

fn parse(body: &[u8]) -> Option<Report> {
    let body = std::str::from_utf8(body).ok()?;
    if body.trim() == "?" {
        return Some(Report::Query);
    }
    let (mut state, mut kind, mut id) = (None, None, None);
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "state" => state = Some(value),
            "kind" => kind = Some(value),
            "id" => id = Some(value),
            _ => {}
        }
    }
    // Reports with an id are about parts of the CLI's work, like one of its
    // subagents. Only the CLI as a whole is shown.
    if id.is_some() {
        return None;
    }
    let status = match state? {
        "idle" => Status::Idle,
        "working" => Status::Working,
        "blocked" => Status::Blocked(match kind {
            Some("permission") => Some(Need::Permission),
            Some("question") => Some(Need::Question),
            Some("auth") => Some(Need::Auth),
            _ => None,
        }),
        "done" => Status::Done,
        "error" => Status::Error,
        "clear" => return Some(Report::Status(None)),
        _ => return None,
    };
    Some(Report::Status(Some(status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Report> {
        let mut scanner = Scanner::default();
        let mut reports = Vec::new();
        for chunk in chunks {
            scanner.scan(chunk, |report, _| reports.push(report));
        }
        reports
    }

    #[test]
    fn finds_reports_between_other_output() {
        assert_eq!(
            scan(&[b"\x1b[1mhi\x1b]7501;state=working:app=claude-code\x1b\\there\x1b]0;title\x07"]),
            [Report::Status(Some(Status::Working))]
        );
        assert_eq!(
            scan(&[b"\x1b]7501;?\x1b\\\x1b[c\x1b]7501; state = done \x07"]),
            [Report::Query, Report::Status(Some(Status::Done))]
        );
    }

    #[test]
    fn finds_reports_split_across_chunks() {
        let report = b"\x1b]7501;state=blocked:kind=permission\x1b\\";
        for at in 0..report.len() {
            assert_eq!(
                scan(&[b"x", &report[..at], &report[at..], b"y"]),
                [Report::Status(Some(Status::Blocked(Some(
                    Need::Permission
                ))))],
                "split at {at}"
            );
        }
    }

    #[test]
    fn says_where_reports_end() {
        let mut ends = Vec::new();
        let mut scanner = Scanner::default();
        scanner.scan(b"\x1b[c\x1b]7501;?\x07\x1b[c\x1b]7501;state=", |_, end| {
            ends.push(end)
        });
        scanner.scan(b"done\x1b\\x", |_, end| ends.push(end));
        assert_eq!(ends, [12, 6]);
    }

    #[test]
    fn reads_reports_like_the_spec() {
        let status = |body: &str| parse(body.as_bytes());
        assert_eq!(
            status("state=blocked:kind=later:msg=V2FpdA=="),
            Some(Report::Status(Some(Status::Blocked(None))))
        );
        assert_eq!(
            status("state=idle:state=error:junk"),
            Some(Report::Status(Some(Status::Error)))
        );
        assert_eq!(status("state=clear"), Some(Report::Status(None)));
        assert_eq!(status("state=working:id=task/1"), None);
        assert_eq!(status("state=resting"), None);
        assert_eq!(status("app=codex"), None);
    }

    #[test]
    fn drops_cut_short_and_oversized_reports() {
        assert_eq!(
            scan(&[
                b"\x1b]7501;state=working\x18\x1b]7501;state=idle\x1b[c\x1b]7501;state=done\x07"
            ]),
            [Report::Status(Some(Status::Done))]
        );
        let long = [
            b"\x1b]7501;state=working:".as_slice(),
            &[b'a'; MAX_LEN],
            b"\x07",
        ]
        .concat();
        assert_eq!(scan(&[&long]), []);
    }
}

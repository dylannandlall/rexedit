use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use regex::bytes::Regex;

use crate::model::SearchMatch;

const BATCH_SIZE: usize = 256;
const PROGRESS_INTERVAL: usize = 1024 * 1024;

enum SearchPattern {
    Masked(Vec<Option<u8>>),
    Regex(Regex),
}

#[derive(Debug)]
pub enum SearchMessage {
    Batch(Vec<SearchMatch>),
    Progress(usize),
    Done,
    Error(String),
}

pub struct SearchWorker {
    pub receiver: Receiver<SearchMessage>,
    cancel: Arc<AtomicBool>,
}

impl SearchWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(bytes: Arc<Vec<u8>>, query: String) -> Result<SearchWorker, String> {
    let pattern = parse_query(&query)?;
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);

    thread::spawn(move || {
        let result = match pattern {
            SearchPattern::Masked(pattern) => {
                search_masked_stream(&bytes, &pattern, &sender, &worker_cancel)
            }
            SearchPattern::Regex(regex) => {
                search_regex_stream(&bytes, &regex, &sender, &worker_cancel)
            }
        };
        if let Err(error) = result {
            let _ = sender.send(SearchMessage::Error(error));
        }
    });

    Ok(SearchWorker { receiver, cancel })
}

#[cfg(test)]
pub fn search(bytes: &[u8], query: &str) -> Result<Vec<SearchMatch>, String> {
    let pattern = parse_query(query)?;
    Ok(match pattern {
        SearchPattern::Masked(pattern) => search_masked(bytes, &pattern),
        SearchPattern::Regex(regex) => regex
            .find_iter(bytes)
            .filter(|found| !found.is_empty())
            .map(|found| SearchMatch {
                start: found.start(),
                end: found.end() - 1,
            })
            .collect(),
    })
}

fn search_masked_stream(
    bytes: &[u8],
    pattern: &[Option<u8>],
    sender: &Sender<SearchMessage>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if pattern.is_empty() || pattern.len() > bytes.len() {
        sender
            .send(SearchMessage::Done)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    let mut batch = Vec::with_capacity(BATCH_SIZE);
    for (start, window) in bytes.windows(pattern.len()).enumerate() {
        if start % 16_384 == 0 && cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        if pattern
            .iter()
            .zip(window)
            .all(|(expected, actual)| expected.is_none_or(|byte| byte == *actual))
        {
            batch.push(SearchMatch {
                start,
                end: start + pattern.len() - 1,
            });
            if batch.len() == BATCH_SIZE {
                sender
                    .send(SearchMessage::Batch(std::mem::take(&mut batch)))
                    .map_err(|error| error.to_string())?;
            }
        }
        if start > 0 && start % PROGRESS_INTERVAL == 0 {
            send_batch(sender, &mut batch)?;
            sender
                .send(SearchMessage::Progress(start))
                .map_err(|error| error.to_string())?;
        }
    }
    finish_stream(sender, batch, bytes.len())
}

fn search_regex_stream(
    bytes: &[u8],
    regex: &Regex,
    sender: &Sender<SearchMessage>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut last_progress = 0;
    for found in regex.find_iter(bytes).filter(|found| !found.is_empty()) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        batch.push(SearchMatch {
            start: found.start(),
            end: found.end() - 1,
        });
        if batch.len() == BATCH_SIZE {
            sender
                .send(SearchMessage::Batch(std::mem::take(&mut batch)))
                .map_err(|error| error.to_string())?;
        }
        if found.end().saturating_sub(last_progress) >= PROGRESS_INTERVAL {
            last_progress = found.end();
            send_batch(sender, &mut batch)?;
            sender
                .send(SearchMessage::Progress(last_progress))
                .map_err(|error| error.to_string())?;
        }
    }
    finish_stream(sender, batch, bytes.len())
}

fn finish_stream(
    sender: &Sender<SearchMessage>,
    batch: Vec<SearchMatch>,
    scanned: usize,
) -> Result<(), String> {
    if !batch.is_empty() {
        sender
            .send(SearchMessage::Batch(batch))
            .map_err(|error| error.to_string())?;
    }
    sender
        .send(SearchMessage::Progress(scanned))
        .map_err(|error| error.to_string())?;
    sender
        .send(SearchMessage::Done)
        .map_err(|error| error.to_string())
}

fn send_batch(sender: &Sender<SearchMessage>, batch: &mut Vec<SearchMatch>) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    sender
        .send(SearchMessage::Batch(std::mem::take(batch)))
        .map_err(|error| error.to_string())
}

fn parse_query(query: &str) -> Result<SearchPattern, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("search query is empty".into());
    }

    if let Some(pattern) = query.strip_prefix("re:") {
        return Regex::new(pattern)
            .map(SearchPattern::Regex)
            .map_err(|error| format!("invalid regex: {error}"));
    }

    if let Some(decimal) = query.strip_prefix("dec:") {
        let value = decimal
            .trim()
            .parse::<u128>()
            .map_err(|_| "invalid unsigned decimal value".to_string())?;
        return Ok(SearchPattern::Masked(
            minimal_be_bytes(value).into_iter().map(Some).collect(),
        ));
    }

    if let Some(binary) = query.strip_prefix("bin:") {
        let bits: String = binary
            .chars()
            .filter(|character| !character.is_whitespace() && *character != '_')
            .collect();
        if bits.is_empty() || !bits.bytes().all(|byte| matches!(byte, b'0' | b'1')) {
            return Err("binary searches may only contain 0 and 1".into());
        }
        if !bits.len().is_multiple_of(8) {
            return Err("binary searches must contain a multiple of 8 bits".into());
        }
        let bytes = bits
            .as_bytes()
            .chunks(8)
            .map(|chunk| {
                u8::from_str_radix(std::str::from_utf8(chunk).expect("ASCII bits"), 2)
                    .map(Some)
                    .map_err(|_| "invalid binary byte".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(SearchPattern::Masked(bytes));
    }

    parse_hex(query.strip_prefix("hex:").unwrap_or(query)).map(SearchPattern::Masked)
}

fn parse_hex(input: &str) -> Result<Vec<Option<u8>>, String> {
    let normalized = input.replace("0x", "").replace("0X", "");
    let has_separators = normalized
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, ',' | '_' | '-'));
    let tokens: Vec<String> = if has_separators {
        normalized
            .split(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '_' | '-')
            })
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        if !normalized.len().is_multiple_of(2) {
            return Err("compact hex must contain an even number of digits".into());
        }
        normalized
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect()
    };
    if tokens.is_empty() {
        return Err("hex search is empty".into());
    }
    tokens
        .into_iter()
        .map(|token| {
            if token == "??" || token == "**" {
                Ok(None)
            } else if token.len() == 2 {
                u8::from_str_radix(&token, 16)
                    .map(Some)
                    .map_err(|_| format!("invalid hex byte: {token}"))
            } else {
                Err(format!("hex byte must have two digits: {token}"))
            }
        })
        .collect()
}

fn minimal_be_bytes(value: u128) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    bytes[first..].to_vec()
}

#[cfg(test)]
fn search_masked(bytes: &[u8], pattern: &[Option<u8>]) -> Vec<SearchMatch> {
    if pattern.is_empty() || pattern.len() > bytes.len() {
        return Vec::new();
    }
    bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, window)| {
            pattern
                .iter()
                .zip(*window)
                .all(|(expected, actual)| expected.is_none_or(|byte| byte == *actual))
        })
        .map(|(start, _)| SearchMatch {
            start,
            end: start + pattern.len() - 1,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_compact_and_spaced_hex() {
        let bytes = b"\xDE\xAD\xBE\xEF\xDE\xAD";
        assert_eq!(search(bytes, "DEAD").unwrap().len(), 2);
        assert_eq!(search(bytes, "DE AD BE EF").unwrap()[0].end, 3);
    }

    #[test]
    fn supports_hex_wildcards() {
        assert_eq!(
            search(b"\xDE\x00\xBE\xDE\x11\xBE", "DE ?? BE")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn supports_decimal_binary_and_regex() {
        let bytes = b"\x01\x00AB\xFF";
        assert_eq!(search(bytes, "dec:256").unwrap()[0].start, 0);
        assert_eq!(search(bytes, "bin:01000001 01000010").unwrap()[0].start, 2);
        assert_eq!(search(bytes, r"re:A.").unwrap()[0].end, 3);
    }

    #[test]
    fn streams_search_results_in_the_background() {
        let worker = spawn(Arc::new(b"ABxxAB".to_vec()), "41 42".into()).unwrap();
        let mut found = Vec::new();
        loop {
            match worker.receiver.recv().unwrap() {
                SearchMessage::Batch(batch) => found.extend(batch),
                SearchMessage::Done => break,
                SearchMessage::Progress(_) => {}
                SearchMessage::Error(error) => panic!("{error}"),
            }
        }
        assert_eq!(found.len(), 2);
    }
}

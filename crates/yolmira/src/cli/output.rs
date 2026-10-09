//! Human and JSON output. Errors go to stderr; JSON mode prints one object per line.
//! Human output never passes control characters through: text from the daemon must
//! not be able to drive the terminal (escape sequences).

use serde_json::{json, Value};
use yolmira_core::{Event, YolmiraError};

pub struct Out {
    pub json: bool,
}

impl Out {
    pub fn value(&self, human: impl FnOnce() -> String, value: Value) {
        if self.json {
            println!("{value}");
        } else {
            println!("{}", printable(&human(), false));
        }
    }

    pub fn event(&self, e: &Event) {
        if self.json {
            println!("{}", json!(e));
        } else {
            let line = format!(
                "{:>6}  {}  {:<18} {:<12} {}",
                e.seq, e.ts, e.kind, e.actor, e.payload
            );
            println!("{}", printable(&line, false));
        }
    }

    pub fn error(&self, e: &YolmiraError) {
        if self.json {
            println!("{}", json!({ "error": e }));
        } else {
            eprintln!("yolmira: {}", printable(&e.to_string(), true));
        }
    }
}

/// Replace control characters (ESC, BEL, CR, …) with U+FFFD; optionally keep newlines.
pub fn printable(s: &str, keep_newlines: bool) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && !(keep_newlines && c == '\n') {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_cannot_reach_the_terminal() {
        assert_eq!(
            printable("ok\u{1b}[2J\u{7}", false),
            "ok\u{FFFD}[2J\u{FFFD}"
        );
        assert_eq!(printable("a\nb", true), "a\nb");
        assert_eq!(printable("a\nb", false), "a\u{FFFD}b");
        assert_eq!(printable("Проект", false), "Проект");
    }
}

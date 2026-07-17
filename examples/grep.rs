//! A miniature grep built on the public API — dogfoods the builder's
//! line mode and the iteration API, and doubles as a manual test tool.
//!
//! ```text
//! cargo run --example grep -- [-i] [--posix] [-o] <pattern> <file>
//! ```
//!
//! Prints matching lines (line mode: `^`/`$` are line anchors, `.` stops
//! at newlines); `-o` prints each match instead, `grep -o`-style.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut icase = false;
    let mut posix = false;
    let mut only_matches = false;
    let mut rest = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-i" => icase = true,
            "--posix" => posix = true,
            "-o" => only_matches = true,
            _ => rest.push(arg),
        }
    }
    let [pattern, path] = rest.as_slice() else {
        eprintln!("usage: grep [-i] [--posix] [-o] <pattern> <file>");
        return ExitCode::from(2);
    };

    let re = match rusty_regx::Regex::builder()
        .posix(posix)
        .case_insensitive(icase)
        .newline(true)
        .build(pattern)
    {
        Ok(re) => re,
        Err(e) => {
            eprintln!("grep: invalid pattern: {e}");
            return ExitCode::from(2);
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("grep: {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let mut hit = false;
    if only_matches {
        for m in re.find_iter(&text) {
            if !m.is_empty() {
                hit = true;
                println!("{}", m.as_str());
            }
        }
    } else {
        // One search over the whole text per match, then report whole
        // lines; line mode keeps `^`/`$` meaningful.
        let mut last_line_end = 0;
        for m in re.find_iter(&text) {
            if m.start() < last_line_end {
                continue; // same line already printed
            }
            hit = true;
            let line_start = text[..m.start()].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[m.end()..]
                .find('\n')
                .map_or(text.len(), |i| m.end() + i);
            println!("{}", &text[line_start..line_end]);
            last_line_end = line_end + 1;
        }
    }
    ExitCode::from(if hit { 0 } else { 1 })
}

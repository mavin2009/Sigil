//! Human diagnostics: turn `at bytes A..B` markers into real source
//! positions with a caret snippet.
//!
//! Checks report byte spans because that is what the parser produces; the
//! person reading the error wants a line, a column, and the offending text.
//! This module is the single place that bridges the two, so every check —
//! present and future — gets good diagnostics for free.

/// 1-based line and column for a byte offset.
pub fn line_col(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn line_text(source: &str, line_no: usize) -> &str {
    source.lines().nth(line_no.saturating_sub(1)).unwrap_or("")
}

/// Rewrite every `at bytes A..B` in `msg` to `at <file>:line:col`, and append
/// a caret snippet for the first such span.
pub fn render(msg: &str, source: &str, path: &str) -> String {
    let mut out = String::new();
    let mut rest = msg;
    let mut first_span: Option<(usize, usize)> = None;

    while let Some(idx) = rest.find("at bytes ") {
        let (before, tail) = rest.split_at(idx);
        out.push_str(before);
        let after = &tail["at bytes ".len()..];
        // Parse `A..B`
        let mut it = after.splitn(2, "..");
        let a = it.next().unwrap_or("");
        let a_num: String = a.chars().take_while(|c| c.is_ascii_digit()).collect();
        let b_part = it.next().unwrap_or("");
        let b_num: String = b_part.chars().take_while(|c| c.is_ascii_digit()).collect();
        match (a_num.parse::<usize>(), b_num.parse::<usize>()) {
            (Ok(start), Ok(end)) => {
                let (l, c) = line_col(source, start);
                out.push_str(&format!("at {path}:{l}:{c}"));
                if first_span.is_none() {
                    first_span = Some((start, end));
                }
                let consumed = "at bytes ".len() + a_num.len() + 2 + b_num.len();
                rest = &tail[consumed..];
            }
            _ => {
                out.push_str("at bytes ");
                rest = after;
            }
        }
    }
    out.push_str(rest);

    if let Some((start, _end)) = first_span {
        let (l, c) = line_col(source, start);
        let text = line_text(source, l);
        let gutter = format!("{l}");
        let pad = " ".repeat(gutter.len());
        out.push_str(&format!(
            "\n\n{pad} |\n{gutter} | {}\n{pad} | {}^\n",
            text.trim_end(),
            " ".repeat(c.saturating_sub(1))
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "schema M { v: Int }\nprocess P {\n  state n: Int = 0\n}\n";

    #[test]
    fn maps_offsets_to_line_and_column() {
        assert_eq!(line_col(SRC, 0), (1, 1));
        let idx = SRC.find("process").unwrap();
        assert_eq!(line_col(SRC, idx), (2, 1));
        let idx = SRC.find("state").unwrap();
        assert_eq!(line_col(SRC, idx), (3, 3));
    }

    #[test]
    fn renders_position_and_caret() {
        let idx = SRC.find("state").unwrap();
        let msg = format!("Level-1 violation at bytes {}..{}: bad thing", idx, idx + 5);
        let rendered = render(&msg, SRC, "demo.sigil");
        assert!(rendered.contains("at demo.sigil:3:3"), "got: {rendered}");
        assert!(rendered.contains("state n: Int = 0"), "snippet missing: {rendered}");
        assert!(rendered.contains('^'), "caret missing: {rendered}");
        assert!(!rendered.contains("at bytes"), "raw offsets remain: {rendered}");
    }

    #[test]
    fn passes_through_messages_without_spans() {
        let msg = "Level-4 violation in spec 'X': ORDERING fails";
        assert_eq!(render(msg, SRC, "demo.sigil"), msg);
    }

    #[test]
    fn rewrites_every_span_in_one_message() {
        let a = SRC.find("process").unwrap();
        let b = SRC.find("state").unwrap();
        let msg = format!("first at bytes {a}..{} then at bytes {b}..{}", a + 7, b + 5);
        let r = render(&msg, SRC, "f.sigil");
        assert!(r.contains("at f.sigil:2:1") && r.contains("at f.sigil:3:3"), "got: {r}");
    }
}

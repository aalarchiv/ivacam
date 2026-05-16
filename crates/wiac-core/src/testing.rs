//! Golden-file diff harness: tolerant gcode comparator + helpers.
//!
//! Used by `cargo test` (in `tests/`) and the `wiac` CLI's verification
//! mode. Strips comments, normalizes whitespace, and matches numeric tokens
//! within `epsilon`.

#[derive(Debug, Clone)]
pub struct DiffOptions {
    pub epsilon: f64,
    pub ignore_comments: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            epsilon: 1e-3,
            ignore_comments: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffOutcome {
    Equal,
    Different {
        line: usize,
        expected: String,
        actual: String,
        reason: String,
    },
}

/// Compare two gcode programs up to `opts.epsilon` numeric tolerance.
pub fn diff_gcode(expected: &str, actual: &str, opts: &DiffOptions) -> DiffOutcome {
    let exp_lines: Vec<String> = expected
        .lines()
        .filter_map(|l| normalize_line(l, opts))
        .collect();
    let act_lines: Vec<String> = actual
        .lines()
        .filter_map(|l| normalize_line(l, opts))
        .collect();
    let max = exp_lines.len().max(act_lines.len());
    for i in 0..max {
        let exp = exp_lines.get(i).map(String::as_str).unwrap_or("");
        let act = act_lines.get(i).map(String::as_str).unwrap_or("");
        if !lines_equivalent(exp, act, opts.epsilon) {
            return DiffOutcome::Different {
                line: i + 1,
                expected: exp.to_string(),
                actual: act.to_string(),
                reason: format!(
                    "tokens differ beyond epsilon={} at line {}",
                    opts.epsilon,
                    i + 1
                ),
            };
        }
    }
    DiffOutcome::Equal
}

fn normalize_line(line: &str, opts: &DiffOptions) -> Option<String> {
    let mut s = String::with_capacity(line.len());
    let mut in_paren = false;
    for ch in line.chars() {
        if opts.ignore_comments {
            if ch == '(' {
                in_paren = true;
                continue;
            }
            if ch == ')' {
                in_paren = false;
                continue;
            }
            if ch == ';' {
                break;
            }
            if in_paren {
                continue;
            }
        }
        s.push(ch);
    }
    let trimmed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn lines_equivalent(a: &str, b: &str, epsilon: f64) -> bool {
    let ta: Vec<&str> = a.split_whitespace().collect();
    let tb: Vec<&str> = b.split_whitespace().collect();
    if ta.len() != tb.len() {
        return false;
    }
    for (x, y) in ta.iter().zip(tb.iter()) {
        if x == y {
            continue;
        }
        // If they differ, accept only if both are numeric tokens with the
        // same prefix letter and values within epsilon.
        if let (Some(xv), Some(yv)) = (parse_word(x), parse_word(y)) {
            if xv.0 == yv.0 && (xv.1 - yv.1).abs() <= epsilon {
                continue;
            }
        }
        return false;
    }
    true
}

fn parse_word(s: &str) -> Option<(char, f64)> {
    let mut chars = s.chars();
    let head = chars.next()?;
    if !head.is_ascii_alphabetic() {
        return None;
    }
    let rest: String = chars.collect();
    let val: f64 = rest.parse().ok()?;
    Some((head.to_ascii_uppercase(), val))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epsilon_tolerance_matches() {
        let a = "G1 X10.0001 Y5.0\nG1 X20 Y0\n";
        let b = "G1 X10.0 Y5.0\nG1 X20.0 Y0\n";
        let r = diff_gcode(a, b, &DiffOptions::default());
        assert_eq!(r, DiffOutcome::Equal);
    }

    #[test]
    fn beyond_epsilon_diffs() {
        let a = "G1 X10.5 Y0\n";
        let b = "G1 X10 Y0\n";
        let r = diff_gcode(a, b, &DiffOptions::default());
        assert!(matches!(r, DiffOutcome::Different { .. }));
    }

    #[test]
    fn comments_are_ignored() {
        let a = "(setup)\nG1 X1\n; trailing comment\n";
        let b = "G1 X1 ; later note\n";
        let r = diff_gcode(a, b, &DiffOptions::default());
        assert_eq!(r, DiffOutcome::Equal);
    }
}

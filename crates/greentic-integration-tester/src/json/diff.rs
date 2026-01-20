use serde_json::Value;

pub fn diff_values(left: &Value, right: &Value) -> Option<String> {
    if left == right {
        return None;
    }
    let left_text = serde_json::to_string_pretty(left).unwrap_or_else(|_| left.to_string());
    let right_text = serde_json::to_string_pretty(right).unwrap_or_else(|_| right.to_string());
    Some(render_line_diff(&left_text, &right_text))
}

fn render_line_diff(left: &str, right: &str) -> String {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let max_len = left_lines.len().max(right_lines.len());
    let mut out = String::new();
    for idx in 0..max_len {
        let l = left_lines.get(idx).copied().unwrap_or("");
        let r = right_lines.get(idx).copied().unwrap_or("");
        if l == r {
            continue;
        }
        if !l.is_empty() {
            out.push_str("- ");
            out.push_str(l);
            out.push('\n');
        }
        if !r.is_empty() {
            out.push_str("+ ");
            out.push_str(r);
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push_str("JSON values differ");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_detects_changes() {
        let left = serde_json::json!({"a": 1});
        let right = serde_json::json!({"a": 2});
        let diff = diff_values(&left, &right).unwrap();
        assert!(diff.contains("-"));
        assert!(diff.contains("+"));
    }
}

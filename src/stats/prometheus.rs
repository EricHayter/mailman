//! Reads Prometheus' text exposition format, just enough for vLLM's `/metrics`.

use std::collections::HashMap;

/// Sums each metric's samples across label sets, so a counter split by
/// `model_name` or `finished_reason` becomes one total. Histogram buckets are
/// summed too, which is meaningless, so read only their `_sum` and `_count`.
/// Lines that don't parse are skipped.
pub fn sum_by_name(text: &str) -> HashMap<String, f64> {
    let mut totals = HashMap::new();
    for line in text.lines() {
        if let Some((name, value)) = parse_sample(line) {
            *totals.entry(name.to_string()).or_default() += value;
        }
    }
    totals
}

/// `name{labels} value [timestamp]`, returning the name and value.
fn parse_sample(line: &str) -> Option<(&str, f64)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let name_end = line.find(|c: char| c == '{' || c.is_whitespace())?;
    let (name, mut rest) = line.split_at(name_end);
    if rest.starts_with('{') {
        rest = &rest[labels_len(rest)?..];
    }
    let value = rest.split_whitespace().next()?.parse().ok()?;
    Some((name, value))
}

/// Length of the `{...}` label set at the start of `text`, skipping braces
/// inside quoted label values.
fn labels_len(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '}' if !in_quotes => return Some(i + 1),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_across_labels_and_skips_comments() {
        let text = r#"
# HELP vllm:request_success_total Count of successfully processed requests.
# TYPE vllm:request_success_total counter
vllm:request_success_total{finished_reason="stop",model_name="m"} 3.0
vllm:request_success_total{finished_reason="length",model_name="m"} 2.0
vllm:num_requests_running{model_name="m"} 1.0
"#;
        let totals = sum_by_name(text);
        assert_eq!(totals["vllm:request_success_total"], 5.0);
        assert_eq!(totals["vllm:num_requests_running"], 1.0);
        assert_eq!(totals.len(), 2);
    }

    #[test]
    fn handles_no_labels_timestamps_and_braces_in_values() {
        let text = "up 1 1700000000000\nx{model_name=\"a}\\\"b\"} 2.5\n";
        let totals = sum_by_name(text);
        assert_eq!(totals["up"], 1.0);
        assert_eq!(totals["x"], 2.5);
    }

    #[test]
    fn skips_malformed_lines() {
        let totals = sum_by_name("x{unterminated 1\ny notanumber\nz 3\n");
        assert_eq!(totals.len(), 1);
        assert_eq!(totals["z"], 3.0);
    }
}

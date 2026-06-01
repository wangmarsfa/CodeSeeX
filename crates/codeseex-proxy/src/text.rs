pub(crate) fn compact_line(text: &str, max_chars: usize) -> String {
    let compacted = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = compacted.chars().count();
    if char_count <= max_chars {
        return compacted;
    }
    let prefix = compacted.chars().take(max_chars).collect::<String>();
    format!("{prefix}...[truncated chars={char_count}]")
}

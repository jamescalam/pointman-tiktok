//! Numbers and sizes as a post says them.

/// A size: 812 KB, 1.4 GB.
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..=999 => format!("{bytes} B"),
        1_000..=999_999 => format!("{:.0} KB", b / 1e3),
        1_000_000..=999_999_999 => format!("{:.1} MB", b / 1e6),
        _ => format!("{:.2} GB", b / 1e9),
    }
}

/// A list in words: a, b and c.
pub fn and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A caption's start, for a line that names it: its first `n` characters.
pub fn start(text: &str, n: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= n {
        return one_line;
    }
    format!("{}…", one_line.chars().take(n).collect::<String>().trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_them() {
        assert_eq!(size(812_000), "812 KB");
        assert_eq!(size(1_400_000_000), "1.40 GB");
        assert_eq!(and(&["comments", "duets", "stitches"]), "comments, duets and stitches");
        assert_eq!(start("Claude Code\nin 60 seconds #ai", 14), "Claude Code in…");
        assert_eq!(start("short", 14), "short");
    }
}

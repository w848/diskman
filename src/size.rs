//! Sizes and counts as people read them.

/// "512 B", "1.5 KB", "12.3 GB" (1 KB = 1024 bytes, like Explorer).
pub fn format(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "1,234,567".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 file", "2,000 files".
pub fn counted(n: u64, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{} {word}s", count(n)) }
}

/// A bar `width` cells wide, filled to `fraction`, with eighths at the end.
pub fn bar(fraction: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (fraction.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let mut out = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        out.push(PARTS[eighths % 8]);
    }
    let len = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format(0), "0 B");
        assert_eq!(format(1023), "1023 B");
        assert_eq!(format(1536), "1.5 KB");
        assert_eq!(format(250 * 1024 * 1024 * 1024), "250 GB");
        assert_eq!(format(13_207_024_435), "12.3 GB");
    }

    #[test]
    fn counts_and_bars() {
        assert_eq!(count(7), "7");
        assert_eq!(counted(1, "file"), "1 file");
        assert_eq!(counted(2000, "file"), "2,000 files");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(1.0 / 16.0, 2), "▏ ");
        assert_eq!(bar(0.25, 1), "▎");
        assert_eq!(bar(0.0, 3), "   ");
        assert_eq!(bar(2.0, 2), "██");
    }
}

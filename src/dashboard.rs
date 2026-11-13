//! Shared presentation of byte counts and elapsed time.

pub fn human_bytes(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 {
            return format!("{n:.1}{unit}");
        }
        n /= 1024.0;
    }
    format!("{n:.1}PB")
}

pub fn human_time(secs: i64) -> String {
    let (m, s) = (secs.max(0) / 60, secs.max(0) % 60);
    let (h, m) = (m / 60, m % 60);
    if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m{s:02}s") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human_bytes(512.0), "512.0B");
        assert_eq!(human_bytes(2048.0), "2.0KB");
        assert_eq!(human_time(59), "0m59s");
        assert_eq!(human_time(3720), "1h02m");
    }
}

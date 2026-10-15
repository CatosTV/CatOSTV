//! Small facts for Settings → System: free storage. (The LAN address comes
//! from [`crate::remote::local_ipv4`].)

use crate::process::{Cmd, QUICK, Runner};

/// `df -B1 --output=avail,size /`
pub fn df_cmd() -> Cmd {
    Cmd::new("df")
        .args(["-B1", "--output=avail,size", "/"])
        .timeout(QUICK)
}

/// `(available, total)` bytes from [`df_cmd`] output.
pub fn parse_df(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().nth(1)?;
    let mut it = line.split_whitespace();
    let avail = it.next()?.parse().ok()?;
    let size = it.next()?.parse().ok()?;
    Some((avail, size))
}

pub fn storage(r: &mut dyn Runner) -> Option<(u64, u64)> {
    r.run_ok(&df_cmd()).ok().and_then(|o| parse_df(&o.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_parses_bytes() {
        let text = "      Avail         1B-blocks\n38400000000 256000000000\n";
        assert_eq!(parse_df(text), Some((38_400_000_000, 256_000_000_000)));
        assert_eq!(parse_df("Avail Size\n"), None);
    }
}

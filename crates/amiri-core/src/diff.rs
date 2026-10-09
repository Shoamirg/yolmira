//! A line diff for logging config edits (small files: a plain LCS is enough).

/// `-` removed and `+` added lines, in order; unchanged lines are left out.
pub fn line_diff(old: &str, new: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    // lcs[i][j]: longest common subsequence of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] > lcs[i + 1][j]) {
            out.push(format!("+{}", b[j]));
            j += 1;
        } else {
            out.push(format!("-{}", a[i]));
            i += 1;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_what_changed() {
        let old = "[global]\n\"session.kill\" = \"ask\"\n";
        let new = "[global]\n\"session.kill\" = \"ask\"\n\"daemon.stop\" = \"allow\"\n";
        assert_eq!(line_diff(old, new), "+\"daemon.stop\" = \"allow\"");
        assert_eq!(line_diff(new, old), "-\"daemon.stop\" = \"allow\"");
        assert_eq!(line_diff("a\nb\n", "a\nc\n"), "-b\n+c");
        assert_eq!(line_diff("same", "same"), "");
    }
}

//! Terminal formatting.
//!
//! The style follows `DESIGN.md` §6.3. Square corners, monospace, no colour.
//! The terminal output and the panel must look like the same product.

/// A simple column-aligned table.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new<const N: usize>(headers: [&str; N]) -> Self {
        Table {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row<const N: usize>(&mut self, cells: [String; N]) {
        self.rows.push(cells.to_vec());
    }

    /// Prints the table to stdout, with `indent` before every line.
    pub fn print(&self, indent: &str) {
        let mut out = std::io::stdout().lock();
        let _ = self.write(&mut out, indent);
    }

    /// Writes the table anywhere.
    pub fn write(&self, out: &mut impl std::io::Write, indent: &str) -> std::io::Result<()> {
        let widths = self.widths();

        let header: Vec<String> = self
            .headers
            .iter()
            .zip(&widths)
            .map(|(h, w)| pad(h, *w))
            .collect();
        writeln!(out, "{indent}{}", header.join("  ").trim_end())?;

        for row in &self.rows {
            let cells: Vec<String> = row.iter().zip(&widths).map(|(c, w)| pad(c, *w)).collect();
            writeln!(out, "{indent}{}", cells.join("  ").trim_end())?;
        }
        Ok(())
    }

    fn widths(&self) -> Vec<usize> {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| width(h)).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(width(cell));
                }
            }
        }
        widths
    }
}

/// Box drawing, matching the notched labels in the visual reference.
pub mod box_line {
    use super::width;

    /// `┌─ LABEL ──────────┐`
    pub fn top(label: &str, total: usize) -> String {
        let head = format!("┌─ {label} ");
        let used = width(&head);
        let fill = total.saturating_sub(used + 1);
        format!("{head}{}┐", "─".repeat(fill))
    }

    /// `└──────────────────┘`
    pub fn bottom(total: usize) -> String {
        format!("└{}┘", "─".repeat(total.saturating_sub(2)))
    }

    /// A section rule with the label sitting in a gap, and no box.
    pub fn label(label: &str) -> String {
        let head = format!("  {label} ");
        let fill = 70usize.saturating_sub(width(&head));
        format!("{head}{}", "─".repeat(fill))
    }
}

/// Renders a Unix timestamp as a short relative time.
pub fn relative_time(then: i64) -> String {
    let now = crate::store::now_secs();
    let delta = now.saturating_sub(then);

    match delta {
        d if d < 0 => "in the future".into(),
        d if d < 60 => "just now".into(),
        d if d < 3_600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3_600),
        d if d < 86_400 * 30 => format!("{}d ago", d / 86_400),
        _ => "long ago".into(),
    }
}

/// The printed width of a string.
///
/// This counts characters, not grapheme clusters. A name with an emoji or a
/// combining mark may sit one column off. That is acceptable for a roster and
/// it avoids a dependency.
fn width(s: &str) -> usize {
    s.chars().count()
}

fn pad(s: &str, w: usize) -> String {
    let len = width(s);
    if len >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_reads_naturally() {
        let now = crate::store::now_secs();
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - 120), "2m ago");
        assert_eq!(relative_time(now - 7_200), "2h ago");
        assert_eq!(relative_time(now - 86_400 * 3), "3d ago");
    }

    #[test]
    fn a_box_top_is_the_width_asked_for() {
        let line = box_line::top("YOUR KEY", 40);
        assert_eq!(width(&line), 40);
        assert!(line.starts_with("┌─ YOUR KEY "));
        assert!(line.ends_with('┐'));
    }

    #[test]
    fn a_long_label_does_not_panic() {
        let line = box_line::top(&"X".repeat(80), 40);
        assert!(line.ends_with('┐'));
    }
}

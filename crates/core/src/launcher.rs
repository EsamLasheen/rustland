//! Launcher logic (pure): fuzzy ranking of application names and an inline calculator.

/// Score how well `query` (lower-case) matches `name` (lower-case). Higher is better; `None` = no match.
/// Order: exact > prefix > word-start > substring > in-order subsequence (with gap penalty).
pub fn score(name: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    if name == query {
        return Some(1000);
    }
    if name.starts_with(query) {
        return Some(900 - name.len() as i32);
    }
    let initials: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| w.chars().next())
        .collect();
    if initials.starts_with(query) {
        return Some(800 - name.len() as i32);
    }
    if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.starts_with(query))
    {
        return Some(700 - name.len() as i32);
    }
    if let Some(i) = name.find(query) {
        return Some(500 - i as i32 - name.len() as i32);
    }
    // subsequence
    let mut gaps = 0;
    let mut chars = name.chars();
    for q in query.chars() {
        let mut found = false;
        for c in chars.by_ref() {
            if c == q {
                found = true;
                break;
            }
            gaps += 1;
        }
        if !found {
            return None;
        }
    }
    Some(200 - gaps.min(150) - name.len() as i32)
}

/// Indices of `names` ranked for `query`, best first, at most `limit`.
pub fn rank(names: &[&str], query: &str, limit: usize) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(i32, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| score(n, &q).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Evaluate a simple arithmetic expression: + - * / % ^, parentheses, unary minus, decimals.
/// Returns `None` unless the text is a real expression (contains an operator) and evaluates finitely.
pub fn calc(expr: &str) -> Option<f64> {
    let s: Vec<char> = expr
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '×' | 'x' => '*',
            '÷' => '/',
            ',' => '.',
            c => c,
        })
        .collect();
    if s.is_empty()
        || !s.iter().any(|c| "+-*/%^".contains(*c))
        || !s.iter().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    let mut p = Parser {
        s: &s,
        i: 0,
        depth: 0,
    };
    let v = p.expr()?;
    (p.i == s.len() && v.is_finite()).then_some(v)
}

/// Maximum recursion depth of the calculator parser (nesting of parentheses, unary signs and `^`).
/// Deeper input is rejected instead of overflowing the stack.
const MAX_DEPTH: u32 = 64;

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    depth: u32,
}
impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    /// Run a recursive step one level deeper; `None` once the depth limit is exceeded.
    fn nested(&mut self, f: impl FnOnce(&mut Self) -> Option<f64>) -> Option<f64> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let v = f(self);
        self.depth -= 1;
        v
    }
    fn expr(&mut self) -> Option<f64> {
        self.nested(Self::expr_inner)
    }
    fn expr_inner(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(c @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == '+' { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        while let Some(c @ ('*' | '/' | '%')) = self.peek() {
            self.i += 1;
            let r = self.power()?;
            v = match c {
                '*' => v * r,
                '/' => v / r,
                _ => v % r,
            };
        }
        Some(v)
    }
    fn power(&mut self) -> Option<f64> {
        let b = self.unary()?;
        if self.peek() == Some('^') {
            self.i += 1;
            let e = self.nested(Self::power)?; // right-associative
            return Some(b.powf(e));
        }
        Some(b)
    }
    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.nested(Self::unary)?)
            }
            '+' => {
                self.i += 1;
                self.nested(Self::unary)
            }
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                (self.peek() == Some(')')).then(|| {
                    self.i += 1;
                    v
                })
            }
            _ => {
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                    self.i += 1;
                }
                self.s[start..self.i]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .ok()
            }
        }
    }
}

/// Format a calculator result without float noise.
pub fn format_number(v: f64) -> String {
    if v.fract() == 0. && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.10}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking_prefers_prefix_and_initials() {
        let names = [
            "visual studio code",
            "windows terminal",
            "code",
            "notepad",
            "task manager",
        ];
        assert_eq!(rank(&names, "code", 3)[0], 2);
        assert_eq!(rank(&names, "vsc", 3)[0], 0);
        assert_eq!(rank(&names, "wt", 3)[0], 1);
        assert_eq!(rank(&names, "term", 3)[0], 1);
        assert!(rank(&names, "zzz", 3).is_empty());
        assert_eq!(rank(&names, "tmgr", 3)[0], 4);
    }
    #[test]
    fn calculator() {
        assert_eq!(calc("2+3*4"), Some(14.));
        assert_eq!(calc("(2+3)*4"), Some(20.));
        assert_eq!(calc("2^3^2"), Some(512.));
        assert_eq!(calc("-3 + 5"), Some(2.));
        assert_eq!(calc("10 % 4"), Some(2.));
        assert_eq!(calc("1/0"), None);
        assert_eq!(calc("notepad"), None);
        assert_eq!(calc("42"), None);
        assert_eq!(calc("2+"), None);
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(12.0), "12");
    }
    #[test]
    fn calculator_depth_limit() {
        // Moderate nesting still works.
        let ok = format!("{}1+1{}", "(".repeat(20), ")".repeat(20));
        assert_eq!(calc(&ok), Some(2.));
        assert_eq!(calc(&format!("{}5", "-".repeat(10))), Some(5.));
        // Pathological input is rejected without overflowing the stack.
        let deep = format!("{}1+1{}", "(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(calc(&deep), None);
        let unbalanced = format!("1+{}", "(".repeat(100_000));
        assert_eq!(calc(&unbalanced), None);
        assert_eq!(calc(&format!("{}5", "-".repeat(100_000))), None);
        assert_eq!(calc(&format!("1+{}2", "+".repeat(100_000))), None);
        assert_eq!(calc(&format!("2{}", "^2".repeat(100_000))), None);
        assert_eq!(calc(&format!("1{}", "+-".repeat(100_000))), None);
    }
}

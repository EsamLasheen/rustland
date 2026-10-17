use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: u64,
    pub text: String,
    pub done: bool,
    pub urgent: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tasks {
    pub schema: u32,
    pub next_id: u64,
    pub items: Vec<Task>,
}
impl Default for Tasks {
    fn default() -> Self {
        Self {
            schema: 1,
            next_id: 1,
            items: vec![],
        }
    }
}
impl Tasks {
    pub fn validate(&self) -> std::io::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        if self.schema != 1
            || self.next_id == 0
            || self.items.len() > 500
            || self.items.iter().any(|t| {
                t.id == 0
                    || t.id >= self.next_id
                    || t.text.trim().is_empty()
                    || t.text.chars().count() > 2000
                    || !ids.insert(t.id)
            })
        {
            return Err(std::io::Error::other(
                "Invalid task file; original preserved",
            ));
        }
        Ok(())
    }
    pub fn add(&mut self, text: &str) -> bool {
        let text = text.trim();
        if self.items.len() >= 500
            || text.is_empty()
            || text.chars().count() > 2000
            || self.next_id == u64::MAX
        {
            return false;
        }
        self.items.push(Task {
            id: self.next_id,
            text: text.into(),
            done: false,
            urgent: false,
        });
        self.next_id += 1;
        true
    }
    pub fn ordered(&self) -> Vec<usize> {
        let mut rows: Vec<_> = (0..self.items.len()).collect();
        rows.sort_by_key(|i| {
            let t = &self.items[*i];
            (t.done, !t.urgent, t.id)
        });
        rows
    }
}
#[derive(Default)]
pub struct Clipboard {
    pub items: Vec<String>,
}
impl Clipboard {
    pub fn push(&mut self, text: String) {
        if text.trim().is_empty() || text.encode_utf16().count() > 20_000 {
            return;
        }
        self.items.retain(|s| s != &text);
        self.items.insert(0, text);
        self.items.truncate(30);
    }
    pub fn filter(&self, q: &str) -> Vec<usize> {
        let q = q.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, s)| s.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urgent_and_done_sort_without_changing_identity() {
        let mut t = Tasks::default();
        assert!(t.add("أول مهمة"));
        assert!(t.add("second"));
        assert!(t.add("urgent"));
        t.items[0].done = true;
        t.items[2].urgent = true;
        assert_eq!(t.ordered(), vec![2, 1, 0]);
        t.items.remove(1);
        assert!(t.add("next"));
        assert_eq!(t.items.last().unwrap().id, 4);
        t.validate().unwrap();
    }
    #[test]
    fn corrupt_or_future_data_is_rejected() {
        let mut t = Tasks::default();
        t.add("one");
        t.items.push(t.items[0].clone());
        assert!(t.validate().is_err());
        t.items.pop();
        t.schema = 2;
        assert!(t.validate().is_err());
    }
    #[test]
    fn clipboard_bounded_deduplicated_and_searchable() {
        let mut c = Clipboard::default();
        for n in 0..40 {
            c.push(format!("item {n}"));
        }
        assert_eq!(c.items.len(), 30);
        c.push("item 20".into());
        assert_eq!(c.items.len(), 30);
        assert_eq!(c.items[0], "item 20");
        assert_eq!(c.filter("ITEM 20"), vec![0]);
        c.push("x".repeat(20_001));
        assert_eq!(c.items[0], "item 20");
    }
}

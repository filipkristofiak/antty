#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    Command,
    Search,
}

impl CmdKind {
    pub fn prompt(self) -> char {
        match self {
            Self::Command => ':',
            Self::Search => '/',
        }
    }
}

pub struct CmdLine {
    pub kind: CmdKind,
    pub text: String,
    browse: Option<usize>,
    prefix: String,
}

pub const HISTORY_MAX: usize = 100;

impl CmdLine {
    pub fn new(kind: CmdKind) -> Self {
        Self { kind, text: String::new(), browse: None, prefix: String::new() }
    }

    pub fn push(&mut self, c: char) {
        self.text.push(c);
        self.browse = None;
    }

    pub fn backspace(&mut self) -> bool {
        self.browse = None;
        self.text.pop().is_some()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.browse = None;
    }

    pub fn delete_word(&mut self) {
        self.browse = None;
        self.text.truncate(self.text.trim_end_matches(char::is_whitespace).len());
        let Some(last) = self.text.chars().last() else { return };
        let keyword = last.is_alphanumeric() || last == '_';
        while self
            .text
            .chars()
            .last()
            .is_some_and(|c| !c.is_whitespace() && (c.is_alphanumeric() || c == '_') == keyword)
        {
            self.text.pop();
        }
    }

    pub fn older(&mut self, history: &[String]) {
        if self.browse.is_none() {
            self.prefix = self.text.clone();
        }
        let end = self.browse.unwrap_or(history.len());
        if let Some(idx) = (0..end).rev().find(|&i| history[i].starts_with(&self.prefix)) {
            self.text.clone_from(&history[idx]);
            self.browse = Some(idx);
        }
    }

    pub fn newer(&mut self, history: &[String]) {
        let Some(from) = self.browse else { return };
        if let Some(idx) = (from + 1..history.len()).find(|&i| history[i].starts_with(&self.prefix)) {
            self.text.clone_from(&history[idx]);
            self.browse = Some(idx);
        } else {
            self.text.clone_from(&self.prefix);
            self.browse = None;
        }
    }
}

pub fn record(history: &mut Vec<String>, entry: &str) {
    history.retain(|old| old != entry);
    history.push(entry.to_string());
    if history.len() > HISTORY_MAX {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_w_deletes_one_word_and_trailing_space() {
        for (input, expected) in [("foo bar  ", "foo "), ("a.b", "a."), ("", "")] {
            let mut line = CmdLine::new(CmdKind::Command);
            line.text = input.into();
            line.delete_word();
            assert_eq!(line.text, expected);
        }
    }

    #[test]
    fn history_browses_matching_prefix_and_restores_input() {
        let history = ["q", "qa", "x"].map(String::from);
        let mut line = CmdLine::new(CmdKind::Command);
        line.push('q');
        for expected in ["qa", "q", "q"] {
            line.older(&history);
            assert_eq!(line.text, expected);
        }
        line.newer(&history);
        assert_eq!(line.text, "qa");
        line.newer(&history);
        assert_eq!(line.text, "q");
        line.newer(&history);
        assert_eq!(line.text, "q");
    }

    #[test]
    fn history_deduplicates_and_caps_at_recent_entries() {
        let mut history = Vec::new();
        for i in 0..HISTORY_MAX + 1 {
            record(&mut history, &i.to_string());
        }
        assert_eq!(history.len(), HISTORY_MAX);
        assert_eq!(history.first().unwrap(), "1");
        record(&mut history, "1");
        assert_eq!(history.len(), HISTORY_MAX);
        assert_eq!(history.last().unwrap(), "1");
        assert_eq!(history.iter().filter(|entry| *entry == "1").count(), 1);
    }
}

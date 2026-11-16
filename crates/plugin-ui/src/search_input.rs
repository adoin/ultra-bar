#[derive(Default)]
pub(crate) struct SearchInput {
    current: String,
    pending: Option<String>,
    revision: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
impl SearchInput {
    pub fn dispatched(&mut self, text: String) {
        if self.current != text {
            self.revision
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.current = text;
        self.pending = None;
    }
    pub fn change(&mut self, text: String) {
        if self.current != text {
            self.revision
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.current = text.clone();
        self.pending = Some(text);
    }
    pub fn take(&mut self) -> Option<String> {
        self.pending.take()
    }
    pub fn source(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.revision.clone()
    }
    pub fn ticket(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn accepts(&self, query: Option<(&str, u64)>) -> bool {
        query.is_none_or(|(query, revision)| query == self.current && revision == self.ticket())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fast_typing_dispatches_only_the_latest_text_and_rejects_late_results() {
        let mut input = SearchInput::default();
        for text in ["i", "im", "ima", "imag", "image"] {
            input.change(text.into());
        }
        assert_eq!(input.take().as_deref(), Some("image"));
        assert!(input.take().is_none());
        let revision = input.ticket();
        assert!(!input.accepts(Some(("i", revision))));
        assert!(!input.accepts(Some(("imag", revision))));
        assert!(input.accepts(Some(("image", revision))));
        input.change("imag".into());
        input.change("image".into());
        assert!(!input.accepts(Some(("image", revision))));
        input.change(String::new());
        assert!(!input.accepts(Some(("image", revision))));
        assert_eq!(input.take(), Some(String::new()));
    }
}

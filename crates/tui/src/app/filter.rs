//! A list pane's `/` filter: which pane it narrows, the composer's words held aside while its line
//! is the filter, and the one predicate every pane's rows are narrowed by.
//!
//! The line is the composer's - one buffer, so typing, undo and the cursor are the editor's, exactly
//! as the session list's search is. What the words MEAN is `app::query`'s, so a `"quoted phrase"`
//! and a `re:` pattern narrow a pane the way they narrow the session list.

use super::*;

/// A filter that is up: the pane it narrows, and the draft its line displaced.
pub(crate) struct Filtering {
    pub(crate) surface: Surface,
    pub(crate) draft: String,
}

impl App {
    /// Whether a `/` filter is up on a list pane. The keys ask this before the composer does, so a
    /// typed line that is a query is never sent as a message.
    pub(crate) fn filtering(&self) -> bool {
        self.filtering.is_some()
    }

    /// `/` on a list: the pane is narrowed by the composer's line from here. The draft is held
    /// aside because what is typed next is a question about the list and not the operator's words.
    pub(crate) fn start_filter(&mut self) {
        let Some(s) = self.focused_surface().filter(|s| s.is_list()) else {
            return;
        };
        if self.filtering.is_some() {
            return;
        }
        let draft = self.editor.text().to_string();
        self.set_the_composer_line("");
        self.filtering = Some(Filtering { surface: s, draft });
        // The rows are about to be fewer, so the window starts at the top of the new list.
        self.pane_scroll = 0;
    }

    /// Esc while a filter is up: the rows come back whole and the draft the filter displaced comes
    /// back with them. Returns whether there was a filter, which is what makes the FIRST Esc the
    /// filter's and the second Esc the pane's.
    pub(crate) fn clear_filter(&mut self) -> bool {
        let Some(f) = self.filtering.take() else {
            return false;
        };
        self.set_the_composer_line(&f.draft);
        self.pane_scroll = 0;
        true
    }

    /// The query the filter is asking, or `None` when no filter is up - every row is kept then. It
    /// is read from the composer on every ask, so nothing has to be kept in step with the typing.
    pub(crate) fn filter_query(&self) -> Option<Query> {
        let f = self.filtering.as_ref()?;
        (self.focused_surface() == Some(f.surface)).then(|| Query::parse(self.editor.text()))
    }

    /// The rows of list `s` a filter keeps, as indices into the pane's own list, from each row's
    /// fields - and every row when nothing narrows it. One query for every pane.
    pub(crate) fn pane_filter_hits<'a>(
        &self,
        s: Surface,
        rows: impl Iterator<Item = Vec<&'a str>>,
    ) -> Vec<usize> {
        let Some(q) = self
            .filter_query()
            .filter(|_| self.filtering.as_ref().is_some_and(|f| f.surface == s))
        else {
            return (0..rows.count()).collect();
        };
        let tiers: Vec<Option<u8>> = rows.map(|fields| q.best(&fields)).collect();
        let strict = Query::strict(tiers.iter());
        tiers
            .iter()
            .enumerate()
            .filter(|(_, t)| q.keeps(**t, strict))
            .map(|(i, _)| i)
            .collect()
    }

    /// The filter's own line, over the list it narrows: the words typed, how many rows they keep,
    /// and - for a `re:` that does not compile - why it matches nothing. `None` while no filter is
    /// up, which is the same question `pane_window` asks about the room it has.
    pub(crate) fn filter_field(&self, kept: usize, w: usize) -> Option<String> {
        let q = self.filter_query()?;
        let note = match q.error() {
            Some(why) => why.to_string(),
            None if kept == 1 => "1 row".to_string(),
            None => format!("{kept} rows"),
        };
        let line = crate::ui::cards::model::query_row(self.editor.text().trim(), note, w);
        Some(crate::ui::render::row(&line, self.cfg.palette()))
    }

    /// How many rows of a list the filter's own line takes away from the pane's window: one while a
    /// filter is up on the drawn surface, and none otherwise. The click map and the window are
    /// measured against the body the filter left, so both read this one number.
    pub(crate) fn filter_rows(&self) -> usize {
        usize::from(self.filter_query().is_some())
    }
}

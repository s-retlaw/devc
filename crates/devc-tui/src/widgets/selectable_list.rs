//! Type-safe list selection widget that keeps items, selection index, and
//! TableState in sync. Replaces manual `items: Vec<T>` + `selected: usize` +
//! `table_state: TableState` triples.

use ratatui::widgets::TableState;

/// A list of items with a synced selection index and ratatui TableState.
#[derive(Debug, Clone)]
pub struct SelectableList<T> {
    items: Vec<T>,
    selected: usize,
    table_state: TableState,
}

impl<T> Default for SelectableList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SelectableList<T> {
    /// Create an empty selectable list.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            table_state: TableState::default(),
        }
    }

    /// Create from an existing Vec, selecting the first item.
    pub fn from_items(items: Vec<T>) -> Self {
        let mut table_state = TableState::default();
        if !items.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            items,
            selected: 0,
            table_state,
        }
    }

    /// Replace all items. Clamps the selection to the new length.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        if self.items.is_empty() {
            self.selected = 0;
            self.table_state.select(None);
        } else {
            self.selected = self.selected.min(self.items.len() - 1);
            self.table_state.select(Some(self.selected));
        }
    }

    /// Move selection to the next item (wraps around).
    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.table_state.select(Some(self.selected));
    }

    /// Move selection to the previous item (wraps around).
    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.items.len() - 1;
        } else {
            self.selected -= 1;
        }
        self.table_state.select(Some(self.selected));
    }

    /// Move selection to a specific index. Clamps to valid range.
    pub fn select(&mut self, index: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = index.min(self.items.len() - 1);
        self.table_state.select(Some(self.selected));
    }

    /// Get the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&T> {
        self.items.get(self.selected)
    }

    /// Get the current selection index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Get a mutable reference to the TableState (needed by render_stateful_widget).
    pub fn table_state_mut(&mut self) -> &mut TableState {
        &mut self.table_state
    }

    /// Get the items slice.
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Get a mutable reference to the items.
    pub fn items_mut(&mut self) -> &mut Vec<T> {
        &mut self.items
    }

    /// Get number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Get item at index.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.items.get(index)
    }

    /// Iterate over items.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    /// Clear all items.
    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.table_state.select(None);
    }

    /// Remove the selected item and return it. Adjusts selection.
    pub fn remove_selected(&mut self) -> Option<T> {
        if self.items.is_empty() {
            return None;
        }
        let item = self.items.remove(self.selected);
        if self.items.is_empty() {
            self.selected = 0;
            self.table_state.select(None);
        } else {
            self.selected = self.selected.min(self.items.len() - 1);
            self.table_state.select(Some(self.selected));
        }
        Some(item)
    }

    /// Find the index of the first item matching a predicate.
    pub fn find_index<F>(&self, predicate: F) -> Option<usize>
    where
        F: Fn(&T) -> bool,
    {
        self.items.iter().position(predicate)
    }

    /// Select the first item matching a predicate. Returns true if found.
    pub fn select_where<F>(&mut self, predicate: F) -> bool
    where
        F: Fn(&T) -> bool,
    {
        if let Some(idx) = self.find_index(predicate) {
            self.select(idx);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let list: SelectableList<String> = SelectableList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert_eq!(list.selected_index(), 0);
        assert!(list.selected_item().is_none());
    }

    #[test]
    fn test_from_items() {
        let list = SelectableList::from_items(vec!["a", "b", "c"]);
        assert_eq!(list.len(), 3);
        assert_eq!(list.selected_index(), 0);
        assert_eq!(list.selected_item(), Some(&"a"));
    }

    #[test]
    fn test_select_next_wraps() {
        let mut list = SelectableList::from_items(vec![1, 2, 3]);
        assert_eq!(list.selected_index(), 0);
        list.select_next();
        assert_eq!(list.selected_index(), 1);
        list.select_next();
        assert_eq!(list.selected_index(), 2);
        list.select_next(); // wraps
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_select_prev_wraps() {
        let mut list = SelectableList::from_items(vec![1, 2, 3]);
        assert_eq!(list.selected_index(), 0);
        list.select_prev(); // wraps
        assert_eq!(list.selected_index(), 2);
        list.select_prev();
        assert_eq!(list.selected_index(), 1);
    }

    #[test]
    fn test_select_next_on_empty() {
        let mut list: SelectableList<i32> = SelectableList::new();
        list.select_next(); // should not panic
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_set_items_clamps_selection() {
        let mut list = SelectableList::from_items(vec![1, 2, 3, 4, 5]);
        list.select(4); // select last item
        assert_eq!(list.selected_index(), 4);

        list.set_items(vec![10, 20]); // shrink to 2 items
        assert_eq!(list.selected_index(), 1); // clamped to last valid
        assert_eq!(list.selected_item(), Some(&20));
    }

    #[test]
    fn test_set_items_to_empty() {
        let mut list = SelectableList::from_items(vec![1, 2]);
        list.set_items(Vec::new());
        assert!(list.is_empty());
        assert_eq!(list.selected_index(), 0);
        assert!(list.selected_item().is_none());
    }

    #[test]
    fn test_select_clamped() {
        let mut list = SelectableList::from_items(vec![1, 2, 3]);
        list.select(999);
        assert_eq!(list.selected_index(), 2); // clamped to last
    }

    #[test]
    fn test_table_state_stays_in_sync() {
        let mut list = SelectableList::from_items(vec!["a", "b", "c"]);

        assert_eq!(list.table_state_mut().selected(), Some(0));

        list.select_next();
        assert_eq!(list.table_state_mut().selected(), Some(1));

        list.select(2);
        assert_eq!(list.table_state_mut().selected(), Some(2));

        list.set_items(vec!["x"]);
        assert_eq!(list.table_state_mut().selected(), Some(0));

        list.set_items(Vec::new());
        assert_eq!(list.table_state_mut().selected(), None);
    }

    #[test]
    fn test_remove_selected() {
        let mut list = SelectableList::from_items(vec![10, 20, 30]);
        list.select(1);
        let removed = list.remove_selected();
        assert_eq!(removed, Some(20));
        assert_eq!(list.len(), 2);
        assert_eq!(list.items(), &[10, 30]);
        assert_eq!(list.selected_index(), 1); // stays at 1, now pointing to 30
    }

    #[test]
    fn test_remove_selected_last_item() {
        let mut list = SelectableList::from_items(vec![10, 20, 30]);
        list.select(2);
        list.remove_selected();
        assert_eq!(list.selected_index(), 1); // clamped
    }

    #[test]
    fn test_remove_selected_empties_list() {
        let mut list = SelectableList::from_items(vec![42]);
        let removed = list.remove_selected();
        assert_eq!(removed, Some(42));
        assert!(list.is_empty());
        assert!(list.selected_item().is_none());
    }

    #[test]
    fn test_find_index() {
        let list = SelectableList::from_items(vec!["alpha", "beta", "gamma"]);
        assert_eq!(list.find_index(|s| *s == "beta"), Some(1));
        assert_eq!(list.find_index(|s| *s == "delta"), None);
    }

    #[test]
    fn test_select_where() {
        let mut list = SelectableList::from_items(vec![1, 2, 3, 4, 5]);
        assert!(list.select_where(|&x| x == 3));
        assert_eq!(list.selected_index(), 2);

        assert!(!list.select_where(|&x| x == 99));
        assert_eq!(list.selected_index(), 2); // unchanged
    }

    #[test]
    fn test_iter() {
        let list = SelectableList::from_items(vec![1, 2, 3]);
        let sum: i32 = list.iter().sum();
        assert_eq!(sum, 6);
    }

    #[test]
    fn test_clear() {
        let mut list = SelectableList::from_items(vec![1, 2, 3]);
        list.clear();
        assert!(list.is_empty());
        assert_eq!(list.selected_index(), 0);
    }
}

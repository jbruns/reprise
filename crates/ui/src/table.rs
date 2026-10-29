//! The workbench's tables: `leptos-struct-table` with virtualization, a sortable header of buttons, and a keyboard
//! cursor. Rows are fixed at 40 px, which [`ROW_HEIGHT`] and the stylesheet share.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::ops::Range;
use std::sync::Arc;

use leptos::html::Div;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_struct_table::{
    ColumnSort, EventHandler, HeadDragHandler, PaginatedTableDataProvider, TableClassesProvider,
    TableContent, TableHeadEvent, TableRow, get_sorting_for_column,
};

use crate::model::Sort;

pub const ROW_HEIGHT: f64 = 40.0;

/// A row type the workbench shows in a table.
pub trait Grid: Clone + Send + Sync + 'static {
    type Col: Copy + Eq + Ord + Debug + Send + Sync + 'static;
    const COLUMNS: &'static [Self::Col];
    fn header(col: Self::Col) -> &'static str;
    /// The class that sets the column's width and alignment.
    fn class(col: Self::Col) -> &'static str;
    fn sortable(_col: Self::Col) -> bool {
        false
    }
    fn cell(&self, col: Self::Col) -> AnyView;
}

/// Where a table's rows come from: a list, or a window onto an Audit's Entries.
pub trait RowSource<T>: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn rows(&self, range: Range<usize>) -> Vec<T>;
}

impl<T: Clone + Send + Sync> RowSource<T> for Vec<T> {
    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn rows(&self, range: Range<usize>) -> Vec<T> {
        self[range].to_vec()
    }
}

pub type Rows<T> = Arc<dyn RowSource<T>>;

pub fn rows_of<T: Clone + Send + Sync + 'static>(rows: Vec<T>) -> Rows<T> {
    Arc::new(rows)
}

/// The column a table is sorted by, from `leptos-struct-table`'s sorting state.
pub fn sort_of<C: Copy>(sorting: &VecDeque<(C, ColumnSort)>) -> Option<Sort<C>> {
    sorting.front().and_then(|(column, sort)| match sort {
        ColumnSort::Ascending => Some(Sort {
            column: *column,
            descending: false,
        }),
        ColumnSort::Descending => Some(Sort {
            column: *column,
            descending: true,
        }),
        ColumnSort::None => None,
    })
}

/// A row of a [`Grid`], as `leptos-struct-table` needs it.
#[derive(Clone)]
pub struct GridRow<T>(pub T);

#[derive(Clone, Copy)]
pub struct Classes;

impl TableClassesProvider for Classes {
    fn new() -> Self {
        Classes
    }

    fn thead_cell(&self, _sort: ColumnSort, macro_class: &str) -> String {
        macro_class.to_string()
    }

    fn row(&self, _row_index: usize, _selected: bool, prop_class: &str) -> String {
        prop_class.to_string()
    }
}

fn aria_sort(sort: ColumnSort) -> &'static str {
    match sort {
        ColumnSort::Ascending => "ascending",
        ColumnSort::Descending => "descending",
        ColumnSort::None => "none",
    }
}

impl<T: Grid> TableRow<T::Col> for GridRow<T> {
    type ClassesProvider = Classes;
    const COLUMN_COUNT: usize = T::COLUMNS.len();

    fn render_row(
        row: RwSignal<Self>,
        _index: usize,
        columns: RwSignal<Vec<T::Col>>,
    ) -> impl IntoView {
        let row = row.get_untracked();
        columns
            .get_untracked()
            .into_iter()
            .map(|col| view! { <td class=T::class(col)>{row.0.cell(col)}</td> })
            .collect_view()
    }

    fn render_head_row<F>(
        sorting: Signal<VecDeque<(T::Col, ColumnSort)>>,
        on_head_click: F,
        _drag_handlers: HeadDragHandler<T::Col>,
        columns: RwSignal<Vec<T::Col>>,
    ) -> impl IntoView
    where
        F: Fn(TableHeadEvent<T::Col>) + Send + Clone + 'static,
    {
        columns
            .get_untracked()
            .into_iter()
            .map(|col| {
                if !T::sortable(col) {
                    return view! { <th scope="col" class=T::class(col)><span class="label">{T::header(col)}</span></th> }.into_any();
                }
                let sort = move || get_sorting_for_column(col, sorting);
                let on_head_click = on_head_click.clone();
                view! {
                    <th scope="col" class=T::class(col) aria-sort=move || aria_sort(sort())>
                        <button
                            type="button"
                            class="sort"
                            on:click=move |mouse_event| on_head_click(TableHeadEvent { index: col, mouse_event })
                        >
                            {T::header(col)}
                            <span class=move || format!("sort-mark {}", sort().as_class()) aria-hidden="true"></span>
                        </button>
                    </th>
                }
                .into_any()
            })
            .collect_view()
    }

    fn cell_renderer_for_column(
        row: RwSignal<Self>,
        column: T::Col,
        class: String,
    ) -> impl IntoView {
        let row = row.get_untracked();
        view! { <td class=format!("{} {class}", T::class(column))>{row.0.cell(column)}</td> }
    }

    fn columns() -> &'static [T::Col] {
        T::COLUMNS
    }

    fn col_name(col: T::Col) -> &'static str {
        T::header(col)
    }
}

/// Feeds a table from a reactive [`RowSource`]; a new source reloads the table.
pub struct Provider<T: 'static> {
    rows: Signal<Rows<T>>,
}

// Paginated rather than ranged: the library's blanket impl over pages is the only way to implement it generically.
impl<T: Grid> PaginatedTableDataProvider<GridRow<T>, T::Col> for Provider<T> {
    const PAGE_ROW_COUNT: usize = 50;

    async fn get_page(&self, page: usize) -> Result<Vec<GridRow<T>>, String> {
        let rows = self.rows.get_untracked();
        let end = ((page + 1) * Self::PAGE_ROW_COUNT).min(rows.len());
        let start = (page * Self::PAGE_ROW_COUNT).min(end);
        Ok(rows.rows(start..end).into_iter().map(GridRow).collect())
    }

    async fn row_count(&self) -> Option<usize> {
        Some(self.rows.with_untracked(|rows| rows.len()))
    }

    fn track(&self) {
        self.rows.track();
    }
}

/// A virtualized, sortable table with a keyboard cursor: with the table focused, the arrow keys, Page Up, Page Down,
/// Home and End move the selected row, Enter opens it and Escape clears it.
#[component]
pub fn DataTable<T: Grid>(
    /// Names the table for assistive technology, and prefixes its rows' IDs.
    id: &'static str,
    label: String,
    rows: Signal<Rows<T>>,
    sorting: RwSignal<VecDeque<(T::Col, ColumnSort)>>,
    selected: RwSignal<Option<usize>>,
    /// Runs on Enter or a double click.
    on_open: Callback<T>,
    /// Shown instead of rows when there are none.
    empty: String,
) -> impl IntoView {
    let container = NodeRef::<Div>::new();
    let count = Memo::new(move |_| rows.with(|r| r.len()));

    Effect::new(move || {
        let n = count.get();
        if selected.get_untracked().is_some_and(|i| i >= n) {
            selected.set(None);
        }
    });

    let open = move |i: usize| {
        if let Some(row) = rows.with_untracked(|r| {
            if i < r.len() {
                r.rows(i..i + 1).pop()
            } else {
                None
            }
        }) {
            on_open.run(row);
        }
    };

    let scroll_to = move |i: usize| {
        let Some(el) = container.get_untracked() else {
            return;
        };
        let head = el
            .query_selector("thead")
            .ok()
            .flatten()
            .map_or(ROW_HEIGHT, |h| h.client_height() as f64);
        let view = el.client_height() as f64 - head;
        let top = i as f64 * ROW_HEIGHT;
        let scrolled = el.scroll_top() as f64;
        if top < scrolled {
            el.set_scroll_top(top as i32);
        } else if top + ROW_HEIGHT > scrolled + view {
            el.set_scroll_top((top + ROW_HEIGHT - view).ceil() as i32);
        }
    };

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.target() != ev.current_target() || ev.alt_key() || ev.ctrl_key() || ev.meta_key() {
            return;
        }
        let n = count.get_untracked();
        if n == 0 {
            return;
        }
        let page = container.get_untracked().map_or(10, |el| {
            ((el.client_height() as f64 / ROW_HEIGHT) as usize)
                .saturating_sub(2)
                .max(1)
        });
        let current = selected.get_untracked();
        let next = match ev.key().as_str() {
            "ArrowDown" => current.map_or(0, |i| (i + 1).min(n - 1)),
            "ArrowUp" => current.map_or(0, |i| i.saturating_sub(1)),
            "PageDown" => current.map_or(0, |i| (i + page).min(n - 1)),
            "PageUp" => current.map_or(0, |i| i.saturating_sub(page)),
            "Home" => 0,
            "End" => n - 1,
            "Enter" => {
                if let Some(i) = current {
                    ev.prevent_default();
                    open(i);
                }
                return;
            }
            "Escape" => {
                selected.set(None);
                return;
            }
            _ => return,
        };
        ev.prevent_default();
        selected.set(Some(next));
        scroll_to(next);
    };

    let row_renderer = move |_class: Signal<String>,
                             row: RwSignal<GridRow<T>>,
                             index: usize,
                             _selected: Signal<bool>,
                             _on_select: EventHandler<web_sys::MouseEvent>,
                             columns: RwSignal<Vec<T::Col>>| {
        let is_selected = move || selected.get() == Some(index);
        view! {
            <tr
                id=format!("{id}-r{index}")
                class:selected=is_selected
                aria-selected=move || is_selected().to_string()
                aria-rowindex=index + 2
                on:click=move |_| selected.set(Some(index))
                on:dblclick=move |_| open(index)
            >
                {GridRow::render_row(row, index, columns)}
            </tr>
        }
    };

    let loading_row_renderer = |_class: Signal<String>,
                                _cell_class: Callback<(usize,), String>,
                                _inner_class: Callback<(usize,), String>,
                                _index: usize,
                                col_count: usize| {
        view! {
            <tr class="loading" aria-hidden="true">
                {(0..col_count).map(|_| view! { <td></td> }).collect_view()}
            </tr>
        }
    };

    let provider = Provider { rows };

    view! {
        <div class="table-scroll" node_ref=container>
            <table
                role="grid"
                tabindex="0"
                aria-label=label
                aria-rowcount=move || count.get() + 1
                aria-activedescendant=move || selected.get().map(|i| format!("{id}-r{i}"))
                on:keydown=on_keydown
            >
                <TableContent
                    rows=provider
                    scroll_container=container
                    sorting=sorting
                    sorting_mode=leptos_struct_table::SortingMode::SingleColumn
                    row_renderer=row_renderer
                    loading_row_renderer=loading_row_renderer
                />
            </table>
            <Show when=move || count.get() == 0>
                <p class="empty">{empty.clone()}</p>
            </Show>
        </div>
    }
}

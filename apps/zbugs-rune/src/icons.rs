//! The toolbar and list icons zbugs-swift takes from SF Symbols, drawn on
//! a 16-point grid.

use rune::Icon;

/// `circle`: an open issue.
pub static CIRCLE: Icon = Icon {
    name: "zbugs-circle",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.75" fill="none" stroke="currentColor" stroke-width="1.5"/>"#,
};

/// `checkmark.circle.fill`: a closed issue.
pub static CHECK_CIRCLE_FILL: Icon = Icon {
    name: "zbugs-check-circle-fill",
    view: 16.0,
    body: r#"<path fill="currentColor" fill-rule="evenodd" d="M8 1.5A6.5 6.5 0 1 1 8 14.5A6.5 6.5 0 0 1 8 1.5ZM4.9 8.1L5.9 7.1L7.2 8.4L10.1 5.5L11.1 6.5L7.2 10.4Z"/>"#,
};

/// `checkmark.circle`: close the issue.
pub static CHECK_CIRCLE: Icon = Icon {
    name: "zbugs-check-circle",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.25"/><path d="M5.5 8.25L7.25 10L10.5 6.25" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// `arrow.uturn.backward.circle`: reopen the issue.
pub static REOPEN: Icon = Icon {
    name: "zbugs-reopen",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.25"/><path d="M5.5 7H9a1.75 1.75 0 0 1 0 3.5H7.5M7 5.25L5.25 7L7 8.75" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// `pencil`: edit the issue.
pub static PENCIL: Icon = Icon {
    name: "zbugs-pencil",
    view: 16.0,
    body: r#"<path d="M10.75 3.25L12.75 5.25L6 12H4V10Z" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linejoin="round"/>"#,
};

/// `trash`: delete the issue.
pub static TRASH: Icon = Icon {
    name: "zbugs-trash",
    view: 16.0,
    body: r#"<path d="M3.5 4.5H12.5M6.5 4.5V3H9.5V4.5M4.75 4.5L5.3 13H10.7L11.25 4.5" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// `square.and.pencil`: a new issue.
pub static COMPOSE: Icon = Icon {
    name: "zbugs-compose",
    view: 16.0,
    body: r#"<path d="M8 3H3.5V12.5H13V8" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"/><path d="M11.75 2.5L13.5 4.25L8.5 9.25H6.75V7.5Z" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linejoin="round"/>"#,
};

/// `person.crop.circle`: the account menu.
pub static PERSON: Icon = Icon {
    name: "zbugs-person",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" stroke-width="1.25"/><circle cx="8" cy="6.5" r="2" fill="none" stroke="currentColor" stroke-width="1.25"/><path d="M4.25 12.25C5.1 10.9 6.4 10.25 8 10.25S10.9 10.9 11.75 12.25" fill="none" stroke="currentColor" stroke-width="1.25"/>"#,
};

/// `checklist`: pick issues to delete.
pub static SELECT: Icon = Icon {
    name: "zbugs-select",
    view: 16.0,
    body: r#"<path d="M2.75 4.5L3.75 5.5L5.75 3.5M2.75 10.5L3.75 11.5L5.75 9.5M8 4.5H13.25M8 10.5H13.25" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

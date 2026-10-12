//! Path conventions.
//!
//! A *property path* names a property (`/devices/0/inputs/0/Mute`); its value lives at
//! `<property>/value`, which is what `subscribe`, `set` and pushes use. Stores are keyed by the
//! property path; both spellings are accepted everywhere.

/// `/a/b/Mute/value` → `/a/b/Mute` (a `?query` is dropped); anything else is returned unchanged.
pub fn property_path(path: &str) -> &str {
    let p = match path.find('?') {
        Some(q) => &path[..q],
        None => path,
    };
    p.strip_suffix("/value").unwrap_or(p)
}

/// `/a/b/Mute` → `/a/b/Mute/value` (idempotent).
pub fn value_path(path: &str) -> String {
    format!("{}/value", property_path(path))
}

/// Joins a node path and a child or property name.
pub fn join(node: &str, name: &str) -> String {
    if node == "/" || node.is_empty() {
        format!("/{name}")
    } else {
        format!("{node}/{name}")
    }
}

/// The node part and last component: `/a/b/Mute` → (`/a/b`, `Mute`); `/Mute` → (`/`, `Mute`).
pub fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        None => ("/", path),
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
    }
}

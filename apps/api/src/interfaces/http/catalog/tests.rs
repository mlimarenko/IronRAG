use super::*;

#[test]
fn library_search_window_is_bounded() {
    let window = normalize_catalog_library_page(CatalogLibraryListQuery {
        search: Some("  alpha  ".to_string()),
        offset: Some(4),
        limit: Some(u32::MAX),
    })
    .expect("bounded query should be valid");

    assert_eq!(window.search.as_deref(), Some("alpha"));
    assert_eq!(window.offset, 4);
    assert_eq!(window.limit, MAX_CATALOG_LIBRARY_PAGE_LIMIT);
}

#[test]
fn library_search_results_stay_bounded() {
    let items = (0..MAX_CATALOG_LIBRARY_PAGE_LIMIT + 1).collect::<Vec<_>>();

    assert_eq!(
        cap_catalog_library_page(items, MAX_CATALOG_LIBRARY_PAGE_LIMIT).len(),
        MAX_CATALOG_LIBRARY_PAGE_LIMIT as usize,
    );
}

#[test]
fn library_search_rejects_oversized_text() {
    let error = normalize_catalog_library_page(CatalogLibraryListQuery {
        search: Some("x".repeat(MAX_CATALOG_LIBRARY_SEARCH_CHARS + 1)),
        offset: None,
        limit: None,
    });

    assert!(matches!(error, Err(ApiError::BadRequest(_))));
}

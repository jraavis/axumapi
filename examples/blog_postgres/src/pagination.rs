//! Page-number pagination shared by every list endpoint.

use axumapi::orm::{Model, QuerySet};
use axumapi::prelude::*;

/// Page size when the client sends none.
pub const DEFAULT_PER_PAGE: u64 = 20;

/// Query parameters `?page=2&per_page=10`.
#[derive(Debug, Clone, Copy, Deserialize, Validate, Schema)]
pub struct PageParams {
    /// 1-based page number (default 1).
    #[field(ge = 1)]
    pub page: Option<u64>,
    /// Items per page, at most 100 (default 20).
    #[field(ge = 1, le = 100)]
    pub per_page: Option<u64>,
}

impl PageParams {
    /// The requested page, defaulting to the first.
    pub fn page(&self) -> u64 {
        self.page.unwrap_or(1)
    }

    /// The requested page size, defaulting to [`DEFAULT_PER_PAGE`].
    pub fn per_page(&self) -> u64 {
        self.per_page.unwrap_or(DEFAULT_PER_PAGE)
    }
}

/// One page of `T`.
#[derive(Debug, Serialize, Schema)]
pub struct Paginated<T: Schema> {
    /// The items of this page.
    pub items: Vec<T>,
    /// Items over all pages.
    pub total: u64,
    /// 1-based number of this page.
    pub page: u64,
    /// Requested page size.
    pub per_page: u64,
    /// Number of pages.
    pub total_pages: u64,
}

/// Run `queryset` for the requested page and convert each row with `map`.
///
/// # Errors
/// Database errors, or those of `map`.
pub async fn paginate<M, T, F, Fut>(
    queryset: QuerySet<M>,
    params: PageParams,
    map: F,
) -> Result<Paginated<T>, ApiError>
where
    M: Model,
    T: Schema,
    F: Fn(M) -> Fut,
    Fut: std::future::Future<Output = Result<T, ApiError>>,
{
    let page = queryset.paginate(params.page(), params.per_page()).await?;
    let (total, total_pages) = (page.total, page.total_pages());
    let mut items = Vec::with_capacity(page.items.len());
    for row in page.items {
        items.push(map(row).await?);
    }
    Ok(Paginated {
        items,
        total,
        page: page.page,
        per_page: page.per_page,
        total_pages,
    })
}

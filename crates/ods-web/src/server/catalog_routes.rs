//! The Catalog's routes (#313): its page, the model pages, and their JSON API. GET
//! only; the same view models back the pages and the API.

use std::sync::atomic::Ordering;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};

use super::{Shared, error};
use crate::catalog::{CatalogQuery, ModelView};

/// Whether this server serves the plan's Why page (`state/plan`, #311), so model pages
/// link to it rather than showing it as planned. Set when that route is registered.
pub(super) const WHY_AVAILABLE: bool = false;

/// `/catalog`: every node, filtered by the query.
pub(super) async fn page(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Html<String> {
    let generation = state.generation.load(Ordering::SeqCst);
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    let view = dashboard.catalog(
        &snapshot.document,
        &CatalogQuery::from_pairs(&pairs),
        state.details,
    );
    Html(crate::catalog_page::catalog_page(
        &dashboard.shell("catalog/models"),
        &view,
        generation,
    ))
}

/// A node's view model, with what this server links to.
fn model_view(state: &Shared, id: &str) -> Option<ModelView> {
    let snapshot = state.current();
    let mut view = snapshot
        .dashboard()
        .model(&snapshot.document, id, state.details)?;
    view.links.why_available = WHY_AVAILABLE;
    Some(view)
}

/// `/catalog/<id>`: one node, on the tab asked for (`?tab=code`); the shell's 404 page
/// when there is no such node, or the id isn't a valid percent-encoding.
pub(super) async fn model(
    State(state): State<Shared>,
    id: Result<Path<String>, PathRejection>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let generation = state.generation.load(Ordering::SeqCst);
    let dashboard = state.current().dashboard();
    let shell = dashboard.shell("catalog/models");
    let tab = pairs
        .iter()
        .find(|(k, _)| k == "tab")
        .map(|(_, v)| v.as_str());
    let id = id.map(|Path(id)| id);
    match id.as_deref().ok().and_then(|id| model_view(&state, id)) {
        Some(view) => Html(crate::model_page::model_page(
            &shell,
            &view,
            crate::model_page::tab(tab),
            generation,
        ))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Html(crate::model_page::not_found_page(
                &shell,
                id.as_deref().unwrap_or("(not a valid node id)"),
                generation,
            )),
        )
            .into_response(),
    }
}

/// `/api/catalog`: the Catalog's view model, with the same query as the page.
pub(super) async fn api(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let snapshot = state.current();
    Json(snapshot.dashboard().catalog(
        &snapshot.document,
        &CatalogQuery::from_pairs(&pairs),
        state.details,
    ))
    .into_response()
}

/// `/api/catalog/<id>`: a model page's view model, every tab.
pub(super) async fn model_api(
    State(state): State<Shared>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    match id.ok().and_then(|Path(id)| model_view(&state, &id)) {
        Some(view) => Json(view).into_response(),
        None => error(StatusCode::NOT_FOUND, "no such node"),
    }
}

//! The Catalog's routes (#313): its page, the model pages, the Freshness evidence screen
//! (#350), the Semantic layer (#352), and their JSON API; and the Impact simulator's (#347) and the ERD page's
//! (#64). GET only; the same view models back the pages and the API. Pages are built on a blocking thread, as the State pages
//! are: building one may plan.

use std::sync::atomic::Ordering;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};

use super::{Shared, error};
use crate::catalog::CatalogQuery;
use crate::erd::{ErdQuery, erd_view};
use crate::impact::{ImpactQuery, impact_view};
use crate::state_pages::blocking;

/// `/catalog`: every node, filtered by the query.
pub(super) async fn page(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let view = dashboard.catalog(
            &snapshot.document,
            &CatalogQuery::from_pairs(&pairs),
            state.details,
        );
        Html(crate::catalog_page::catalog_page(
            &dashboard.shell("catalog"),
            &view,
            generation,
        ))
        .into_response()
    })
    .await
}

/// `/catalog/<id>`: one node, on the tab asked for (`?tab=code`); the shell's 404 page
/// when there is no such node, or the id isn't a valid percent-encoding.
pub(super) async fn model(
    State(state): State<Shared>,
    id: Result<Path<String>, PathRejection>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let id = id.map(|Path(id)| id).ok();
    blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let shell = dashboard.shell("catalog");
        let tab = pairs
            .iter()
            .find(|(k, _)| k == "tab")
            .map(|(_, v)| v.as_str());
        let view = id
            .as_deref()
            .and_then(|id| dashboard.model(&snapshot.document, id, state.details));
        match view {
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
    })
    .await
}

/// `/api/catalog`: the Catalog's view model, with the same query as the page.
pub(super) async fn api(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let snapshot = state.current();
        Json(snapshot.dashboard().catalog(
            &snapshot.document,
            &CatalogQuery::from_pairs(&pairs),
            state.details,
        ))
        .into_response()
    })
    .await
}

/// `/api/catalog/<id>`: a model page's view model, every tab.
pub(super) async fn model_api(
    State(state): State<Shared>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let id = id.map(|Path(id)| id).ok();
    blocking(move || {
        let snapshot = state.current();
        match id.and_then(|id| {
            snapshot
                .dashboard()
                .model(&snapshot.document, &id, state.details)
        }) {
            Some(view) => Json(view).into_response(),
            None => error(StatusCode::NOT_FOUND, "no such node"),
        }
    })
    .await
}

/// `/catalog/sources`: the Freshness evidence screen (#350).
pub(super) async fn sources_page(State(state): State<Shared>) -> Response {
    blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let view = dashboard.freshness(&snapshot.document, state.details);
        Html(crate::freshness_page::freshness_page(
            &dashboard.shell("catalog"),
            &view,
            generation,
        ))
        .into_response()
    })
    .await
}

/// `/api/catalog/sources`: the Freshness evidence screen's view model.
pub(super) async fn sources_api(State(state): State<Shared>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        Json(
            snapshot
                .dashboard()
                .freshness(&snapshot.document, state.details),
        )
        .into_response()
    })
    .await
}

/// `/catalog/semantic`: the Semantic layer (#352), read-only.
pub(super) async fn semantic_page(State(state): State<Shared>) -> Html<String> {
    let generation = state.generation.load(Ordering::SeqCst);
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    Html(crate::semantic_page::semantic_page(
        &dashboard.shell("catalog"),
        &dashboard.semantic(),
        generation,
    ))
}

/// `/api/catalog/semantic`: the Semantic layer page's view model.
pub(super) async fn semantic_api(
    State(state): State<Shared>,
) -> Json<crate::semantic::SemanticView> {
    Json(state.current().dashboard().semantic())
}

/// `/lineage/impact`: the Impact simulator (#347), simulating the query's changes.
pub(super) async fn impact_page(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let query = ImpactQuery::from_pairs(&pairs);
        let view = impact_view(
            &snapshot.graph,
            &snapshot.names(),
            &dashboard.catalog,
            &query,
        );
        Html(crate::impact_page::impact_page(
            &dashboard.shell("lineage"),
            &view,
            &pairs,
            query.add,
            generation,
        ))
        .into_response()
    })
    .await
}

/// `/api/lineage/impact`: the Impact simulator's view model, for the same query.
pub(super) async fn impact_api(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        Json(impact_view(
            &snapshot.graph,
            &snapshot.names(),
            &dashboard.catalog,
            &ImpactQuery::from_pairs(&pairs),
        ))
        .into_response()
    })
    .await
}

/// `/erd`: the ERD page (#64), scoped by the query.
pub(super) async fn erd_page(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let view = erd_view(dashboard.erd.as_ref(), &ErdQuery::from_pairs(&pairs));
        match crate::erd_page::erd_page(&dashboard.shell("erd"), &view, &pairs, generation) {
            Ok(page) => Html(page).into_response(),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    })
    .await
}

/// `/api/erd`: the ERD page's view model, for the same query.
pub(super) async fn erd_api(
    State(state): State<Shared>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        Json(erd_view(
            dashboard.erd.as_ref(),
            &ErdQuery::from_pairs(&pairs),
        ))
        .into_response()
    })
    .await
}

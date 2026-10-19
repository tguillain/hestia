use axum::Json;
use axum::extract::{Query, State};
use hestia_core::{DeviceId, Semantic, Unit};
use hestia_history::Buckets;
use serde::{Deserialize, Serialize};

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;

const DEFAULT_BUCKETS: usize = 240;

/// A graph of the dashboard, declared in the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChartConfig {
    pub title: String,
    #[serde(default = "default_hours")]
    pub hours: u32,
    pub series: Vec<SeriesSelector>,
}

/// One point (`point`), or every point of a kind (`semantic`), of a device.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesSelector {
    pub device: DeviceId,
    pub point: Option<String>,
    pub semantic: Option<Semantic>,
    pub label: Option<String>,
}

fn default_hours() -> u32 {
    24
}

#[derive(Serialize)]
pub(crate) struct ResolvedChart {
    title: String,
    hours: u32,
    series: Vec<ResolvedSeries>,
}

#[derive(Serialize)]
struct ResolvedSeries {
    device: DeviceId,
    point: String,
    label: String,
    unit: Option<Unit>,
    semantic: Semantic,
}

pub(crate) async fn charts(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Json<Vec<ResolvedChart>> {
    let charts = state
        .charts
        .iter()
        .map(|chart| ResolvedChart {
            title: chart.title.clone(),
            hours: chart.hours,
            series: chart
                .series
                .iter()
                .flat_map(|selector| resolve(&state, selector))
                .collect(),
        })
        .collect();
    Json(charts)
}

fn resolve(state: &AppState, selector: &SeriesSelector) -> Vec<ResolvedSeries> {
    let Some(device) = state.hub.device(&selector.device) else {
        return Vec::new();
    };
    device
        .points
        .iter()
        .filter(|point| {
            selector
                .point
                .as_ref()
                .is_none_or(|key| *key == point.spec.key)
                && selector
                    .semantic
                    .is_none_or(|semantic| semantic == point.spec.semantic)
                && (selector.point.is_some() || selector.semantic.is_some())
        })
        .map(|point| ResolvedSeries {
            device: device.id.clone(),
            point: point.spec.key.clone(),
            label: selector
                .label
                .clone()
                .unwrap_or_else(|| point.spec.label.clone()),
            unit: point.spec.unit,
            semantic: point.spec.semantic,
        })
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    device: String,
    point: String,
    #[serde(default = "default_hours")]
    hours: u32,
    #[serde(default)]
    buckets: Option<usize>,
}

pub(crate) async fn history(
    State(state): State<AppState>,
    _user: CurrentUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Buckets>, ApiError> {
    let history = state
        .history
        .clone()
        .ok_or_else(|| ApiError::NotFound("history is disabled".to_owned()))?;
    let device: DeviceId = query
        .device
        .parse()
        .map_err(|_| ApiError::NotFound(format!("unknown device {}", query.device)))?;
    let buckets = query.buckets.unwrap_or(DEFAULT_BUCKETS);
    let result = tokio::task::spawn_blocking(move || {
        history.query(&device, &query.point, query.hours, buckets)
    })
    .await
    .map_err(|_| ApiError::Internal)?;
    result.map(Json).map_err(|error| {
        tracing::warn!(error = format!("{error:#}"), "history query failed");
        ApiError::Internal
    })
}

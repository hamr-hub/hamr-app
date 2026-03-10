use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use chrono::Utc;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::DashboardStats};

pub async fn get_stats(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<DashboardStats>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;

    let people_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM people WHERE family_id = $1")
        .bind(family_id).fetch_one(&state.db).await?;

    let now = Utc::now();
    let upcoming_events = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM events WHERE family_id = $1 AND start_time >= $2"
    )
    .bind(family_id).bind(now).fetch_one(&state.db).await?;

    let pending_tasks = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tasks WHERE family_id = $1 AND status IN ('todo', 'in_progress')"
    )
    .bind(family_id).fetch_one(&state.db).await?;

    let things_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM things WHERE family_id = $1 AND status = 'active'"
    )
    .bind(family_id).fetch_one(&state.db).await?;

    let spaces_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM spaces WHERE family_id = $1")
        .bind(family_id).fetch_one(&state.db).await?;

    Ok(Json(DashboardStats {
        family_id,
        people_count,
        upcoming_events,
        pending_tasks,
        things_count,
        spaces_count,
    }))
}

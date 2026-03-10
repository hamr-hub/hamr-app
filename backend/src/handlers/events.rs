use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::*};

pub async fn list(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Vec<Event>>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;
    let rows = sqlx::query_as::<_, Event>(
        "SELECT * FROM events WHERE family_id = $1 ORDER BY start_time ASC"
    )
    .bind(family_id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateEventRequest>,
) -> AppResult<Json<Event>> {
    let created_by: Option<Uuid> = claims.sub.parse().ok();
    let row = sqlx::query_as::<_, Event>(
        r#"INSERT INTO events (id, family_id, title, description, start_time, end_time, all_day, category, location, remind_at, created_by)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(req.family_id).bind(&req.title).bind(&req.description)
    .bind(req.start_time).bind(req.end_time).bind(req.all_day.unwrap_or(false))
    .bind(&req.category).bind(&req.location).bind(req.remind_at).bind(created_by)
    .fetch_one(&state.db).await?;
    Ok(Json(row))
}

pub async fn get(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<Event>> {
    let row = sqlx::query_as::<_, Event>("SELECT * FROM events WHERE id = $1")
        .bind(id).fetch_optional(&state.db).await?
        .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<UpdateEventRequest>,
) -> AppResult<Json<Event>> {
    let row = sqlx::query_as::<_, Event>(
        r#"UPDATE events SET
           title = COALESCE($2, title),
           description = COALESCE($3, description),
           start_time = COALESCE($4, start_time),
           end_time = COALESCE($5, end_time),
           category = COALESCE($6, category),
           location = COALESCE($7, location),
           remind_at = COALESCE($8, remind_at),
           updated_at = NOW()
           WHERE id = $1 RETURNING *"#
    )
    .bind(id).bind(&req.title).bind(&req.description).bind(req.start_time)
    .bind(req.end_time).bind(&req.category).bind(&req.location).bind(req.remind_at)
    .fetch_optional(&state.db).await?
    .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(id).execute(&state.db).await?.rows_affected();
    if affected == 0 { return Err(crate::errors::AppError::NotFound); }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

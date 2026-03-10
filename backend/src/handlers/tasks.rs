use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use chrono::Utc;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::*};

pub async fn list(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Vec<Task>>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;
    let rows = sqlx::query_as::<_, Task>(
        "SELECT * FROM tasks WHERE family_id = $1 ORDER BY priority DESC, created_at ASC"
    )
    .bind(family_id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateTaskRequest>,
) -> AppResult<Json<Task>> {
    let created_by: Option<Uuid> = claims.sub.parse().ok();
    let row = sqlx::query_as::<_, Task>(
        r#"INSERT INTO tasks (id, family_id, title, description, priority, due_date, assigned_to, tags, is_milestone, created_by)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(req.family_id).bind(&req.title).bind(&req.description)
    .bind(req.priority.as_deref().unwrap_or("medium")).bind(req.due_date)
    .bind(req.assigned_to).bind(&req.tags).bind(req.is_milestone.unwrap_or(false))
    .bind(created_by)
    .fetch_one(&state.db).await?;
    Ok(Json(row))
}

pub async fn get(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<Task>> {
    let row = sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = $1")
        .bind(id).fetch_optional(&state.db).await?
        .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<UpdateTaskRequest>,
) -> AppResult<Json<Task>> {
    let completed_at = if req.status.as_deref() == Some("done") {
        Some(Utc::now())
    } else {
        None
    };
    let row = sqlx::query_as::<_, Task>(
        r#"UPDATE tasks SET
           title = COALESCE($2, title),
           description = COALESCE($3, description),
           status = COALESCE($4, status),
           priority = COALESCE($5, priority),
           due_date = COALESCE($6, due_date),
           assigned_to = COALESCE($7, assigned_to),
           tags = COALESCE($8, tags),
           completed_at = CASE WHEN $4 = 'done' THEN $9 ELSE completed_at END,
           updated_at = NOW()
           WHERE id = $1 RETURNING *"#
    )
    .bind(id).bind(&req.title).bind(&req.description).bind(&req.status)
    .bind(&req.priority).bind(req.due_date).bind(req.assigned_to).bind(&req.tags)
    .bind(completed_at)
    .fetch_optional(&state.db).await?
    .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = sqlx::query("DELETE FROM tasks WHERE id = $1")
        .bind(id).execute(&state.db).await?.rows_affected();
    if affected == 0 { return Err(crate::errors::AppError::NotFound); }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

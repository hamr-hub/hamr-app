use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::*};

pub async fn list(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Vec<Space>>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;
    let rows = sqlx::query_as::<_, Space>(
        "SELECT * FROM spaces WHERE family_id = $1 ORDER BY created_at ASC"
    )
    .bind(family_id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

pub async fn create(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateSpaceRequest>,
) -> AppResult<Json<Space>> {
    let row = sqlx::query_as::<_, Space>(
        r#"INSERT INTO spaces (id, family_id, name, type, description, icon, notes)
           VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(req.family_id).bind(&req.name).bind(&req.r#type)
    .bind(&req.description).bind(&req.icon).bind(&req.notes)
    .fetch_one(&state.db).await?;
    Ok(Json(row))
}

pub async fn get(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<Space>> {
    let row = sqlx::query_as::<_, Space>("SELECT * FROM spaces WHERE id = $1")
        .bind(id).fetch_optional(&state.db).await?
        .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<UpdateSpaceRequest>,
) -> AppResult<Json<Space>> {
    let row = sqlx::query_as::<_, Space>(
        r#"UPDATE spaces SET
           name = COALESCE($2, name),
           type = COALESCE($3, type),
           description = COALESCE($4, description),
           icon = COALESCE($5, icon),
           notes = COALESCE($6, notes),
           updated_at = NOW()
           WHERE id = $1 RETURNING *"#
    )
    .bind(id).bind(&req.name).bind(&req.r#type).bind(&req.description)
    .bind(&req.icon).bind(&req.notes)
    .fetch_optional(&state.db).await?
    .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = sqlx::query("DELETE FROM spaces WHERE id = $1")
        .bind(id).execute(&state.db).await?.rows_affected();
    if affected == 0 { return Err(crate::errors::AppError::NotFound); }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

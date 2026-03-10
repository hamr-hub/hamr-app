use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::*};

pub async fn list(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Vec<Thing>>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;
    let rows = sqlx::query_as::<_, Thing>(
        "SELECT * FROM things WHERE family_id = $1 ORDER BY created_at DESC"
    )
    .bind(family_id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

pub async fn create(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateThingRequest>,
) -> AppResult<Json<Thing>> {
    let row = sqlx::query_as::<_, Thing>(
        r#"INSERT INTO things (id, family_id, name, category, description, location, quantity, unit, purchase_date, expiry_date, notes, tags)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(req.family_id).bind(&req.name).bind(&req.category)
    .bind(&req.description).bind(&req.location).bind(req.quantity.unwrap_or(1))
    .bind(&req.unit).bind(req.purchase_date).bind(req.expiry_date)
    .bind(&req.notes).bind(&req.tags)
    .fetch_one(&state.db).await?;
    Ok(Json(row))
}

pub async fn get(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<Thing>> {
    let row = sqlx::query_as::<_, Thing>("SELECT * FROM things WHERE id = $1")
        .bind(id).fetch_optional(&state.db).await?
        .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<UpdateThingRequest>,
) -> AppResult<Json<Thing>> {
    let row = sqlx::query_as::<_, Thing>(
        r#"UPDATE things SET
           name = COALESCE($2, name),
           category = COALESCE($3, category),
           description = COALESCE($4, description),
           location = COALESCE($5, location),
           quantity = COALESCE($6, quantity),
           status = COALESCE($7, status),
           notes = COALESCE($8, notes),
           tags = COALESCE($9, tags),
           updated_at = NOW()
           WHERE id = $1 RETURNING *"#
    )
    .bind(id).bind(&req.name).bind(&req.category).bind(&req.description)
    .bind(&req.location).bind(req.quantity).bind(&req.status)
    .bind(&req.notes).bind(&req.tags)
    .fetch_optional(&state.db).await?
    .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = sqlx::query("DELETE FROM things WHERE id = $1")
        .bind(id).execute(&state.db).await?.rows_affected();
    if affected == 0 { return Err(crate::errors::AppError::NotFound); }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

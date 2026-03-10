use axum::{extract::{Query, State}, Extension, Json};
use uuid::Uuid;
use crate::{db::AppState, errors::AppResult, middleware::Claims, models::*};

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Vec<Person>>> {
    let family_id: Uuid = params.get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| crate::errors::AppError::ValidationError("family_id required".into()))?;
    let _ = claims;
    let rows = sqlx::query_as::<_, Person>(
        "SELECT * FROM people WHERE family_id = $1 ORDER BY created_at ASC"
    )
    .bind(family_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreatePersonRequest>,
) -> AppResult<Json<Person>> {
    let _ = claims;
    let row = sqlx::query_as::<_, Person>(
        r#"INSERT INTO people (id, family_id, name, role, birthday, phone, email, notes, tags)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(req.family_id)
    .bind(&req.name)
    .bind(&req.role)
    .bind(req.birthday)
    .bind(&req.phone)
    .bind(&req.email)
    .bind(&req.notes)
    .bind(&req.tags)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row))
}

pub async fn get(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<Person>> {
    let row = sqlx::query_as::<_, Person>("SELECT * FROM people WHERE id = $1")
        .bind(id).fetch_optional(&state.db).await?
        .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<UpdatePersonRequest>,
) -> AppResult<Json<Person>> {
    let row = sqlx::query_as::<_, Person>(
        r#"UPDATE people SET
           name = COALESCE($2, name),
           role = COALESCE($3, role),
           birthday = COALESCE($4, birthday),
           phone = COALESCE($5, phone),
           email = COALESCE($6, email),
           notes = COALESCE($7, notes),
           tags = COALESCE($8, tags),
           updated_at = NOW()
           WHERE id = $1 RETURNING *"#
    )
    .bind(id).bind(&req.name).bind(&req.role).bind(req.birthday)
    .bind(&req.phone).bind(&req.email).bind(&req.notes).bind(&req.tags)
    .fetch_optional(&state.db).await?
    .ok_or(crate::errors::AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected = sqlx::query("DELETE FROM people WHERE id = $1")
        .bind(id).execute(&state.db).await?.rows_affected();
    if affected == 0 { return Err(crate::errors::AppError::NotFound); }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

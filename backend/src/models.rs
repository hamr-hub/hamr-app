use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// ===== People =====
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Person {
    pub id: Uuid,
    pub family_id: Uuid,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub role: Option<String>,
    pub birthday: Option<NaiveDate>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePersonRequest {
    pub family_id: Uuid,
    pub name: String,
    pub role: Option<String>,
    pub birthday: Option<NaiveDate>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePersonRequest {
    pub name: Option<String>,
    pub role: Option<String>,
    pub birthday: Option<NaiveDate>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

// ===== Events =====
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Event {
    pub id: Uuid,
    pub family_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub all_day: bool,
    pub category: Option<String>,
    pub location: Option<String>,
    pub remind_at: Option<DateTime<Utc>>,
    pub is_recurring: bool,
    pub recurrence_rule: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEventRequest {
    pub family_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub category: Option<String>,
    pub location: Option<String>,
    pub remind_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEventRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub category: Option<String>,
    pub location: Option<String>,
    pub remind_at: Option<DateTime<Utc>>,
}

// ===== Tasks =====
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Task {
    pub id: Uuid,
    pub family_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub due_date: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub tags: Option<Vec<String>>,
    pub is_milestone: bool,
    pub created_by: Option<Uuid>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub family_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub tags: Option<Vec<String>>,
    pub is_milestone: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub tags: Option<Vec<String>>,
}

// ===== Things =====
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Thing {
    pub id: Uuid,
    pub family_id: Uuid,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub quantity: i32,
    pub unit: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_price: Option<f64>,
    pub expiry_date: Option<NaiveDate>,
    pub status: String,
    pub image_url: Option<String>,
    pub tags: Option<Vec<String>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateThingRequest {
    pub family_id: Uuid,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub quantity: Option<i32>,
    pub unit: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub expiry_date: Option<NaiveDate>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateThingRequest {
    pub name: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub quantity: Option<i32>,
    pub status: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

// ===== Spaces =====
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Space {
    pub id: Uuid,
    pub family_id: Uuid,
    pub name: String,
    pub r#type: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub area_sqm: Option<f64>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSpaceRequest {
    pub family_id: Uuid,
    pub name: String,
    pub r#type: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSpaceRequest {
    pub name: Option<String>,
    pub r#type: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub notes: Option<String>,
}

// ===== Dashboard =====
#[derive(Debug, Serialize)]
pub struct DashboardStats {
    pub family_id: Uuid,
    pub people_count: i64,
    pub upcoming_events: i64,
    pub pending_tasks: i64,
    pub things_count: i64,
    pub spaces_count: i64,
}

// ===== Pagination =====
#[derive(Debug, Deserialize)]
pub struct Pagination {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

impl Pagination {
    pub fn offset(&self) -> i64 {
        let p = self.page.unwrap_or(1).max(1);
        let l = self.limit();
        (p - 1) * l
    }
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(20).min(100).max(1)
    }
}

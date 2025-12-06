//! SONA Personalization Engine HTTP Server
//!
//! Actix-web server providing REST API for personalization services.
//! Runs on port 8082 as specified in SPARC architecture.

use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::sync::Arc;

use media_gateway_sona::{
    SonaEngine, SonaConfig, GenerateRecommendations, BuildUserPreferenceVector,
    UpdateUserLoRA, UserProfile, UserLoRAAdapter, ViewingEvent,
};

/// Application state
struct AppState {
    engine: Arc<SonaEngine>,
    lora_storage: Arc<media_gateway_sona::LoRAStorage>,
    db_pool: sqlx::PgPool,
}

impl AppState {
    /// Load user profile from database
    async fn load_user_profile(&self, user_id: Uuid) -> anyhow::Result<UserProfile> {
        // Fetch viewing history from database
        let viewing_history = sqlx::query_as::<_, ViewingEventRow>(
            r#"
            SELECT content_id, timestamp, completion_rate, rating, is_rewatch, dismissed
            FROM viewing_events
            WHERE user_id = $1
            ORDER BY timestamp DESC
            LIMIT 100
            "#
        )
        .bind(user_id)
        .fetch_all(&self.db_pool)
        .await?;

        let events: Vec<ViewingEvent> = viewing_history.into_iter().map(|row| row.into()).collect();

        // Get content embedding function
        let get_embedding = |content_id: Uuid| -> anyhow::Result<Vec<f32>> {
            // In production, query embedding service
            Ok(vec![0.0; 512])
        };

        // Build preference vector
        let preference_vector = BuildUserPreferenceVector::execute(
            user_id,
            &events,
            get_embedding,
        ).await?;

        Ok(UserProfile {
            user_id,
            preference_vector,
            genre_affinities: std::collections::HashMap::new(),
            temporal_patterns: Default::default(),
            mood_history: Vec::new(),
            interaction_count: events.len(),
            last_update_time: chrono::Utc::now(),
        })
    }
}

#[derive(sqlx::FromRow)]
struct ViewingEventRow {
    content_id: Uuid,
    timestamp: chrono::DateTime<chrono::Utc>,
    completion_rate: f32,
    rating: Option<i16>,
    is_rewatch: bool,
    dismissed: bool,
}

impl From<ViewingEventRow> for ViewingEvent {
    fn from(row: ViewingEventRow) -> Self {
        ViewingEvent {
            content_id: row.content_id,
            timestamp: row.timestamp,
            completion_rate: row.completion_rate,
            rating: row.rating.map(|r| r as u8),
            is_rewatch: row.is_rewatch,
            dismissed: row.dismissed,
        }
    }
}

/// Health check endpoint
async fn health() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "service": "sona-personalization-engine",
        "version": "0.1.0"
    }))
}

/// Recommendation request
#[derive(Debug, Deserialize)]
struct RecommendationRequest {
    user_id: Uuid,
    context: Option<RecommendationContextDto>,
    limit: Option<usize>,
    exclude_watched: Option<bool>,
    diversity_threshold: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct RecommendationContextDto {
    mood: Option<String>,
    time_of_day: Option<String>,
    device_type: Option<String>,
    viewing_with: Option<Vec<String>>,
}

/// Recommendation response
#[derive(Debug, Serialize)]
struct RecommendationResponse {
    recommendations: Vec<RecommendationDto>,
    generated_at: String,
    ttl_seconds: u32,
}

#[derive(Debug, Serialize)]
struct RecommendationDto {
    content_id: Uuid,
    confidence_score: f32,
    recommendation_type: String,
    based_on: Vec<String>,
    explanation: String,
}

/// POST /api/v1/recommendations
async fn get_recommendations(
    req: web::Json<RecommendationRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    // Load user profile
    let profile = match state.load_user_profile(req.user_id).await {
        Ok(profile) => profile,
        Err(e) => {
            tracing::error!("Failed to load user profile: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to load user profile",
                "message": e.to_string()
            }));
        }
    };

    // Load LoRA adapter if available
    let lora_adapter = state.lora_storage.load_adapter(req.user_id).await.ok();

    // Convert context
    let context = req.context.as_ref().map(|ctx| {
        media_gateway_sona::RecommendationContext {
            mood: ctx.mood.clone(),
            time_of_day: ctx.time_of_day.clone(),
            device_type: ctx.device_type.clone(),
            viewing_with: ctx.viewing_with.clone().unwrap_or_default(),
        }
    });

    // Get content embedding function (simulated for now)
    let get_embedding = |content_id: Uuid| -> anyhow::Result<Vec<f32>> {
        // In production, this would query the embedding database
        Ok(vec![0.0; 512])
    };

    // Generate recommendations
    match GenerateRecommendations::execute(
        req.user_id,
        &profile,
        context,
        lora_adapter.as_ref(),
        get_embedding,
    ).await {
        Ok(recommendations) => {
            let response = RecommendationResponse {
                recommendations: recommendations.into_iter().map(|r| RecommendationDto {
                    content_id: r.content_id,
                    confidence_score: r.confidence_score,
                    recommendation_type: format!("{:?}", r.recommendation_type),
                    based_on: r.based_on,
                    explanation: r.explanation,
                }).collect(),
                generated_at: chrono::Utc::now().to_rfc3339(),
                ttl_seconds: 3600,
            };
            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            tracing::error!("Recommendation generation failed: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Recommendation generation failed",
                "message": e.to_string()
            }))
        }
    }
}

/// Similar content request
#[derive(Debug, Deserialize)]
struct SimilarContentRequest {
    content_id: Uuid,
    limit: Option<usize>,
}

/// POST /api/v1/recommendations/similar
async fn get_similar_content(
    _req: web::Json<SimilarContentRequest>,
    _engine: web::Data<Arc<SonaEngine>>,
) -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "similar_content": []
    }))
}

/// Personalization score request
#[derive(Debug, Deserialize)]
struct PersonalizationScoreRequest {
    user_id: Uuid,
    content_id: Uuid,
}

/// POST /api/v1/personalization/score
async fn get_personalization_score(
    req: web::Json<PersonalizationScoreRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    // Load user profile
    let profile = match state.load_user_profile(req.user_id).await {
        Ok(profile) => profile,
        Err(e) => {
            tracing::error!("Failed to load user profile: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to load user profile",
                "message": e.to_string()
            }));
        }
    };

    // Load LoRA adapter
    let lora_adapter = match state.lora_storage.load_adapter(req.user_id).await {
        Ok(adapter) => adapter,
        Err(e) => {
            tracing::warn!("LoRA adapter not found, using base model: {}", e);
            // Create new adapter for user
            let mut adapter = UserLoRAAdapter::new(req.user_id);
            adapter.initialize_random();
            adapter
        }
    };

    // Get content embedding
    let content_embedding = vec![0.0; 512]; // In production, query embedding service

    // Compute LoRA personalization score
    let lora_score = match media_gateway_sona::lora::compute_lora_score(
        &lora_adapter,
        &content_embedding,
        &profile.preference_vector,
    ) {
        Ok(score) => score,
        Err(e) => {
            tracing::error!("LoRA scoring failed: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "LoRA scoring failed",
                "message": e.to_string()
            }));
        }
    };

    // Compute component scores (simulated for now)
    let collaborative_score = 0.35;
    let content_based_score = 0.25;
    let graph_score = 0.30;
    let context_score = 0.10;

    let total_score = (collaborative_score + content_based_score + graph_score + context_score)
        * (1.0 + lora_score * 0.3);

    HttpResponse::Ok().json(serde_json::json!({
        "user_id": req.user_id,
        "content_id": req.content_id,
        "score": total_score.min(1.0).max(0.0),
        "components": {
            "collaborative": collaborative_score,
            "content_based": content_based_score,
            "graph_based": graph_score,
            "context": context_score,
            "lora_boost": lora_score
        }
    }))
}

/// User profile update request
#[derive(Debug, Deserialize)]
struct ProfileUpdateRequest {
    user_id: Uuid,
    viewing_events: Vec<ViewingEventDto>,
}

#[derive(Debug, Deserialize)]
struct ViewingEventDto {
    content_id: Uuid,
    timestamp: String,
    completion_rate: f32,
    rating: Option<u8>,
    is_rewatch: bool,
    dismissed: bool,
}

/// POST /api/v1/profile/update
async fn update_profile(
    req: web::Json<ProfileUpdateRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    // Convert viewing events
    let events: Vec<ViewingEvent> = req.viewing_events.iter().map(|dto| {
        ViewingEvent {
            content_id: dto.content_id,
            timestamp: chrono::DateTime::parse_from_rfc3339(&dto.timestamp)
                .unwrap_or_else(|_| chrono::Utc::now().into())
                .with_timezone(&chrono::Utc),
            completion_rate: dto.completion_rate,
            rating: dto.rating,
            is_rewatch: dto.is_rewatch,
            dismissed: dto.dismissed,
        }
    }).collect();

    // Store events in database
    for event in &events {
        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO viewing_events
            (user_id, content_id, timestamp, completion_rate, rating, is_rewatch, dismissed)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (user_id, content_id, timestamp) DO UPDATE
            SET completion_rate = EXCLUDED.completion_rate,
                rating = EXCLUDED.rating,
                is_rewatch = EXCLUDED.is_rewatch,
                dismissed = EXCLUDED.dismissed
            "#
        )
        .bind(req.user_id)
        .bind(event.content_id)
        .bind(event.timestamp)
        .bind(event.completion_rate)
        .bind(event.rating.map(|r| r as i16))
        .bind(event.is_rewatch)
        .bind(event.dismissed)
        .execute(&state.db_pool)
        .await {
            tracing::error!("Failed to store viewing event: {}", e);
        }
    }

    // Get content embedding function
    let get_embedding = |content_id: Uuid| -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.0; 512])
    };

    // Update preference vector
    match BuildUserPreferenceVector::execute(req.user_id, &events, get_embedding).await {
        Ok(preference_vector) => {
            // Store updated preference vector (simulated)
            tracing::info!("Updated preference vector for user {}", req.user_id);

            HttpResponse::Ok().json(serde_json::json!({
                "status": "updated",
                "user_id": req.user_id,
                "events_processed": req.viewing_events.len(),
                "preference_vector_dim": preference_vector.len()
            }))
        }
        Err(e) => {
            tracing::error!("Failed to update preference vector: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to update preference vector",
                "message": e.to_string()
            }))
        }
    }
}

/// LoRA training request
#[derive(Debug, Deserialize)]
struct LoraTrainingRequest {
    user_id: Uuid,
    force: Option<bool>,
}

/// POST /api/v1/lora/train
async fn trigger_lora_training(
    req: web::Json<LoraTrainingRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    // Load or create LoRA adapter
    let mut adapter = match state.lora_storage.load_adapter(req.user_id).await {
        Ok(adapter) => adapter,
        Err(_) => {
            let mut adapter = UserLoRAAdapter::new(req.user_id);
            adapter.initialize_random();
            adapter
        }
    };

    // Load user profile
    let profile = match state.load_user_profile(req.user_id).await {
        Ok(profile) => profile,
        Err(e) => {
            tracing::error!("Failed to load user profile: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to load user profile",
                "message": e.to_string()
            }));
        }
    };

    // Fetch recent viewing events
    let viewing_history = match sqlx::query_as::<_, ViewingEventRow>(
        r#"
        SELECT content_id, timestamp, completion_rate, rating, is_rewatch, dismissed
        FROM viewing_events
        WHERE user_id = $1
        ORDER BY timestamp DESC
        LIMIT 50
        "#
    )
    .bind(req.user_id)
    .fetch_all(&state.db_pool)
    .await {
        Ok(history) => history,
        Err(e) => {
            tracing::error!("Failed to fetch viewing history: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to fetch viewing history",
                "message": e.to_string()
            }));
        }
    };

    let events: Vec<ViewingEvent> = viewing_history.into_iter().map(|row| row.into()).collect();

    if events.len() < 10 {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Insufficient training data",
            "message": "At least 10 viewing events required for LoRA training",
            "current_count": events.len()
        }));
    }

    // Get content embedding function
    let get_embedding = |content_id: Uuid| -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.0; 512])
    };

    // Train LoRA adapter
    let start_time = std::time::Instant::now();
    match UpdateUserLoRA::execute(
        &mut adapter,
        &events,
        get_embedding,
        &profile.preference_vector,
    ).await {
        Ok(_) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;

            // Save trained adapter
            if let Err(e) = state.lora_storage.save_adapter(&adapter).await {
                tracing::error!("Failed to save LoRA adapter: {}", e);
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to save trained adapter",
                    "message": e.to_string()
                }));
            }

            HttpResponse::Ok().json(serde_json::json!({
                "status": "training_completed",
                "user_id": req.user_id,
                "duration_ms": duration_ms,
                "training_iterations": adapter.training_iterations,
                "events_used": events.len()
            }))
        }
        Err(e) => {
            tracing::error!("LoRA training failed: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "LoRA training failed",
                "message": e.to_string()
            }))
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .json()
        .init();

    tracing::info!("Starting SONA Personalization Engine on port 8082");

    // Initialize database connection
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@localhost/media_gateway".to_string());

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Initialize SONA engine
    let config = SonaConfig::default();
    let engine = Arc::new(SonaEngine::new(config));

    // Initialize LoRA storage
    let lora_storage = Arc::new(media_gateway_sona::LoRAStorage::new(db_pool.clone()));

    // Create app state
    let app_state = web::Data::new(AppState {
        engine,
        lora_storage,
        db_pool,
    });

    // Start HTTP server
    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .route("/health", web::get().to(health))
            .service(
                web::scope("/api/v1")
                    .route("/recommendations", web::post().to(get_recommendations))
                    .route("/recommendations/similar", web::post().to(get_similar_content))
                    .route("/personalization/score", web::post().to(get_personalization_score))
                    .route("/profile/update", web::post().to(update_profile))
                    .route("/lora/train", web::post().to(trigger_lora_training))
            )
    })
    .bind(("0.0.0.0", 8082))?
    .run()
    .await
}

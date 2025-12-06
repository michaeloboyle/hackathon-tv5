pub mod handlers;

pub use handlers::{
    delete_ranking_variant, get_analytics, get_quality_report, get_ranking_config,
    get_ranking_config_history, get_ranking_variant, list_ranking_variants, update_ranking_config,
    update_ranking_variant,
};

use actix_web::{web, HttpResponse, Responder};
use serde::Serialize;
use std::sync::Arc;

use crate::config::DiscoveryConfig;
use crate::search::{HybridSearchService, RankingConfigStore};

/// Application state shared across all handlers
pub struct AppState {
    pub config: Arc<DiscoveryConfig>,
    pub search_service: Arc<HybridSearchService>,
    pub ranking_store: Option<Arc<RankingConfigStore>>,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
    service: String,
    version: String,
}

/// Health check endpoint
async fn health() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse {
        status: "healthy".to_string(),
        service: "discovery-service".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Configure application routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api/v1")
            .route("/health", web::get().to(health)),
    );

    // Configure catalog routes
    crate::catalog::configure_routes(cfg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};

    #[actix_web::test]
    async fn test_health_endpoint() {
        let app = test::init_service(
            App::new()
                .configure(configure_routes)
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/v1/health")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }
}

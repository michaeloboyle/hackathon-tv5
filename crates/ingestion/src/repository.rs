//! Content repository for database persistence

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::normalizer::{AvailabilityInfo, CanonicalContent, ContentType, ImageSet};

/// Content repository trait for persistence operations
#[async_trait]
pub trait ContentRepository: Send + Sync {
    /// Upsert content (insert or update)
    async fn upsert(&self, content: &CanonicalContent) -> Result<Uuid>;

    /// Upsert batch of content items in a transaction
    async fn upsert_batch(&self, items: &[CanonicalContent]) -> Result<Vec<Uuid>>;

    /// Update only availability fields
    async fn update_availability(
        &self,
        content_id: Uuid,
        platform: &str,
        region: &str,
        available: bool,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()>;

    /// Find content expiring within duration
    async fn find_expiring_within(&self, duration: Duration) -> Result<Vec<ExpiringContent>>;
}

/// Content expiring soon
#[derive(Debug, Clone)]
pub struct ExpiringContent {
    pub content_id: Uuid,
    pub title: String,
    pub platform: String,
    pub region: String,
    pub expires_at: DateTime<Utc>,
}

/// PostgreSQL implementation of ContentRepository
pub struct PostgresContentRepository {
    pool: PgPool,
}

impl PostgresContentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Convert ContentType to string for database storage
    fn content_type_to_string(content_type: &ContentType) -> &'static str {
        match content_type {
            ContentType::Movie => "movie",
            ContentType::Series => "series",
            ContentType::Episode => "episode",
            ContentType::Short => "short",
            ContentType::Documentary => "documentary",
        }
    }

    /// Serialize ImageSet to JSONB value
    fn serialize_images(images: &ImageSet) -> serde_json::Value {
        json!({
            "poster_small": images.poster_small,
            "poster_medium": images.poster_medium,
            "poster_large": images.poster_large,
            "backdrop": images.backdrop,
        })
    }

    /// Serialize AvailabilityInfo to JSONB value
    fn serialize_availability(availability: &AvailabilityInfo) -> serde_json::Value {
        json!({
            "regions": availability.regions,
            "subscription_required": availability.subscription_required,
            "purchase_price": availability.purchase_price,
            "rental_price": availability.rental_price,
            "currency": availability.currency,
            "available_from": availability.available_from,
            "available_until": availability.available_until,
        })
    }

    /// Upsert a single content item within a transaction
    async fn upsert_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        content: &CanonicalContent,
    ) -> Result<Uuid> {
        // Extract external IDs for conflict detection
        let eidr_id = content.external_ids.get("eidr");
        let imdb_id = content.external_ids.get("imdb");
        let tmdb_id = content.external_ids.get("tmdb").and_then(|id| id.parse::<i32>().ok());
        let tvdb_id = content.external_ids.get("tvdb").and_then(|id| id.parse::<i32>().ok());
        let gracenote_id = content.external_ids.get("gracenote");

        // Serialize complex types to JSONB
        let external_ids_json = serde_json::to_value(&content.external_ids)
            .context("Failed to serialize external_ids")?;
        let genres_json = serde_json::to_value(&content.genres)
            .context("Failed to serialize genres")?;
        let images_json = Self::serialize_images(&content.images);
        let availability_json = Self::serialize_availability(&content.availability);

        let content_type_str = Self::content_type_to_string(&content.content_type);

        // Extract release date (convert from release_year if needed)
        let release_date = content.release_year.map(|year| {
            chrono::NaiveDate::from_ymd_opt(year, 1, 1)
                .map(|d| d.and_hms_opt(0, 0, 0))
                .flatten()
                .map(|dt| DateTime::<Utc>::from_utc(dt, Utc))
        }).flatten();

        // Try to find existing content by external IDs
        let existing_id = if let Some(eidr) = eidr_id {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT content_id FROM external_ids WHERE eidr_id = $1"
            )
            .bind(eidr)
            .fetch_optional(&mut **tx)
            .await?
        } else if let Some(imdb) = imdb_id {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT content_id FROM external_ids WHERE imdb_id = $1"
            )
            .bind(imdb)
            .fetch_optional(&mut **tx)
            .await?
        } else if let Some(year) = content.release_year {
            // Fallback: match by title + release year (within 1 year tolerance)
            sqlx::query_scalar::<_, Uuid>(
                r#"
                SELECT c.id
                FROM content c
                WHERE c.title = $1
                  AND c.content_type = $2
                  AND EXTRACT(YEAR FROM c.release_date) BETWEEN $3 AND $4
                LIMIT 1
                "#
            )
            .bind(&content.title)
            .bind(content_type_str)
            .bind(year - 1)
            .bind(year + 1)
            .fetch_optional(&mut **tx)
            .await?
        } else {
            None
        };

        let content_id = existing_id.unwrap_or_else(Uuid::new_v4);

        // Upsert main content record
        sqlx::query(
            r#"
            INSERT INTO content (
                id, content_type, title, original_title, overview, tagline,
                release_date, runtime_minutes, popularity_score, average_rating,
                vote_count, last_updated
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (id) DO UPDATE SET
                content_type = EXCLUDED.content_type,
                title = EXCLUDED.title,
                original_title = EXCLUDED.original_title,
                overview = EXCLUDED.overview,
                tagline = EXCLUDED.tagline,
                release_date = EXCLUDED.release_date,
                runtime_minutes = EXCLUDED.runtime_minutes,
                popularity_score = EXCLUDED.popularity_score,
                average_rating = EXCLUDED.average_rating,
                vote_count = EXCLUDED.vote_count,
                last_updated = EXCLUDED.last_updated
            "#
        )
        .bind(content_id)
        .bind(content_type_str)
        .bind(&content.title)
        .bind(&content.title) // Use title as original_title fallback
        .bind(&content.overview)
        .bind::<Option<String>>(None) // tagline not in CanonicalContent
        .bind(release_date)
        .bind(content.runtime_minutes)
        .bind(0.5) // Default popularity score
        .bind(content.user_rating.unwrap_or(0.0))
        .bind(0) // Default vote count
        .bind(content.updated_at)
        .execute(&mut **tx)
        .await
        .context("Failed to upsert content")?;

        // Upsert external IDs
        sqlx::query(
            r#"
            INSERT INTO external_ids (content_id, eidr_id, imdb_id, tmdb_id, tvdb_id, gracenote_tms_id)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (content_id) DO UPDATE SET
                eidr_id = COALESCE(EXCLUDED.eidr_id, external_ids.eidr_id),
                imdb_id = COALESCE(EXCLUDED.imdb_id, external_ids.imdb_id),
                tmdb_id = COALESCE(EXCLUDED.tmdb_id, external_ids.tmdb_id),
                tvdb_id = COALESCE(EXCLUDED.tvdb_id, external_ids.tvdb_id),
                gracenote_tms_id = COALESCE(EXCLUDED.gracenote_tms_id, external_ids.gracenote_tms_id)
            "#
        )
        .bind(content_id)
        .bind(eidr_id)
        .bind(imdb_id)
        .bind(tmdb_id)
        .bind(tvdb_id)
        .bind(gracenote_id)
        .execute(&mut **tx)
        .await
        .context("Failed to upsert external IDs")?;

        // Upsert platform ID
        sqlx::query(
            r#"
            INSERT INTO platform_ids (content_id, platform, platform_content_id)
            VALUES ($1, $2, $3)
            ON CONFLICT (content_id, platform) DO UPDATE SET
                platform_content_id = EXCLUDED.platform_content_id
            "#
        )
        .bind(content_id)
        .bind(&content.platform_id)
        .bind(&content.platform_content_id)
        .execute(&mut **tx)
        .await
        .context("Failed to upsert platform ID")?;

        // Delete and re-insert genres (simpler than complex upsert logic)
        sqlx::query("DELETE FROM content_genres WHERE content_id = $1")
            .bind(content_id)
            .execute(&mut **tx)
            .await?;

        for genre in &content.genres {
            sqlx::query(
                "INSERT INTO content_genres (content_id, genre) VALUES ($1, $2)"
            )
            .bind(content_id)
            .bind(genre)
            .execute(&mut **tx)
            .await?;
        }

        // Upsert content rating if available
        if let Some(rating) = &content.rating {
            for region in &content.availability.regions {
                sqlx::query(
                    r#"
                    INSERT INTO content_ratings (content_id, region, rating, advisory_notes)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (content_id, region) DO UPDATE SET
                        rating = EXCLUDED.rating,
                        advisory_notes = EXCLUDED.advisory_notes
                    "#
                )
                .bind(content_id)
                .bind(region)
                .bind(rating)
                .bind::<Option<String>>(None)
                .execute(&mut **tx)
                .await?;
            }
        }

        // Upsert platform availability
        for region in &content.availability.regions {
            let deep_link = format!("https://{}.com/watch/{}", content.platform_id, content.platform_content_id);
            let availability_type = if content.availability.subscription_required { "subscription" } else { "free" };

            // First, delete any existing availability records for this content/platform/region
            // to avoid conflicts with different availability types
            sqlx::query(
                "DELETE FROM platform_availability WHERE content_id = $1 AND platform = $2 AND region = $3"
            )
            .bind(content_id)
            .bind(&content.platform_id)
            .bind(region)
            .execute(&mut **tx)
            .await?;

            // Insert new availability record
            sqlx::query(
                r#"
                INSERT INTO platform_availability (
                    content_id, platform, region, availability_type,
                    price_cents, currency, deep_link, web_fallback,
                    available_from, expires_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                "#
            )
            .bind(content_id)
            .bind(&content.platform_id)
            .bind(region)
            .bind(availability_type)
            .bind(content.availability.purchase_price.map(|p| (p * 100.0) as i32))
            .bind(&content.availability.currency)
            .bind(&deep_link)
            .bind(&deep_link)
            .bind(content.availability.available_from.unwrap_or_else(Utc::now))
            .bind(content.availability.available_until)
            .execute(&mut **tx)
            .await?;
        }

        Ok(content_id)
    }
}

#[async_trait]
impl ContentRepository for PostgresContentRepository {
    async fn upsert(&self, content: &CanonicalContent) -> Result<Uuid> {
        let mut tx = self.pool.begin().await.context("Failed to begin transaction")?;
        let content_id = Self::upsert_in_transaction(&mut tx, content).await?;
        tx.commit().await.context("Failed to commit transaction")?;
        Ok(content_id)
    }

    async fn upsert_batch(&self, items: &[CanonicalContent]) -> Result<Vec<Uuid>> {
        const BATCH_SIZE: usize = 10;
        let mut all_ids = Vec::with_capacity(items.len());

        // Process items in batches of 10
        for chunk in items.chunks(BATCH_SIZE) {
            let mut tx = self.pool.begin().await.context("Failed to begin batch transaction")?;
            let mut batch_ids = Vec::with_capacity(chunk.len());

            // Process all items in this batch within the same transaction
            for content in chunk {
                let content_id = Self::upsert_in_transaction(&mut tx, content)
                    .await
                    .context("Failed to upsert content in batch")?;
                batch_ids.push(content_id);
            }

            // Commit this batch
            tx.commit().await.context("Failed to commit batch transaction")?;
            all_ids.extend(batch_ids);
        }

        Ok(all_ids)
    }

    async fn update_availability(
        &self,
        content_id: Uuid,
        platform: &str,
        region: &str,
        available: bool,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        // Stub implementation
        // TODO: Update availability based on content lookup
        // This requires querying content by platform_content_id and platform_id,
        // then updating the availability field in the CanonicalContent structure
        // Parameters: content_id, platform, region, available, expires_at

        Ok(())
    }

    async fn find_expiring_within(&self, duration: Duration) -> Result<Vec<ExpiringContent>> {
        // Stub implementation
        // TODO: Query database for content where:
        // content.availability.available_until is Some(date) AND
        // date is within the next 'duration' from now
        // Return ExpiringContent with: content_id, title, platform_id, region, expires_at

        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expiring_content_struct() {
        let expiring = ExpiringContent {
            content_id: Uuid::new_v4(),
            title: "Test Movie".to_string(),
            platform: "netflix".to_string(),
            region: "US".to_string(),
            expires_at: Utc::now(),
        };
        assert!(!expiring.title.is_empty());
    }
}

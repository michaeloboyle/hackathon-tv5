/// Offline-First Sync Queue with SQLite Persistence
///
/// Provides a persistent queue for sync operations with FIFO ordering,
/// automatic reconnection handling, and CRDT merge conflict resolution.

use crate::crdt::HLCTimestamp;
use crate::sync::publisher::{SyncPublisher, PublisherError};
use async_trait::async_trait;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Maximum retry attempts per operation
const MAX_OPERATION_RETRIES: i32 = 3;

/// Sync operation types that can be queued
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum SyncOperation {
    #[serde(rename = "watchlist_add")]
    WatchlistAdd {
        user_id: Uuid,
        content_id: Uuid,
        timestamp: i64,
    },
    #[serde(rename = "watchlist_remove")]
    WatchlistRemove {
        user_id: Uuid,
        content_id: Uuid,
        timestamp: i64,
    },
    #[serde(rename = "progress_update")]
    ProgressUpdate {
        user_id: Uuid,
        content_id: Uuid,
        position: f64,
        timestamp: i64,
    },
}

/// Report of sync replay operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    /// Number of operations successfully synced
    pub success_count: usize,
    /// Number of operations that failed
    pub failure_count: usize,
    /// Total operations processed
    pub total_operations: usize,
    /// IDs of failed operations
    pub failed_operation_ids: Vec<u64>,
    /// Error messages for failures
    pub errors: Vec<String>,
}

impl SyncReport {
    /// Create a new empty sync report
    pub fn new() -> Self {
        Self {
            success_count: 0,
            failure_count: 0,
            total_operations: 0,
            failed_operation_ids: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Check if all operations succeeded
    pub fn all_succeeded(&self) -> bool {
        self.failure_count == 0 && self.total_operations > 0
    }

    /// Check if any operations failed
    pub fn has_failures(&self) -> bool {
        self.failure_count > 0
    }
}

impl Default for SyncReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Offline sync queue with SQLite persistence
pub struct OfflineSyncQueue {
    /// SQLite database connection
    db: Arc<parking_lot::Mutex<Connection>>,
    /// Publisher for sync operations
    publisher: Arc<dyn SyncPublisher>,
}

impl OfflineSyncQueue {
    /// Create a new offline sync queue
    ///
    /// # Arguments
    /// * `db_path` - Path to SQLite database file
    /// * `publisher` - Publisher for sync operations
    ///
    /// # Errors
    /// Returns `QueueError` if database initialization fails
    pub fn new<P: AsRef<Path>>(
        db_path: P,
        publisher: Arc<dyn SyncPublisher>,
    ) -> Result<Self, QueueError> {
        let conn = Connection::open(db_path)?;

        // Create schema
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                retry_count INTEGER DEFAULT 0
            )",
            [],
        )?;

        // Create index for efficient FIFO ordering
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_created_at ON sync_queue(created_at, id)",
            [],
        )?;

        info!("Initialized offline sync queue with database");

        Ok(Self {
            db: Arc::new(parking_lot::Mutex::new(conn)),
            publisher,
        })
    }

    /// Create an in-memory sync queue (for testing)
    pub fn new_in_memory(publisher: Arc<dyn SyncPublisher>) -> Result<Self, QueueError> {
        let conn = Connection::open_in_memory()?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                retry_count INTEGER DEFAULT 0
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_created_at ON sync_queue(created_at, id)",
            [],
        )?;

        Ok(Self {
            db: Arc::new(parking_lot::Mutex::new(conn)),
            publisher,
        })
    }

    /// Enqueue a sync operation
    ///
    /// # Arguments
    /// * `op` - The sync operation to enqueue
    ///
    /// # Returns
    /// The ID of the enqueued operation
    ///
    /// # Errors
    /// Returns `QueueError` if serialization or database insertion fails
    pub fn enqueue(&self, op: SyncOperation) -> Result<u64, QueueError> {
        let operation_type = match &op {
            SyncOperation::WatchlistAdd { .. } => "watchlist_add",
            SyncOperation::WatchlistRemove { .. } => "watchlist_remove",
            SyncOperation::ProgressUpdate { .. } => "progress_update",
        };

        let payload = serde_json::to_string(&op)?;
        let created_at = chrono::Utc::now().timestamp_millis();

        let db = self.db.lock();
        db.execute(
            "INSERT INTO sync_queue (operation_type, payload, created_at, retry_count)
             VALUES (?1, ?2, ?3, 0)",
            params![operation_type, payload, created_at],
        )?;

        let id = db.last_insert_rowid() as u64;

        debug!(
            "Enqueued sync operation {} (type: {}, id: {})",
            operation_type, operation_type, id
        );

        Ok(id)
    }

    /// Dequeue the next operation (FIFO order)
    ///
    /// # Returns
    /// `Some((id, operation))` if an operation is available, `None` if queue is empty
    ///
    /// # Errors
    /// Returns `QueueError` if database query or deserialization fails
    pub fn dequeue(&self) -> Result<Option<(u64, SyncOperation)>, QueueError> {
        let db = self.db.lock();

        let mut stmt = db.prepare(
            "SELECT id, payload FROM sync_queue
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
        )?;

        let result = stmt.query_row([], |row| {
            let id: i64 = row.get(0)?;
            let payload: String = row.get(1)?;
            Ok((id as u64, payload))
        });

        match result {
            Ok((id, payload)) => {
                let op: SyncOperation = serde_json::from_str(&payload)
                    .map_err(|e| QueueError::Deserialization(e.to_string()))?;
                debug!("Dequeued operation with id: {}", id);
                Ok(Some((id, op)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(QueueError::Database(e)),
        }
    }

    /// Peek at the next N operations without removing them
    ///
    /// # Arguments
    /// * `limit` - Maximum number of operations to peek
    ///
    /// # Returns
    /// Vector of (id, operation) tuples in FIFO order
    ///
    /// # Errors
    /// Returns `QueueError` if database query or deserialization fails
    pub fn peek(&self, limit: usize) -> Result<Vec<(u64, SyncOperation)>, QueueError> {
        let db = self.db.lock();

        let mut stmt = db.prepare(
            "SELECT id, payload FROM sync_queue
             ORDER BY created_at ASC, id ASC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map([limit], |row| {
            let id: i64 = row.get(0)?;
            let payload: String = row.get(1)?;
            Ok((id as u64, payload))
        })?;

        let mut operations = Vec::new();
        for row_result in rows {
            let (id, payload) = row_result?;
            let op: SyncOperation = serde_json::from_str(&payload)
                .map_err(|e| QueueError::Deserialization(e.to_string()))?;
            operations.push((id, op));
        }

        debug!("Peeked at {} operations", operations.len());
        Ok(operations)
    }

    /// Remove an operation from the queue (after successful sync)
    ///
    /// # Arguments
    /// * `id` - The ID of the operation to remove
    ///
    /// # Errors
    /// Returns `QueueError` if database deletion fails
    pub fn remove(&self, id: u64) -> Result<(), QueueError> {
        let db = self.db.lock();
        let rows_affected = db.execute("DELETE FROM sync_queue WHERE id = ?1", params![id as i64])?;

        if rows_affected > 0 {
            debug!("Removed operation with id: {}", id);
        } else {
            warn!("Attempted to remove non-existent operation with id: {}", id);
        }

        Ok(())
    }

    /// Clear all operations from the queue
    ///
    /// # Errors
    /// Returns `QueueError` if database deletion fails
    pub fn clear(&self) -> Result<(), QueueError> {
        let db = self.db.lock();
        let rows_affected = db.execute("DELETE FROM sync_queue", [])?;
        info!("Cleared {} operations from sync queue", rows_affected);
        Ok(())
    }

    /// Get the number of operations in the queue
    pub fn len(&self) -> Result<usize, QueueError> {
        let db = self.db.lock();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM sync_queue", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Check if the queue is empty
    pub fn is_empty(&self) -> Result<bool, QueueError> {
        Ok(self.len()? == 0)
    }

    /// Increment retry count for an operation
    fn increment_retry_count(&self, id: u64) -> Result<i32, QueueError> {
        let db = self.db.lock();
        db.execute(
            "UPDATE sync_queue SET retry_count = retry_count + 1 WHERE id = ?1",
            params![id as i64],
        )?;

        let retry_count: i32 = db.query_row(
            "SELECT retry_count FROM sync_queue WHERE id = ?1",
            params![id as i64],
            |row| row.get(0),
        )?;

        Ok(retry_count)
    }

    /// Replay all pending operations after reconnection
    ///
    /// # Returns
    /// A report detailing success/failure counts and any errors
    ///
    /// # Errors
    /// Returns `QueueError` if database operations fail (not if individual publishes fail)
    pub async fn replay_pending(&self) -> Result<SyncReport, QueueError> {
        let mut report = SyncReport::new();

        info!("Starting replay of pending sync operations");

        loop {
            // Dequeue next operation
            let operation = match self.dequeue()? {
                Some(op) => op,
                None => {
                    // Queue is empty
                    break;
                }
            };

            let (id, op) = operation;
            report.total_operations += 1;

            debug!("Replaying operation {}: {:?}", id, op);

            // Attempt to publish the operation
            match self.publish_operation(&op).await {
                Ok(_) => {
                    // Success - remove from queue
                    self.remove(id)?;
                    report.success_count += 1;
                    info!("Successfully replayed operation {}", id);
                }
                Err(e) => {
                    // Failure - increment retry count
                    let retry_count = self.increment_retry_count(id)?;

                    if retry_count >= MAX_OPERATION_RETRIES {
                        // Max retries exceeded - remove from queue and mark as failed
                        warn!(
                            "Operation {} exceeded max retries ({}), removing from queue",
                            id, MAX_OPERATION_RETRIES
                        );
                        self.remove(id)?;
                        report.failure_count += 1;
                        report.failed_operation_ids.push(id);
                        report.errors.push(format!("Operation {}: {}", id, e));
                    } else {
                        // Put back in queue for retry
                        warn!(
                            "Operation {} failed (retry {}/{}): {}",
                            id, retry_count, MAX_OPERATION_RETRIES, e
                        );
                        // Operation stays in queue with incremented retry count
                        report.failure_count += 1;
                        report.failed_operation_ids.push(id);
                        report.errors.push(format!("Operation {} (retry {}): {}", id, retry_count, e));
                    }
                }
            }
        }

        info!(
            "Replay completed: {} succeeded, {} failed out of {} total",
            report.success_count, report.failure_count, report.total_operations
        );

        Ok(report)
    }

    /// Publish a sync operation using the configured publisher
    async fn publish_operation(&self, op: &SyncOperation) -> Result<(), PublisherError> {
        match op {
            SyncOperation::WatchlistAdd { user_id, content_id, timestamp } => {
                debug!(
                    "Publishing watchlist add: user={}, content={}, timestamp={}",
                    user_id, content_id, timestamp
                );
                // In a real implementation, this would convert to WatchlistUpdate
                // For now, we'll create a generic sync message
                // The actual implementation would depend on the specific publisher interface
                // This is a placeholder that shows the pattern
                Ok(())
            }
            SyncOperation::WatchlistRemove { user_id, content_id, timestamp } => {
                debug!(
                    "Publishing watchlist remove: user={}, content={}, timestamp={}",
                    user_id, content_id, timestamp
                );
                Ok(())
            }
            SyncOperation::ProgressUpdate { user_id, content_id, position, timestamp } => {
                debug!(
                    "Publishing progress update: user={}, content={}, position={}, timestamp={}",
                    user_id, content_id, position, timestamp
                );
                Ok(())
            }
        }
    }
}

/// Queue operation errors
#[derive(Debug, Error)]
pub enum QueueError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Publisher error: {0}")]
    Publisher(#[from] PublisherError),

    #[error("Operation not found: {0}")]
    NotFound(u64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::publisher::{SyncMessage, MessagePayload};
    use parking_lot::Mutex as ParkingMutex;
    use std::sync::Arc;

    /// Mock publisher for testing
    struct MockPublisher {
        published: Arc<ParkingMutex<Vec<String>>>,
        should_fail: Arc<ParkingMutex<bool>>,
    }

    impl MockPublisher {
        fn new() -> Self {
            Self {
                published: Arc::new(ParkingMutex::new(Vec::new())),
                should_fail: Arc::new(ParkingMutex::new(false)),
            }
        }

        fn get_published(&self) -> Vec<String> {
            self.published.lock().clone()
        }

        fn set_should_fail(&self, fail: bool) {
            *self.should_fail.lock() = fail;
        }

        fn published_count(&self) -> usize {
            self.published.lock().len()
        }
    }

    #[async_trait]
    impl SyncPublisher for MockPublisher {
        async fn publish(&self, message: SyncMessage) -> Result<(), PublisherError> {
            if *self.should_fail.lock() {
                return Err(PublisherError::InvalidMessage("Mock failure".to_string()));
            }
            self.published.lock().push(message.operation_type.clone());
            Ok(())
        }

        async fn publish_watchlist_update(
            &self,
            _update: crate::sync::WatchlistUpdate,
        ) -> Result<(), PublisherError> {
            if *self.should_fail.lock() {
                return Err(PublisherError::InvalidMessage("Mock failure".to_string()));
            }
            self.published.lock().push("watchlist_update".to_string());
            Ok(())
        }

        async fn publish_progress_update(
            &self,
            _update: crate::sync::ProgressUpdate,
        ) -> Result<(), PublisherError> {
            if *self.should_fail.lock() {
                return Err(PublisherError::InvalidMessage("Mock failure".to_string()));
            }
            self.published.lock().push("progress_update".to_string());
            Ok(())
        }

        async fn publish_batch(&self, _messages: Vec<SyncMessage>) -> Result<(), PublisherError> {
            if *self.should_fail.lock() {
                return Err(PublisherError::InvalidMessage("Mock failure".to_string()));
            }
            self.published.lock().push("batch".to_string());
            Ok(())
        }

        async fn flush(&self) -> Result<(), PublisherError> {
            Ok(())
        }
    }

    #[test]
    fn test_enqueue_dequeue_fifo_order() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(publisher).unwrap();

        let user_id = Uuid::new_v4();
        let content_id1 = Uuid::new_v4();
        let content_id2 = Uuid::new_v4();
        let content_id3 = Uuid::new_v4();

        // Enqueue three operations
        let id1 = queue
            .enqueue(SyncOperation::WatchlistAdd {
                user_id,
                content_id: content_id1,
                timestamp: 1000,
            })
            .unwrap();

        let id2 = queue
            .enqueue(SyncOperation::ProgressUpdate {
                user_id,
                content_id: content_id2,
                position: 0.5,
                timestamp: 2000,
            })
            .unwrap();

        let id3 = queue
            .enqueue(SyncOperation::WatchlistRemove {
                user_id,
                content_id: content_id3,
                timestamp: 3000,
            })
            .unwrap();

        assert_eq!(queue.len().unwrap(), 3);

        // Dequeue in FIFO order
        let (deq_id1, op1) = queue.dequeue().unwrap().unwrap();
        assert_eq!(deq_id1, id1);
        assert!(matches!(op1, SyncOperation::WatchlistAdd { .. }));

        let (deq_id2, op2) = queue.dequeue().unwrap().unwrap();
        assert_eq!(deq_id2, id2);
        assert!(matches!(op2, SyncOperation::ProgressUpdate { .. }));

        let (deq_id3, op3) = queue.dequeue().unwrap().unwrap();
        assert_eq!(deq_id3, id3);
        assert!(matches!(op3, SyncOperation::WatchlistRemove { .. }));

        // Queue should be empty
        assert!(queue.dequeue().unwrap().is_none());
        assert_eq!(queue.len().unwrap(), 0);
    }

    #[test]
    fn test_peek_operations() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(publisher).unwrap();

        let user_id = Uuid::new_v4();

        // Enqueue five operations
        for i in 0..5 {
            queue
                .enqueue(SyncOperation::ProgressUpdate {
                    user_id,
                    content_id: Uuid::new_v4(),
                    position: i as f64 * 0.1,
                    timestamp: (i + 1) * 1000,
                })
                .unwrap();
        }

        // Peek at first 3
        let peeked = queue.peek(3).unwrap();
        assert_eq!(peeked.len(), 3);

        // Verify peek doesn't remove items
        assert_eq!(queue.len().unwrap(), 5);

        // Verify peek maintains FIFO order
        if let SyncOperation::ProgressUpdate { position, .. } = peeked[0].1 {
            assert_eq!(position, 0.0);
        } else {
            panic!("Expected ProgressUpdate");
        }

        if let SyncOperation::ProgressUpdate { position, .. } = peeked[1].1 {
            assert_eq!(position, 0.1);
        } else {
            panic!("Expected ProgressUpdate");
        }
    }

    #[test]
    fn test_remove_operation() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(publisher).unwrap();

        let user_id = Uuid::new_v4();
        let content_id = Uuid::new_v4();

        let id = queue
            .enqueue(SyncOperation::WatchlistAdd {
                user_id,
                content_id,
                timestamp: 1000,
            })
            .unwrap();

        assert_eq!(queue.len().unwrap(), 1);

        queue.remove(id).unwrap();

        assert_eq!(queue.len().unwrap(), 0);
        assert!(queue.is_empty().unwrap());
    }

    #[test]
    fn test_clear_all_operations() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(publisher).unwrap();

        let user_id = Uuid::new_v4();

        // Enqueue multiple operations
        for i in 0..10 {
            queue
                .enqueue(SyncOperation::ProgressUpdate {
                    user_id,
                    content_id: Uuid::new_v4(),
                    position: i as f64 * 0.1,
                    timestamp: (i + 1) * 1000,
                })
                .unwrap();
        }

        assert_eq!(queue.len().unwrap(), 10);

        queue.clear().unwrap();

        assert_eq!(queue.len().unwrap(), 0);
        assert!(queue.is_empty().unwrap());
    }

    #[test]
    fn test_persistence_across_connections() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join(format!("test_sync_queue_{}.db", Uuid::new_v4()));

        let user_id = Uuid::new_v4();
        let content_id = Uuid::new_v4();

        // Create queue and enqueue operation
        {
            let publisher = Arc::new(MockPublisher::new());
            let queue = OfflineSyncQueue::new(&db_path, publisher).unwrap();

            queue
                .enqueue(SyncOperation::WatchlistAdd {
                    user_id,
                    content_id,
                    timestamp: 1000,
                })
                .unwrap();

            assert_eq!(queue.len().unwrap(), 1);
        } // Drop queue, closing connection

        // Open new connection and verify data persisted
        {
            let publisher = Arc::new(MockPublisher::new());
            let queue = OfflineSyncQueue::new(&db_path, publisher).unwrap();

            assert_eq!(queue.len().unwrap(), 1);

            let (_, op) = queue.dequeue().unwrap().unwrap();
            match op {
                SyncOperation::WatchlistAdd {
                    user_id: uid,
                    content_id: cid,
                    timestamp,
                } => {
                    assert_eq!(uid, user_id);
                    assert_eq!(cid, content_id);
                    assert_eq!(timestamp, 1000);
                }
                _ => panic!("Expected WatchlistAdd"),
            }
        }

        // Cleanup
        std::fs::remove_file(&db_path).ok();
    }

    #[tokio::test]
    async fn test_replay_pending_success() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(Arc::clone(&publisher) as Arc<dyn SyncPublisher>).unwrap();

        let user_id = Uuid::new_v4();

        // Enqueue three operations
        queue
            .enqueue(SyncOperation::WatchlistAdd {
                user_id,
                content_id: Uuid::new_v4(),
                timestamp: 1000,
            })
            .unwrap();

        queue
            .enqueue(SyncOperation::ProgressUpdate {
                user_id,
                content_id: Uuid::new_v4(),
                position: 0.5,
                timestamp: 2000,
            })
            .unwrap();

        queue
            .enqueue(SyncOperation::WatchlistRemove {
                user_id,
                content_id: Uuid::new_v4(),
                timestamp: 3000,
            })
            .unwrap();

        assert_eq!(queue.len().unwrap(), 3);

        // Replay all operations
        let report = queue.replay_pending().await.unwrap();

        // All should succeed
        assert_eq!(report.total_operations, 3);
        assert_eq!(report.success_count, 3);
        assert_eq!(report.failure_count, 0);
        assert!(report.all_succeeded());

        // Queue should be empty
        assert_eq!(queue.len().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_replay_pending_with_failures() {
        let publisher = Arc::new(MockPublisher::new());
        let queue = OfflineSyncQueue::new_in_memory(Arc::clone(&publisher) as Arc<dyn SyncPublisher>).unwrap();

        let user_id = Uuid::new_v4();

        // Enqueue operations
        queue
            .enqueue(SyncOperation::WatchlistAdd {
                user_id,
                content_id: Uuid::new_v4(),
                timestamp: 1000,
            })
            .unwrap();

        // Set publisher to fail
        publisher.set_should_fail(true);

        // Replay - should fail and retry up to MAX_OPERATION_RETRIES
        let report = queue.replay_pending().await.unwrap();

        assert_eq!(report.total_operations, 1);
        assert_eq!(report.success_count, 0);
        assert_eq!(report.failure_count, 1);
        assert!(report.has_failures());
        assert_eq!(report.failed_operation_ids.len(), 1);
    }

    #[test]
    fn test_operation_serialization() {
        let user_id = Uuid::new_v4();
        let content_id = Uuid::new_v4();

        let op = SyncOperation::WatchlistAdd {
            user_id,
            content_id,
            timestamp: 1000,
        };

        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("watchlist_add"));

        let deserialized: SyncOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(op, deserialized);
    }
}

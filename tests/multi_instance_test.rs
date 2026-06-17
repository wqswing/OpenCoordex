use multi_agent_controller::chrono_timestamp;
use multi_agent_controller::ReActController;
use multi_agent_core::traits::{Controller, DistributedRateLimiter, SessionStore};
use multi_agent_core::types::{HistoryEntry, Session, SessionStatus, TaskState, TokenUsage};
use multi_agent_store::{RedisRateLimiter, RedisSessionStore};
use std::sync::Arc;
use std::time::Duration;

// Helper to check if Redis is available.
// If not, we skip the test to avoid fail noise in environments without Redis.
async fn is_redis_available(url: &str) -> bool {
    let client = match redis::Client::open(url) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Try to get a connection
    client.get_multiplexed_async_connection().await.is_ok()
}

#[tokio::test]
async fn test_multi_instance_session_handoff() -> anyhow::Result<()> {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    if !is_redis_available(&redis_url).await {
        println!(
            "Skipping test_multi_instance_session_handoff: Redis not available at {}",
            redis_url
        );
        return Ok(());
    }

    println!(
        "Running test_multi_instance_session_handoff with Redis at {}",
        redis_url
    );

    // 1. Setup shared shared store
    // Use a random prefix to avoid collisions
    let prefix = format!("test_handoff_{}:", uuid::Uuid::new_v4());
    // TTL 24h = 86400 seconds
    let session_store = Arc::new(RedisSessionStore::new(&redis_url, &prefix, 86400)?);

    // 2. Simulate Instance A
    let _controller_a = ReActController::builder()
        .with_session_store(session_store.clone())
        .build();

    // 3. Create initial session in Instance A context
    let session_id = format!("session_handoff_{}", uuid::Uuid::new_v4());
    let mut session = Session {
        id: session_id.clone(),
        trace_id: format!("trace-{}", session_id),
        user_id: None,
        status: SessionStatus::Running,
        history: vec![
            HistoryEntry {
                role: "system".to_string(),
                content: Arc::new("System prompt".to_string()),
                tool_call: None,
                timestamp: chrono_timestamp(),
            },
            HistoryEntry {
                role: "user".to_string(),
                content: Arc::new("Do a multi-step task".to_string()),
                tool_call: None,
                timestamp: chrono_timestamp(),
            },
        ],
        task_state: Some(TaskState {
            iteration: 0,
            goal: "Do a multi-step task".to_string(),
            observations: vec![],
            pending_actions: vec![],
            consecutive_rejections: 0,
        }),
        token_usage: TokenUsage::default(),
        created_at: chrono_timestamp(),
        updated_at: chrono_timestamp(),
    };

    // Save initial state (simulating A starting the work)
    session_store.save(&session).await?;

    // 4. Simulate Instance A "crashing" or stopping after some work
    // We update the session state to reflect progress
    if let Some(ref mut task_state) = session.task_state {
        task_state.iteration = 1;
        task_state
            .observations
            .push(Arc::new("Observation from Instance A".to_string()));
    }
    session_store.save(&session).await?;

    // 5. Simulate Instance B picking it up
    let controller_b = ReActController::builder()
        .with_session_store(session_store.clone())
        // In a real test, we'd mock the LLM for B to finish the task
        .build(); // Using mock LLM (default)

    // 6. Resume on Instance B
    let result = controller_b.resume(&session_id, None).await?;

    // 7. Verify Instance B finished the task (Mock LLM finishes immediately with "Mock ReAct execution...")
    match result {
        multi_agent_core::types::AgentResult::Text(text) => {
            // The mock LLM should respond
            assert!(text.contains("Mock ReAct execution"));
        }
        _ => panic!("Expected text result"),
    }

    // 8. Verify final state in store
    let final_session = session_store.load(&session_id).await?.unwrap();
    assert_eq!(final_session.status, SessionStatus::Completed);
    // Should have history from A and internal steps from B?
    // Since B connects with Mock LLM, it sees the history and returns a response.

    Ok(())
}

#[tokio::test]
async fn test_distributed_rate_limit_persistence() -> anyhow::Result<()> {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    if !is_redis_available(&redis_url).await {
        println!("Skipping test_distributed_rate_limit_persistence: Redis not available");
        return Ok(());
    }

    println!(
        "Running test_distributed_rate_limit_persistence with Redis at {}",
        redis_url
    );

    // Create Limiter A and Limiter B (simulating two pods)
    let limiter_a = RedisRateLimiter::new(&redis_url)?;
    let limiter_b = RedisRateLimiter::new(&redis_url)?;

    let key = format!("limit_test_{}", uuid::Uuid::new_v4());
    let limit = 5;
    let window = Duration::from_secs(60);

    // Consume 3 tokens on A
    for _ in 0..3 {
        let allowed = limiter_a.check_and_increment(&key, limit, window).await?;
        assert!(allowed, "Should allow first 3 requests");
    }

    // Consume 2 tokens on B (should hit shared limit)
    let allowed_b1 = limiter_b.check_and_increment(&key, limit, window).await?;
    assert!(allowed_b1, "Should allow 4th request (on B)");

    let allowed_b2 = limiter_b.check_and_increment(&key, limit, window).await?;
    assert!(allowed_b2, "Should allow 5th request (on B)");

    // Next request on A or B should block
    let allowed_a_last = limiter_a.check_and_increment(&key, limit, window).await?;
    assert!(
        !allowed_a_last,
        "Should block 6th request (shared limit exceeded)"
    );

    let allowed_b_last = limiter_b.check_and_increment(&key, limit, window).await?;
    assert!(
        !allowed_b_last,
        "Should block 7th request (shared limit exceeded)"
    );

    Ok(())
}

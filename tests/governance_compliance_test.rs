use multi_agent_core::{
    traits::ApprovalGate,
    types::{ApprovalRequest, ApprovalResponse, ToolRiskLevel},
};
use multi_agent_governance::{
    approval::ChannelApprovalGate,
    audit::{AuditEntry, AuditOutcome, AuditStore, SqliteAuditStore},
};
use std::sync::Arc;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_role_based_approval_gate_clearance() {
    let gate = ChannelApprovalGate::new(ToolRiskLevel::High)
        .with_timeout(std::time::Duration::from_secs(5));

    let req = ApprovalRequest {
        request_id: "test-compliance-req-1".into(),
        session_id: "session-123".into(),
        tool_name: "sandbox_shell".into(),
        args: serde_json::json!({"command": "rm -rf /"}),
        risk_level: ToolRiskLevel::Critical, // Critical risk tool
        context: "Executing command".into(),
        timeout_secs: None,
        nonce: "test-nonce-123".into(),
        expires_at: 0,
        agent_id: "agent-x".into(),
        agent_type: "Coder".into(),
        system_prompt_hash: "hash-fingerprint".into(),
        model_name: "gpt-4o".into(),
    };

    // 1. Spawn request approval task
    let gate_clone = Arc::new(gate);
    let gate_for_task = gate_clone.clone();
    let req_clone = req.clone();
    let handle = tokio::spawn(async move { gate_for_task.request_approval(&req_clone).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 2. Submit approval with insufficient role (Operator cannot approve Critical tool)
    let res_operator = gate_clone
        .submit_response(
            "test-compliance-req-1",
            "test-nonce-123",
            "user_operator".to_string(),
            vec!["operator".to_string()],
            ApprovalResponse::Approved {
                reason: None,
                reason_code: "USER_APPROVED".into(),
                approver_id: None,
                approver_role: None,
            },
        )
        .await;

    assert!(
        res_operator.is_err(),
        "Operator should not be allowed to approve Critical tools"
    );
    assert!(res_operator.unwrap_err().contains("Insufficient clearance"));

    // 3. Submit approval with sufficient role (Admin can approve Critical tool)
    let res_admin = gate_clone
        .submit_response(
            "test-compliance-req-1",
            "test-nonce-123",
            "user_admin".to_string(),
            vec!["admin".to_string()],
            ApprovalResponse::Approved {
                reason: None,
                reason_code: "USER_APPROVED".into(),
                approver_id: None,
                approver_role: None,
            },
        )
        .await;

    assert!(
        res_admin.is_ok(),
        "Admin should be allowed to approve Critical tools"
    );

    let response = handle.await.unwrap().unwrap();
    match response {
        ApprovalResponse::Approved {
            approver_id,
            approver_role,
            ..
        } => {
            assert_eq!(approver_id.as_deref(), Some("user_admin"));
            assert_eq!(approver_role.as_deref(), Some("admin"));
        }
        _ => panic!("Expected Approved"),
    }
}

#[tokio::test]
async fn test_sqlite_audit_log_tamper_detection() {
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path();
    let store = SqliteAuditStore::new(db_path).unwrap();

    // 1. Log several entries
    let entry1 = AuditEntry {
        id: "log-1".into(),
        timestamp: "2026-06-17T07:00:00Z".into(),
        user_id: "user-1".into(),
        action: "EXECUTE_TOOL".into(),
        resource: "sandbox_shell".into(),
        outcome: AuditOutcome::Success,
        metadata: Some(serde_json::json!({
            "agent_id": "agent-1",
            "system_prompt_hash": "hash-abc"
        })),
        previous_hash: None,
        hash: None,
    };

    let entry2 = AuditEntry {
        id: "log-2".into(),
        timestamp: "2026-06-17T07:01:00Z".into(),
        user_id: "user-2".into(),
        action: "EXECUTE_TOOL".into(),
        resource: "sandbox_shell".into(),
        outcome: AuditOutcome::Success,
        metadata: Some(serde_json::json!({
            "agent_id": "agent-2",
            "system_prompt_hash": "hash-def"
        })),
        previous_hash: None,
        hash: None,
    };

    store.log(entry1).await.unwrap();
    store.log(entry2).await.unwrap();

    // 2. Verify integrity - should be OK initially
    let is_ok = store.verify_integrity().await.unwrap();
    assert!(is_ok, "Audit log chain should be valid initially");

    // 3. Tamper with the database by updating a record directly via a raw SQLite connection
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "UPDATE audit_logs SET resource = 'tampered_resource' WHERE id = 'log-1'",
        [],
    )
    .unwrap();

    // 4. Verify integrity again - should detect tampering
    let is_ok_tampered = store.verify_integrity().await.unwrap();
    assert!(
        !is_ok_tampered,
        "Audit log chain validation should fail after database tampering"
    );
}

#[tokio::test]
async fn test_sqlite_audit_log_gdpr_redaction_and_rechaining() {
    use multi_agent_core::traits::Erasable;

    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path();
    let store = SqliteAuditStore::new(db_path).unwrap();

    // 1. Log three entries
    let entry1 = AuditEntry {
        id: "log-1".into(),
        timestamp: "2026-06-17T07:00:00Z".into(),
        user_id: "user-1".into(),
        action: "EXECUTE_TOOL".into(),
        resource: "sandbox_shell".into(),
        outcome: AuditOutcome::Success,
        metadata: Some(serde_json::json!({
            "agent_id": "agent-1",
            "system_prompt_hash": "hash-abc"
        })),
        previous_hash: None,
        hash: None,
    };

    let entry2 = AuditEntry {
        id: "log-2".into(),
        timestamp: "2026-06-17T07:01:00Z".into(),
        user_id: "user-2".into(),
        action: "EXECUTE_TOOL".into(),
        resource: "sandbox_shell".into(),
        outcome: AuditOutcome::Success,
        metadata: Some(serde_json::json!({
            "agent_id": "agent-2",
            "system_prompt_hash": "hash-def"
        })),
        previous_hash: None,
        hash: None,
    };

    let entry3 = AuditEntry {
        id: "log-3".into(),
        timestamp: "2026-06-17T07:02:00Z".into(),
        user_id: "user-1".into(),
        action: "EXECUTE_TOOL".into(),
        resource: "sandbox_shell".into(),
        outcome: AuditOutcome::Success,
        metadata: Some(serde_json::json!({
            "agent_id": "agent-1",
            "system_prompt_hash": "hash-abc"
        })),
        previous_hash: None,
        hash: None,
    };

    store.log(entry1).await.unwrap();
    store.log(entry2).await.unwrap();
    store.log(entry3).await.unwrap();

    // 2. Verify initial integrity
    let is_ok = store.verify_integrity().await.unwrap();
    assert!(is_ok, "Audit log chain should be valid initially");

    // 3. Perform GDPR erasure of user-1
    let redacted = store.erase_user("user-1").await.unwrap();
    assert_eq!(
        redacted, 2,
        "Should have redacted 2 entries belonging to user-1"
    );

    // 4. Verify that records have been updated to "REDACTED"
    let results = store
        .query(multi_agent_governance::audit::AuditFilter::default())
        .await
        .unwrap();
    assert_eq!(results.len(), 3);

    // results is ordered by timestamp DESC: [log-3, log-2, log-1]
    assert_eq!(results[0].id, "log-3");
    assert_eq!(results[0].user_id, "REDACTED");
    assert_eq!(results[0].metadata, None);

    assert_eq!(results[1].id, "log-2");
    assert_eq!(results[1].user_id, "user-2");
    assert!(results[1].metadata.is_some());

    assert_eq!(results[2].id, "log-1");
    assert_eq!(results[2].user_id, "REDACTED");
    assert_eq!(results[2].metadata, None);

    // 5. Verify integrity remains valid after redaction and re-chaining
    let is_ok_after_redaction = store.verify_integrity().await.unwrap();
    assert!(
        is_ok_after_redaction,
        "Audit log chain should still be valid after redaction due to re-chaining hash recalculation"
    );

    // 6. Tamper with a redacted row directly
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "UPDATE audit_logs SET action = 'MALICIOUS_ACTION' WHERE id = 'log-1'",
        [],
    )
    .unwrap();

    // 7. Verify integrity fails after tampering
    let is_ok_tampered = store.verify_integrity().await.unwrap();
    assert!(
        !is_ok_tampered,
        "Audit log chain should fail integrity check after tampering with a redacted row"
    );
}
